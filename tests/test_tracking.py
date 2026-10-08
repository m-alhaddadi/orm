"""Run with an artifact that selects the timestamps, soft-delete or locking extension."""
import json
import os
from datetime import datetime, timezone

import pytest

import orm
from orm import _native

CAPABILITIES = set(json.loads(_native.native_artifact())["capabilities"])
DATABASES = ["sqlite", "postgres"]
OLD = datetime(2000, 1, 1, tzinfo=timezone.utc)


def requires(capability: str) -> pytest.MarkDecorator:
    return pytest.mark.skipif(capability not in CAPABILITIES, reason=f"requires a selected {capability} native artifact")


async def connect(dialect: str, source: str) -> tuple[orm.Database, dict]:
    if dialect == "sqlite":
        source = 'datasource db { provider = "sqlite" }\n' + source
    registry = orm.Registry()
    models = orm.loads(source, registry=registry)
    url = "sqlite://:memory:" if dialect == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    return db, models


def as_utc(value: datetime) -> datetime:
    return value if value.tzinfo else value.replace(tzinfo=timezone.utc)


@requires("updated-at")
@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("mode", ["database", "application"])
async def test_updated_at_is_set_on_orm_updates_and_by_the_trigger(dialect, mode):
    db, models = await connect(dialect, f"""
model Note {{
  id      Int      @id
  views   Int      @default(0)
  changed DateTime @default(now()) @timestamps.updated_at(mode: "{mode}")
  @@map("tracking_notes")
}}
""")
    Note = models["Note"]
    try:
        note = await Note.objects.using(db).insert(id=1, changed=OLD)
        await Note.objects.using(db).insert(id=2, changed=OLD)
        assert note.changed == OLD
        await note.update(views=1)
        assert as_utc(note.changed) > OLD
        (row,) = await Note.objects.using(db).filter(Note.id == 2).update(views=2).returning()
        assert as_utc(row.changed) > OLD
        await Note.objects.using(db).update_many([{"id": 1, "views": 3}])
        await note.update(changed=OLD)
        assert note.changed == OLD
        await db.execute("UPDATE tracking_notes SET views = 4 WHERE id = 1")
        await note.refresh()
        assert (as_utc(note.changed) > OLD) is (mode == "database")
    finally:
        await db.drop_tables()
        await db.close()


SOFT_DELETE = """
model Author {{
  id      Int       @id
  deleted DateTime? @soft_delete.deleted_at(mode: "{mode}")
  books   Book[]
  @@map("tracking_authors")
}}
model Book {{
  id        Int       @id
  author_id Int
  author    Author    @relation(fields: [author_id], references: [id], onDelete: Cascade)
  removed   DateTime? @soft_delete.deleted_at(mode: "{mode}")
  @@map("tracking_books")
}}
"""


@requires("soft-delete")
@pytest.mark.parametrize("dialect", DATABASES)
@pytest.mark.parametrize("mode", ["database", "application"])
async def test_soft_delete_updates_instead_of_deleting(dialect, mode):
    db, models = await connect(dialect, SOFT_DELETE.format(mode=mode))
    Author, Book = models["Author"], models["Book"]
    try:
        authors = Author.objects.using(db)
        await authors.insert_many([{"id": 1}, {"id": 2}, {"id": 3}])
        await Book.objects.using(db).insert_many([{"id": 1, "author_id": 1}, {"id": 2, "author_id": 2}])
        assert await authors.filter(Author.id == 1).delete() == 1
        assert await authors.filter(Author.id == 1).delete() == 0
        (row,) = await authors.filter(Author.id == 2).delete().returning()
        assert row.id == 2 and row.deleted is not None
        assert await authors.count() == 3
        assert [a.id for a in await authors.deleted_only().order_by(Author.id)] == [1, 2]
        assert await authors.all_with_deleted().count() == 3
        removed = [b.id for b in await Book.objects.using(db).deleted_only().order_by(Book.id)]
        assert removed == ([1, 2] if mode == "database" else [])
        assert await authors.filter(Author.id == 1).undelete() == 1
        restored = await authors.get(Author.id == 1)
        assert restored.deleted is None
        await restored.delete()
        assert (await authors.get(Author.id == 1)).deleted is not None
        await restored.undelete()
        assert restored.deleted is None
        await db.execute("DELETE FROM tracking_books")
        await db.execute("DELETE FROM tracking_authors WHERE id = 3")
        assert await authors.filter(Author.id == 3).count() == (1 if mode == "database" else 0)
        assert await authors.filter(Author.id == 2).hard_delete() == 1
        assert await authors.filter(Author.id == 1).hard_delete() == (0 if mode == "database" else 1)
    finally:
        await db.drop_tables()
        await db.close()


@requires("optimistic-locking")
@pytest.mark.parametrize("dialect", DATABASES)
async def test_a_stale_instance_write_raises_version_conflict(dialect):
    db, models = await connect(dialect, """
model Doc {
  id      Int    @id
  title   String
  version Int    @default(0) @locking.version
  @@map("tracking_docs")
}
""")
    Doc = models["Doc"]
    try:
        docs = Doc.objects.using(db)
        first = await docs.insert(id=1, title="a")
        second = await docs.get(Doc.id == 1)
        await first.update(title="b")
        assert first.version == 1 and first.title == "b"
        with pytest.raises(orm.VersionConflict):
            await second.update(title="c")
        with pytest.raises(orm.VersionConflict):
            await second.delete()
        assert (await docs.get(Doc.id == 1)).title == "b"
        assert await docs.update(title="d") == 1
        assert (await docs.get(Doc.id == 1)).version == 2
        await second.refresh()
        await second.update(title="e")
        assert second.version == 3
        await second.delete()
        with pytest.raises(Doc.DoesNotExist):
            await first.update(title="f")
        with pytest.raises(TypeError, match="no @soft_delete"):
            docs.deleted_only()
    finally:
        await db.drop_tables()
        await db.close()
