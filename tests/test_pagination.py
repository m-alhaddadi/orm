"""Cursor pagination on both supported databases."""

import os
from datetime import datetime, timedelta, timezone

import pytest

import orm
from orm import QueryError

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
    page = await qs.select_related(Item.owner).only(Item.name).order_by(-Item.at).paginate(first=5)
    assert len(page.items) == 5 and page.has_next and not page.has_previous
    assert page.items[0].owner.name == "o"
    with pytest.raises(orm.NotLoaded):
        page.items[0].at
    rest = await qs.only(Item.name).order_by(-Item.at).paginate(first=100, after=page.next_cursor)
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
    cursor = (await qs.order_by(-Item.score).paginate(first=2)).next_cursor
    assert cursor == "eyJvIjoiMDQ5ZjZhMzYwOGNlYzljMiIsInYiOlsiMyIsIjgiXX0", "the same cursor as js/test/pagination.test.ts"
    with pytest.raises(QueryError, match="another order or model"):
        await qs.order_by(Item.score).paginate(first=2, after=cursor)
    for bad in ["%%%", "e30", "eyJvIjoxLCJ2IjpbXX0"]:
        with pytest.raises(QueryError, match="invalid cursor"):
            await qs.order_by(-Item.score).paginate(first=2, after=bad)
