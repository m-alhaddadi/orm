"""Cursor pagination on both supported databases."""

import base64
import json
import math
import os
import re
from datetime import datetime, timedelta, timezone

import pytest

import orm
from orm import QueryError, _native, pagination

SCHEMA = """
datasource db {
  provider = "sqlite"
}
model Owner {
  id BigInt @id @default(autoincrement())
  name String
  items Item[]
  @@map("page07_owners")
}
model Item {
  id BigInt @id @default(autoincrement())
  owner_id BigInt
  score Int
  rank Int?
  name String
  code String @unique
  at DateTime
  tags Json?
  owner Owner @relation(fields: [owner_id], references: [id], onDelete: Cascade)
  @@map("page07_items")
}
"""
T0 = datetime(2026, 10, 1, tzinfo=timezone.utc)


@pytest.fixture(params=["sqlite", "postgres"])
async def items(request):
    registry = orm.Registry()
    provider = "postgresql" if request.param == "postgres" else "sqlite"
    models = orm.loads(SCHEMA.replace('"sqlite"', f'"{provider}"'), registry=registry)
    Owner, Item = models["Owner"], models["Item"]
    url = "sqlite://:memory:" if request.param == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    owner = await Owner.objects.using(db).insert(name="o")
    rows = [
        {"owner_id": owner.pk, "score": i % 4, "rank": None if i % 3 == 0 else i % 5, "name": f"n{i % 3}", "code": f"c{i:02}", "at": T0 + timedelta(microseconds=i * 7)}
        for i in range(23)
    ]
    await Item.objects.using(db).insert_many(rows)
    try:
        yield Item, Item.objects.using(db)
    finally:
        await db.drop_tables()
        await db.close()


async def walk(qs, size):
    """Every page forward, then every page back from the end."""
    pages, cursor = [], None
    while True:
        page = await qs.paginate(first=size, after=cursor)
        pages.append(page)
        if not page.has_next:
            break
        cursor = page.next_cursor
    forward = [x.pk for p in pages for x in p.items]
    assert [p.has_previous for p in pages] == [False] + [True] * (len(pages) - 1)
    back, cursor = [], None
    while True:
        page = await qs.paginate(last=size, before=cursor)
        back = [x.pk for x in page.items] + back
        if not page.has_previous:
            break
        cursor = page.previous_cursor
    assert back == forward
    return forward


@pytest.mark.parametrize(
    "order",
    [
        lambda I: (),
        lambda I: (-I.score,),
        lambda I: (I.score.desc(), I.name),
        lambda I: (I.name, -I.at),
        lambda I: (I.rank.asc(nulls="last"), I.score),
        lambda I: (I.rank.desc(nulls="first"),),
        lambda I: (I.rank.desc(nulls="last"), -I.code),
    ],
)
async def test_pages_follow_the_order(items, order):
    Item, qs = items
    keys = order(Item)
    ordered = qs.order_by(*keys) if keys else qs
    expected = [x.pk for x in await qs.order_by(*keys, Item.id)]
    for size in (1, 5, 23, 40):
        assert await walk(ordered, size) == expected


async def test_page_shape_and_related_rows(items):
    Item, qs = items
    page = await qs.load(Item.owner).load(Item.name).order_by(-Item.at).paginate(first=5)
    assert len(page.items) == 5 and page.has_next and not page.has_previous
    assert page.items[0].owner.name == "o"
    with pytest.raises(orm.NotLoaded):
        page.items[0].at
    rest = await qs.load(Item.name).order_by(-Item.at).paginate(first=100, after=page.next_cursor)
    assert len(rest.items) == 18 and not rest.has_next and rest.has_previous
    empty = await qs.order_by(-Item.at).paginate(first=5, after=rest.next_cursor)
    assert empty.items == [] and empty.next_cursor is None and empty.previous_cursor is None
    assert "has_next=False" in repr(empty)


async def test_rows_added_between_pages_are_not_repeated(items):
    Item, qs = items
    page = await qs.order_by(-Item.score).paginate(first=10)
    await qs.insert(owner_id=page.items[0].owner_id, score=99, name="new", code="new", at=T0)
    after = await qs.order_by(-Item.score).paginate(first=100, after=page.next_cursor)
    seen = {x.pk for x in page.items}
    assert not seen & {x.pk for x in after.items} and len(after.items) == 13


