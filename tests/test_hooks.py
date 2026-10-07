"""The public hooks that packages such as orm-file-storage use (`orm.hooks`)."""
import pytest
import orm
from orm.hooks import decode_field, prepare_insert, prepare_update

SCHEMA = 'datasource db { provider = "sqlite" }\nmodel HookReport {\n id Int @id\n code String @unique\n size Int\n note String?\n}'


async def open_db():
    registry = orm.Registry()
    Report = orm.loads(SCHEMA, registry=registry)["HookReport"]
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    await db.create_tables()
    return db, Report


async def test_prepare_insert_checks_before_execute_and_inserts_replaced_values():
    db, Report = await open_db()
    try:
        objects = Report.objects.using(db)
        with pytest.raises(ValueError, match="size is required"):
            prepare_insert(objects, {"id": 1, "code": "a"})
        with pytest.raises(Exception):
            prepare_insert(objects, {"id": "not an integer", "code": "a", "size": 1})
        prepared = prepare_insert(objects, {"id": 1, "code": "a", "size": 0})
        assert await objects.count() == 0
        row = await prepared.execute({"id": 1, "code": "a", "size": 7})
        assert (row.id, row.size) == (1, 7)
        assert (await prepare_insert(objects, {"id": 2, "code": "b", "size": 2}).execute()).code == "b"
    finally:
        await db.close()


async def test_prepare_update_reports_a_unique_row_and_executes():
    db, Report = await open_db()
    try:
        objects = Report.objects.using(db)
        await objects.insert_many([{"id": 1, "code": "a", "size": 1}, {"id": 2, "code": "b", "size": 1}])
        assert prepare_update(objects.filter(Report.id == 1), {"size": 0}).unique
        assert prepare_update(objects.filter((Report.size == 1) & (Report.code == "b")), {"size": 0}).unique
        assert not prepare_update(objects.filter(Report.size == 1), {"size": 0}).unique
        assert not prepare_update(objects.filter(Report.note == None), {"size": 0}).unique  # noqa: E711
        with pytest.raises(Exception):
            prepare_update(objects.filter(Report.id == 1), {"size": "not an integer"})
        prepared = prepare_update(objects.filter(Report.id == 1), {"size": 0})
        rows = await prepared.execute({"size": 5}, returning=True)
        assert [r.size for r in rows] == [5]
        assert await prepared.execute() == 1
    finally:
        await db.close()


async def test_decode_field_reads_the_loaded_value_through_the_decoder():
    db, Report = await open_db()
    try:
        decode_field(Report, "code", str.upper)
        objects = Report.objects.using(db)
        await objects.insert(id=1, code="a", size=1)
        row = await objects.get(Report.id == 1)
        assert row.code == "A"
        assert row.__dict__["code"] == "a"
        assert await objects.filter(Report.code == "a").count() == 1
        with pytest.raises(AttributeError):
            row.code = "b"
        with pytest.raises(TypeError):
            decode_field(Report, "missing", str)
    finally:
        await db.close()
