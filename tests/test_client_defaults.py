"""`@client_default`: the ORM fills omitted insert values; `@default` stays the database default."""
import datetime
import os
import uuid

import pytest

import orm
from orm.fields import Integer, String

SOURCE = """
enum Status {
  ACTIVE @map("active")
  OLD @map("old")
  @@storage(text)
}
model Item {
  id     String   @id @client_default(uuid7()) @db.Uuid
  token  String   @client_default(uuid())
  status Status   @default(OLD) @client_default(ACTIVE)
  at     DateTime @client_default(now())
  meta   Json     @client_default("{\\"a\\": [1]}")
  n      Int      @client_default(3)
  note   String?  @client_default("note")
  prisma String   @default(uuid())
  @@map("client_default_items")
}
"""
URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")


def test_client_defaults_make_fields_optional_but_stay_out_of_the_database_default():
    Item = orm.loads(SOURCE, registry=orm.Registry())["Item"]
    for name in ("id", "token", "status", "at", "meta", "n", "note", "prisma"):
        assert Item._meta.fields[name].has_insert_default
    assert Item._meta.fields["status"].has_server_value
    assert not Item._meta.fields["token"].has_server_value
    assert Item._meta.fields["id"].ir()["client_default"] == {"call": "uuid7"}


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_client_defaults_fill_insert_bulk_insert_and_upsert(dialect):
    source = SOURCE if dialect == "postgres" else 'datasource db { provider = "sqlite" }\n' + SOURCE
    registry = orm.Registry()
    models = orm.loads(source, registry=registry)
    Item, Status = models["Item"], models["Status"]
    db = await orm.connect("sqlite://:memory:" if dialect == "sqlite" else URL, registry=registry, default=False)
    await db.create_tables()
    try:
        item = await Item.objects.using(db).insert()
        assert isinstance(item.id, uuid.UUID) and item.id.version == 7
        assert uuid.UUID(item.token).version == 4 and uuid.UUID(item.prisma).version == 4
        assert item.status is Status.ACTIVE
        assert abs(item.at - datetime.datetime.now(datetime.timezone.utc)) < datetime.timedelta(minutes=1)
        assert item.meta == {"a": [1]} and item.n == 3 and item.note == "note"
        # An explicit value, also None, wins over both defaults.
        explicit = await Item.objects.using(db).insert(note=None, status=Status.OLD, n=9)
        assert explicit.note is None and explicit.status is Status.OLD and explicit.n == 9
        rows = await Item.objects.using(db).insert_many([{"note": "x"}, {}, {"note": None}]).returning()
        assert [r.note for r in rows] == ["x", "note", None]
        assert len({r.id for r in rows} | {item.id, explicit.id}) == 5
        upserted = await Item.objects.using(db).insert(id=item.id, n=4).on_conflict(Item.id, update=True, update_fields=[Item.n, Item.token]).returning()
        assert upserted is not None and upserted.id == item.id and upserted.n == 4 and upserted.token != item.token
        # Other writers get only the database default.
        await db.execute(
            "INSERT INTO client_default_items (id, token, at, meta, n, prisma) "
            "VALUES ('0192f7e2-0000-7000-8000-000000000002', 't', '2026-01-01T00:00:00Z', '{}', 1, 'p')"
        )
        raw = await Item.objects.using(db).get(Item.token == "t")
        assert raw.status is Status.OLD and raw.note is None
    finally:
        await db.drop_tables()
        await db.close()


async def test_python_callable_defaults_fill_the_same_omitted_values():
    registry = orm.Registry()
    made = iter(range(100))

    class Counter(orm.Model, registry=registry, table="client_default_counters"):
        id = Integer(primary_key=True)
        label = String(default=lambda: f"made-{next(made)}", nullable=True)
        token = String(client_default={"call": "uuid"}, nullable=True)

    registry.prepare()
    assert Counter._meta.fields["token"].has_insert_default and not Counter._meta.fields["token"].has_server_value
    db = await orm.connect(URL, registry=registry, default=False)
    await db.create_tables()
    try:
        rows = await Counter.objects.using(db).insert_many([{"id": 1}, {"id": 2, "label": None, "token": None}, {"id": 3}]).returning()
        assert [r.label for r in rows] == ["made-0", None, "made-1"]
        assert [r.token is None for r in rows] == [False, True, False]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_a_json_null_client_default_writes_json_null(dialect):
    source = 'model Doc {\n  id Int @id\n  a Json @client_default("null")\n  @@map("client_default_docs")\n}'
    if dialect == "sqlite":
        source = 'datasource db { provider = "sqlite" }\n' + source
    registry = orm.Registry()
    Doc = orm.loads(source, registry=registry)["Doc"]
    db = await orm.connect("sqlite://:memory:" if dialect == "sqlite" else URL, registry=registry, default=False)
    await db.create_tables()
    try:
        # The column is NOT NULL, so an SQL NULL fails the insert.
        doc = await Doc.objects.using(db).insert(id=1)
        assert doc.a is None
    finally:
        await db.drop_tables()
        await db.close()