async def test_rejected_orders_and_cursors(items):
    Item, qs = items
    with pytest.raises(QueryError, match=r"Item.rank is nullable: order by Item.rank.asc\(nulls='first'\)"):
        await qs.order_by(Item.rank).paginate(first=2)
    with pytest.raises(QueryError, match="orders by columns of Item itself"):
        await qs.order_by(Item.owner.name).paginate(first=2)
    with pytest.raises(QueryError, match="orders by columns of Item itself"):
        await qs.order_by(Item.score + 1).paginate(first=2)
    with pytest.raises(QueryError, match="sliced"):
        await qs[:5].paginate(first=2)
    with pytest.raises(TypeError, match="first= with after="):
        await qs.paginate(first=2, before="x")
    with pytest.raises(TypeError, match="first= or last="):
        await qs.paginate()
    with pytest.raises(ValueError, match="at least 1"):
        await qs.paginate(first=0)
    with pytest.raises(TypeError, match=r"score=Item.score.desc\(\) is an ordering, for order_by\(\); write 0 - Item.score"):
        qs.update(score=-Item.score)
    for misuse in (lambda: qs.insert(owner_id=1, score=-Item.score, name="x", code="x", at=T0), lambda: qs.filter(Item.score == -Item.score).count()):
        with pytest.raises(TypeError, match=r"Item.score.desc\(\) is an ordering, for order_by\(\); write 0 - Item.score"):
            await misuse()
    cursor = (await qs.order_by(-Item.score).paginate(first=2)).next_cursor
    assert cursor == "eyJvIjoiMDQ5ZjZhMzYwOGNlYzljMiIsInYiOlsiMyIsIjgiXX0", "the same cursor as js/test/pagination.test.ts"
    with pytest.raises(QueryError, match="another order or model"):
        await qs.order_by(Item.score).paginate(first=2, after=cursor)
    for bad in ["%%%", "e30", "eyJvIjoxLCJ2IjpbXX0"]:
        with pytest.raises(QueryError, match="invalid cursor"):
            await qs.order_by(-Item.score).paginate(first=2, after=bad)


def edited(cursor, values):
    """`cursor` with its order values replaced by `values`."""
    data = json.loads(base64.urlsafe_b64decode(cursor + "=" * (-len(cursor) % 4)))
    return base64.urlsafe_b64encode(json.dumps({"o": data["o"], "v": values}).encode()).rstrip(b"=").decode()


async def test_edited_cursors_are_query_errors(items):
    Item, qs = items
    by_at = qs.order_by(-Item.at)
    at = (await by_at.paginate(first=1)).next_cursor
    by_score = qs.order_by(-Item.score)
    score = (await by_score.paginate(first=1)).next_cursor
    for bad in ["2026-10-01T00:00:00", "2026-10-01", 5]:
        with pytest.raises(QueryError, match="invalid cursor"):
            await by_at.paginate(first=2, after=edited(at, [bad, "1"]))
    for bad in ["1" * 23, "5.5", "2147483648", "-2147483649", "+3", " 3", None, 3]:
        with pytest.raises(QueryError, match="invalid cursor"):
            await by_score.paginate(first=2, after=edited(score, [bad, "1"]))
    with pytest.raises(QueryError, match="invalid cursor"):
        await by_score.paginate(first=2, after=edited(score, ["3", "1" * 23]))
    assert (await by_score.paginate(first=2, after=edited(score, ["-2147483648", "1"]))).items == []
    with pytest.raises(QueryError, match="another order or model"):
        await qs.order_by(Item.rank.asc(nulls="first")).paginate(first=2, after=(await qs.order_by(Item.rank.asc(nulls="last")).paginate(first=1)).next_cursor)
    with pytest.raises(QueryError, match="json columns have no cursor value"):
        await qs.order_by(Item.tags).paginate(first=2)


async def test_cursor_of_a_microsecond_datetime(items):
    Item, qs = items
    page = await qs.order_by(-Item.at).paginate(first=2)
    assert page.next_cursor == "eyJvIjoiNzNhMGQ4YWEwZWEzMWQwOSIsInYiOlsiMjAyNi0xMC0wMVQwMDowMDowMC4wMDAxNDcrMDA6MDAiLCIyMiJdfQ", "the same cursor as js/test/pagination.test.ts"
    assert await walk(qs.order_by(Item.name, -Item.at), 1) == [x.pk for x in await qs.order_by(Item.name, -Item.at)]


async def test_reading_back_from_the_second_row(items):
    Item, qs = items
    second = (await qs.paginate(first=2)).next_cursor
    page = await qs.paginate(last=1, before=second)
    assert [x.pk for x in page.items] == [1] and page.has_next and not page.has_previous
    assert [x.pk for x in (await qs.paginate(first=1, before=None)).items] == [1]
    assert [x.pk for x in (await qs.paginate(last=1, after=None)).items] == [23]


async def test_a_deep_page_has_a_leading_bound(items):
    Item, qs = items
    sql = qs.order_by(-Item.score, -Item.at).filter(pagination.after(Item, [Item.score.desc(), Item.at.desc()], [3, T0]))
    assert re.search(r'WHERE "page07_items"\."score" <= \S+ AND \(', sql.sql())
    nullable = qs.filter(pagination.after(Item, [Item.rank.asc(nulls="last"), Item.id.asc()], [1, 1])).sql()
    assert '"rank" >=' not in nullable and '"rank" > ' in nullable


