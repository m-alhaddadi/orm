"""`qs.explain()` and `db.fetch(sql, *params)` on Postgres and SQLite."""
import datetime
import decimal
import uuid

import pytest
import orm
from blog.models import Post, User

SOURCE = '''datasource db {
  provider = "sqlite"
}
model Person {
  id   Int    @id
  name String @unique
}
'''


@pytest.fixture
async def people():
    registry = orm.Registry()
    Person = orm.loads(SOURCE, registry=registry)["Person"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    await Person.objects.using(db).insert_many([{"id": i, "name": f"p{i}"} for i in range(3)])
    yield Person, db
    await db.close()


async def test_explain_on_postgres(clean):
    user = await User.objects.insert(email="e@example.com", name="E")
    qs = Post.objects.filter(Post.author_id == user.id).order_by(Post.id)
    plan = await qs.explain()
    assert "Scan" in plan and "actual time" not in plan
    analyzed = await qs.explain(analyze=True)
    assert "actual time" in analyzed and "Execution Time" in analyzed
    events: list[orm.QueryEvent] = []
    remove = orm.get_database().on_query(events.append)
    try:
        await User.objects.filter(User.id == user.id).explain()
    finally:
        remove()
    assert [e.sql.startswith("EXPLAIN SELECT") and "$1" in e.sql for e in events] == [True]
    with pytest.raises(orm.QueryError, match="row locks"):
        await Post.objects.lock().explain(analyze=True)


async def test_explain_on_sqlite(people):
    Person, db = people
    plan = await Person.objects.using(db).filter(Person.name == "p1").explain()
    assert "SEARCH" in plan and "person" in plan
    scan = await Person.objects.using(db).filter(Person.name.contains("p")).order_by(Person.name).explain()
    assert "SCAN" in scan
    with pytest.raises(orm.QueryError, match="ANALYZE"):
        await Person.objects.using(db).explain(analyze=True)


async def test_fetch_on_postgres(clean):
    db = orm.get_database()
    user = await User.objects.insert(email="f@example.com", name="F")
    rows = await db.fetch("SELECT id, email FROM users WHERE id = $1 AND name = $2", user.id, "F")
    assert rows == [{"id": user.id, "email": "f@example.com"}]
    u = uuid.uuid4()
    when = datetime.datetime(2026, 1, 2, 3, 4, 5, tzinfo=datetime.timezone.utc)
    [row] = await db.fetch(
        "SELECT $1::uuid AS u, $2::timestamptz AS t, $3::numeric AS d, $4::jsonb AS j, 2::int2 AS small, "
        "1.5::float4 AS f, ARRAY[1, 2] AS a, NULL::text AS n, true AS b, DATE '2026-01-02' AS day",
        u, when, decimal.Decimal("1.50"), {"k": [1]},
    )
    assert row == {
        "u": u, "t": when, "d": decimal.Decimal("1.50"), "j": {"k": [1]}, "small": 2, "f": 1.5,
        "a": [1, 2], "n": None, "b": True, "day": datetime.date(2026, 1, 2),
    }
    async with db.transaction():
        await db.execute("UPDATE users SET name = 'G'")
        assert await db.fetch("SELECT name FROM users") == [{"name": "G"}]
    with pytest.raises(orm.DatabaseError, match="interval"):
        await db.fetch("SELECT interval '1 day' AS i")
    assert await db.fetch("SELECT id FROM users WHERE false") == []


async def test_fetch_on_sqlite(people):
    _, db = people
    rows = await db.fetch("SELECT id, name, id * 0.5 AS half FROM person WHERE id >= ? ORDER BY id", 1)
    assert rows == [{"id": 1, "name": "p1", "half": 0.5}, {"id": 2, "name": "p2", "half": 1.0}]
    events: list[orm.QueryEvent] = []
    db.on_query(events.append)
    assert await db.fetch("SELECT count(*) AS n FROM person") == [{"n": 3}]
    assert [(e.sql, e.rows) for e in events] == [("SELECT count(*) AS n FROM person", 1)]
