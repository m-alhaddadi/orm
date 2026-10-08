"""`db.on_query` hooks, `orm.otel` spans, and the N+1 finder on the same events."""
import contextvars
import sys
import types

import pytest
import orm
from orm import debug
from blog.models import Comment, Post, User

SOURCE = '''datasource db {
  provider = "sqlite"
}
model Person {
  id        Int        @id
  name      String     @unique
  customers Customer[]
}
model Customer {
  id        Int    @id
  person_id Int
  person    Person @relation(fields: [person_id], references: [id])
}
'''


@pytest.fixture
async def shop():
    registry = orm.Registry()
    models = orm.loads(SOURCE, registry=registry)
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    Person, Customer = models["Person"], models["Customer"]
    await Person.objects.using(db).insert_many([{"id": i, "name": f"p{i}"} for i in range(3)])
    await Customer.objects.using(db).insert_many([{"id": i, "person_id": i % 2} for i in range(4)])
    yield Person, Customer, db
    await db.close()


async def test_a_hook_gets_each_statement_with_its_shape_time_and_rows(shop):
    Person, Customer, db = shop
    events: list[orm.QueryEvent] = []
    remove = db.on_query(events.append)
    people = await Person.objects.using(db).prefetch_related(Person.customers).order_by(Person.id)
    assert [len(p.customers.cached) for p in people] == [2, 2, 0]
    select, prefetch = events
    assert select.sql.startswith("SELECT") and '"person"' in select.sql and select.rows == 3
    assert '"customer"' in prefetch.sql and "?" in prefetch.sql and prefetch.rows == 4
    assert select.error is None and 0 <= select.duration < 5 and select.start <= prefetch.start

    events.clear()
    assert await Customer.objects.using(db).update_many([{"id": i, "person_id": 2} for i in range(3)], batch_size=1) == 3
    assert [e.rows for e in events] == [1, 1, 1] and all(e.sql.startswith("UPDATE") for e in events)

    events.clear()
    assert await db.execute("DELETE FROM customer WHERE id = 3") == 1
    assert [(e.sql, e.rows) for e in events] == [("DELETE FROM customer WHERE id = 3", 1)]

    remove()
    events.clear()
    await Person.objects.using(db).count()
    assert events == []


async def test_a_failed_statement_gives_an_event_with_the_error(shop):
    Person, _, db = shop
    events: list[orm.QueryEvent] = []
    db.on_query(events.append)
    with pytest.raises(orm.IntegrityError):
        await Person.objects.using(db).insert(id=9, name="p0")
    [e] = events
    assert e.sql.startswith("INSERT INTO") and e.rows == 0 and e.error is not None and "UNIQUE" in e.error


async def test_hooks_run_in_the_callers_context_and_see_prepared_queries(shop):
    Person, _, db = shop
    request = contextvars.ContextVar("request", default="-")
    seen: list[tuple[str, int]] = []
    db.on_query(lambda e: seen.append((request.get(), e.rows)))
    by_name = Person.objects.using(db).filter(Person.name == orm.param("name")).prepare()
    request.set("r1")
    assert (await by_name.get(name="p1")).id == 1
    async with db.transaction():
        request.set("r2")
        await Person.objects.using(db).filter(Person.id == 1).update(name="x")
    assert seen == [("r1", 1), ("r2", 1)]


async def test_a_hook_error_reaches_the_caller(shop):
    Person, _, db = shop

    def broken(e: orm.QueryEvent) -> None:
        raise RuntimeError("exporter down")

    db.on_query(broken)
    with pytest.raises(RuntimeError, match="exporter down"):
        await Person.objects.using(db).count()


class FakeSpan:
    def __init__(self, name, kind, attributes, start_time):
        self.name, self.kind, self.attributes, self.start = name, kind, attributes, start_time
        self.status = None
        self.end_time = None

    def set_status(self, code, description=None):
        self.status = (code, description)

    def end(self, end_time=None):
        self.end_time = end_time


class FakeTracer:
    def __init__(self):
        self.spans: list[FakeSpan] = []

    def start_span(self, name, *, kind=None, attributes=None, start_time=None):
        self.spans.append(FakeSpan(name, kind, attributes, start_time))
        return self.spans[-1]


@pytest.fixture
def opentelemetry(monkeypatch):
    """A stand-in for the `opentelemetry.trace` module: the suite runs without the package."""
    trace = types.ModuleType("opentelemetry.trace")
    trace.SpanKind = types.SimpleNamespace(CLIENT="client")
    trace.StatusCode = types.SimpleNamespace(ERROR="error")
    trace.tracer = FakeTracer()
    trace.get_tracer = lambda name: trace.tracer
    package = types.ModuleType("opentelemetry")
    package.trace = trace
    monkeypatch.setitem(sys.modules, "opentelemetry", package)
    monkeypatch.setitem(sys.modules, "opentelemetry.trace", trace)
    return trace


async def test_otel_gives_a_client_span_per_statement(shop, opentelemetry):
    from orm import otel

    Person, _, db = shop
    stop = otel.instrument(db)
    await Person.objects.using(db).filter(Person.id == 1)
    with pytest.raises(orm.IntegrityError):
        await Person.objects.using(db).insert(id=9, name="p0")
    ok, failed = opentelemetry.tracer.spans
    assert (ok.name, ok.kind, ok.status) == ("SELECT", "client", None)
    assert ok.attributes["db.system.name"] == "sqlite" and ok.attributes["db.response.returned_rows"] == 1
    assert ok.attributes["db.query.text"].startswith("SELECT") and "?" in ok.attributes["db.query.text"]
    assert ok.start > 1_600_000_000 * 10**9 and ok.end_time >= ok.start
    assert failed.name == "INSERT" and failed.status[0] == "error" and "UNIQUE" in failed.status[1]
    stop()
    await Person.objects.using(db).count()
    assert len(opentelemetry.tracer.spans) == 2

    mine = FakeTracer()
    otel.instrument(db, tracer=mine)
    await Person.objects.using(db).count()
    assert [s.name for s in mine.spans] == ["SELECT"]


def test_otel_without_the_package_names_it(monkeypatch):
    from orm import otel

    monkeypatch.setitem(sys.modules, "opentelemetry", None)
    with pytest.raises(ImportError, match="opentelemetry-api"):
        otel.instrument(object())  # type: ignore[arg-type]


async def test_the_n_plus_one_finder_counts_the_traced_sql(shop):
    Person, Customer, db = shop
    with pytest.warns(debug.NPlusOneWarning), debug.n_plus_one(threshold=2) as report:
        for i in range(3):
            await Person.objects.using(db).insert(id=10 + i, name=f"n{i}")
    [shape] = report.repeated
    assert shape.count == 3 and shape.sql.startswith('INSERT INTO "person"') and "?" in shape.sql
    assert shape.site.endswith("test_query_hooks.py:173")


async def test_postgres_events_and_row_counts(clean):
    db = orm.get_database()
    user = await User.objects.insert(email="hook@example.com", name="H")
    post = await Post.objects.insert(author_id=user.id, title="t", body="b")
    await Comment.objects.insert_many([{"post_id": post.id, "body": str(i)} for i in range(3)])
    events: list[orm.QueryEvent] = []
    remove = db.on_query(events.append)
    try:
        await Post.objects.prefetch_related(Post.comments).filter(Post.id == post.id)
        assert await Comment.objects.filter(Comment.post_id == post.id).update(body="x") == 3
    finally:
        remove()
    assert [e.rows for e in events] == [1, 3, 3]
    assert events[0].sql.endswith('WHERE "posts"."id" = $1')