NULLABLE_UNIQUE = """
model Coded {
  id Int @id
  code String? @unique
  @@map("page07_coded")
}
"""


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_a_nullable_unique_order_gets_the_primary_key(dialect):
    registry = orm.Registry()
    provider = "postgresql" if dialect == "postgres" else "sqlite"
    Coded = orm.loads(f'datasource db {{ provider = "{provider}" }}\n' + NULLABLE_UNIQUE, registry=registry)["Coded"]
    url = "sqlite://:memory:" if dialect == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    try:
        await Coded.objects.using(db).insert_many([{"id": 1, "code": "x"}, {"id": 2, "code": None}, {"id": 3, "code": None}, {"id": 4, "code": "y"}])
        assert await walk(Coded.objects.using(db).order_by(Coded.code.asc(nulls="last")), 1) == [1, 4, 2, 3]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.fixture
async def postgres():
    url = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")

    async def open(source):
        registry = orm.Registry()
        models = orm.loads(source, registry=registry)
        db = await orm.connect(url, registry=registry, default=False)
        await db.drop_tables()
        await db.create_tables()
        opened.append(db)
        return models, db

    opened = []
    yield open
    for db in opened:
        await db.drop_tables()
        await db.close()


async def test_a_char_order_column(postgres):
    models, db = await postgres('model Padded {\n  id Int @id\n  k String @db.Char(4)\n  @@map("page07_padded")\n}')
    Padded = models["Padded"]
    qs = Padded.objects.using(db)
    await qs.insert_many([{"id": i, "k": k} for i, k in enumerate(["ab", "ab", "ac", "ab", "a"], 1)])
    assert len(await qs.filter(Padded.k == (await qs.get(Padded.id == 1)).k)) == 3
    for size in (1, 2):
        assert await walk(qs.order_by(Padded.k), size) == [5, 1, 2, 4, 3]


async def test_a_char_column_from_ir_without_write_sql():
    """A generated module embeds compiled IR; its `char(n)` column has no `write_sql`."""
    source = orm.Registry()
    orm.loads('datasource db {\n  provider = "postgresql"\n}\nmodel Padded {\n  id Int @id\n  k String @db.Char(4)\n  @@map("page07_padded_ir")\n}', registry=source)
    ir = source.ir()
    for f in ir["models"][0]["fields"]:
        f.pop("write_sql", None)
    registry = orm.Registry()
    Padded = orm.define(ir, registry=registry)["Padded"]
    db = await orm.connect(os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test"), registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        qs = Padded.objects.using(db)
        await qs.insert_many([{"id": i, "k": k} for i, k in enumerate(["ab", "ab", "ac"], 1)])
        assert len(await qs.filter(Padded.k == (await qs.get(Padded.id == 1)).k)) == 2
        assert len(await qs.filter(Padded.k.in_(["ab"]))) == 2
    finally:
        await db.drop_tables()
        await db.close()


async def test_non_finite_float_order_values(postgres):
    models, db = await postgres('model Measure {\n  id Int @id\n  f Float\n  d Decimal\n  @@map("page07_measures")\n}')
    Measure = models["Measure"]
    qs = Measure.objects.using(db)
    await qs.insert_many([{"id": i, "f": f, "d": 1} for i, f in enumerate([1.0, math.inf, math.inf, math.nan, 2.0], 1)])
    assert await walk(qs.order_by(Measure.f), 2) == [1, 5, 2, 3, 4]
    page = await qs.order_by(Measure.f).paginate(first=2, after=(await qs.order_by(Measure.f).paginate(first=2)).next_cursor)
    assert page.next_cursor == "eyJvIjoiYWY4ZDc2YzZmZDZmZTllNyIsInYiOlsiSW5maW5pdHkiLCIzIl19", "the same cursor as js/test/pagination.test.ts"
    by_d = qs.order_by(Measure.d)
    with pytest.raises(QueryError, match="invalid cursor"):
        await by_d.paginate(first=1, after=edited((await by_d.paginate(first=1)).next_cursor, ["NaN", "1"]))
    await db.execute("UPDATE page07_measures SET d = 'NaN' WHERE id = 1")
    with pytest.raises(QueryError, match="can't make a cursor from the decimal NaN"):
        await qs.order_by(Measure.d.desc()).paginate(first=1)


PROXY = """
model User {
  id Int @id
  name String?
  code String? @unique
  @@map("page07_proxy_users")
}
model Named {
  name String
  code String @unique
  @@proxy.of(User)
}
"""


@pytest.mark.skipif("proxy-models" not in json.loads(_native.native_artifact())["capabilities"], reason="requires selected proxy native artifact")
async def test_a_proxy_narrowed_order_column(postgres):
    models, db = await postgres(PROXY)
    User, Named = models["User"], models["Named"]
    await User.objects.using(db).insert_many([{"id": 1, "name": "a", "code": "x"}, {"id": 2}, {"id": 3, "name": "c"}, {"id": 4, "code": "y"}])
    qs = Named.objects.using(db)
    with pytest.raises(QueryError, match="Named.name is nullable"):
        await qs.order_by(Named.name).paginate(first=1)
    assert await walk(qs.order_by(Named.name.asc(nulls="last")), 1) == [1, 3, 2, 4]
    assert await walk(qs.order_by(Named.code.asc(nulls="last")), 1) == [1, 4, 2, 3]
