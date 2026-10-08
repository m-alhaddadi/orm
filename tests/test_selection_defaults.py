"""Partial output shapes and compiled defaults on both supported databases."""
import json
import os

import pytest
import orm
from orm import _native

QUERY_DEFAULTS = "query-defaults" in json.loads(_native.native_artifact()).get("capabilities", [])


def schema(dialect, defaults=True):
    def field(name, kind="string", **flags):
        return {"name": name, "column": name, "type": kind, **flags}
    return {
        "dialect": dialect,
        "models": [
            {"name": "SelectedAuthor", "table": "selection03_authors", "fields": [
                field("name"), field("id", "int", primary_key=True, auto_increment=True),
                field("visible", "bool", default=True), field("bio", "text"), field("note", nullable=True),
            ], "relations": [{"name": "books", "kind": "many", "target": "SelectedBook", "from": "id", "to": "author_id"}]},
            {"name": "SelectedBook", "table": "selection03_books", "fields": [
                field("title"), field("author_id", "int"), field("id", "int", primary_key=True, auto_increment=True),
            ], "relations": [{"name": "author", "kind": "one", "target": "SelectedAuthor", "from": "author_id", "to": "id", "foreign_key": True, "on_delete": "cascade"}]},
        ],
        "behavior": {"schema_contract": 1, "query_defaults": [
            {"model": "SelectedAuthor", "filter": {"t": "col", "path": [], "name": "visible"}, "fields": ["name", "note"]},
            {"model": "SelectedBook", "fields": ["title"], "related": [["author"]]},
        ]} if defaults else {},
    }


async def open_db(dialect, defaults):
    registry = orm.Registry()
    classes = orm.define(schema(dialect, defaults), registry=registry)
    url = "sqlite://:memory:" if dialect == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    return db, classes["SelectedAuthor"], classes["SelectedBook"]


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_partial_selection(dialect):
    db, Author, Book = await open_db(dialect, defaults=False)
    a, b = Author.objects.using(db), Book.objects.using(db)
    try:
        author = await a.insert(name="ann", bio="large", note=None)
        await a.insert(name="bob", bio="large")
        await b.insert(title="one", author_id=author.pk)
        partial = await a.only(Author.name, Author.note).order_by(Author.id).first()
        assert partial.to_dict() == {"name": "ann", "note": None}
        assert partial.pk == author.pk
        with pytest.raises(orm.NotLoaded):
            partial.bio
        with pytest.raises(TypeError):
            a.only(Author.name, Author.name)
        await partial.update(name="changed")
        assert partial.to_dict() == {"name": "changed", "note": None}
        await partial.refresh(Author.bio)
        assert partial.to_dict() == {"name": "changed", "note": None, "bio": "large"}
        assert [o.name async for batch in a.only(Author.name).batches(1) for o in batch] == ["changed", "bob"]
        assert (await partial.books.using(db).get()).title == "one"
        full = await a.only().order_by(Author.id).first()
        assert "_orm_internal" not in full.__dict__ and full.bio == "large"
        book = await b.only(Book.title).get()
        with pytest.raises(orm.NotLoaded):
            book.author
        assert (await b.select_related(Book.author).only(Book.title).get()).author.name == "changed"
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="needs a native build with the query-defaults feature")
@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_selection_defaults(dialect):
    db, Author, Book = await open_db(dialect, defaults=True)
    a, b = Author.objects.using(db), Book.objects.using(db)
    try:
        with pytest.raises(ValueError):
            await a.insert(name="missing required excluded field")
        visible = await a.insert(name="visible", bio="large", note=None)
        hidden = await a.insert(name="hidden", bio="large", visible=False)
        await b.insert(title="one", author_id=visible.pk)
        await b.insert(title="two", author_id=hidden.pk)
        partial = await a.get()
        assert partial.to_dict() == {"name": "visible", "note": None}
        assert partial.pk == visible.pk
        for name in ("id", "bio", "visible"):
            with pytest.raises(orm.NotLoaded):
                getattr(partial, name)
        assert "bio" not in a.sql()
        assert await a.count() == 1 and await a.exists()
        assert await a.without_defaults().count() == 2
        assert await a.without_defaults().filter(Author.name == "visible").count() == 1
        full = await a.only().get()
        assert full.bio == "large"
        explicit = await a.only(Author.note, Author.name).get()
        assert explicit.to_dict() == {"name": "visible", "note": None}
        await explicit.refresh()
        assert explicit.to_dict() == {"name": "visible", "note": None}
        await explicit.refresh(Author.bio)
        assert explicit.to_dict() == {"name": "visible", "note": None, "bio": "large"}
        await partial.update(name="changed")
        assert partial.to_dict() == {"name": "changed", "note": None}
        joined = await b.order_by(Book.id)
        assert len(joined) == 2 and joined[0].author.name == "changed" and joined[1].author is None
        assert joined[0].to_dict() == {"title": "one"}
        assert joined[0].author.to_dict() == {"name": "changed", "note": None}
        with pytest.raises(orm.NotLoaded):
            joined[0].author_id
        await joined[1].update(author_id=visible.pk)
        with pytest.raises(orm.NotLoaded):
            joined[1].author
        await joined[1].update(author_id=hidden.pk)
        assert joined[1]._field_value("author_id") == hidden.pk
        assert await b.filter(Book.author.name == "hidden").update_many([{"id": joined[1].pk, "title": "t"}]) == 0
        bypassed = await b.without_defaults().select_related(Book.author).order_by(Book.id)
        assert bypassed[0].author.name == "changed" and bypassed[0].author.visible is True
        assert bypassed[1].author.name == "hidden"
        assert await a.in_bulk() == {visible.pk: partial}
        changed_return = await a.only(Author.name).update(name="again").returning()
        assert changed_return[0].to_dict() == {"name": "again"}
        default_return = await a.filter(Author.id == visible.pk).update(note="n").returning()
        assert default_return[0].to_dict() == {"name": "again", "note": "n"}
        cleared = await b.without_related().first()
        with pytest.raises(orm.NotLoaded):
            cleared.author
        prefetched = await a.prefetch_related(Author.books).get()
        assert prefetched.books.cached[0].title == "one"
        bypassed_prefetch = await a.without_defaults().prefetch_related(Author.books).filter(Author.id == visible.pk).get()
        assert bypassed_prefetch.books.cached[0].title == "one" and bypassed_prefetch.books.cached[0].author_id == visible.pk
        assert await a.filter(Author.books.title == "one").count() == 1
        written = await a.only().update(visible=False).returning()
        assert len(written) == 1 and written[0].visible is False
        assert await a.count() == 0
        assert await a.without_defaults().update_many([{"id": hidden.pk, "name": "bulk"}]) == 1
        assert await a.update_many([{"id": hidden.pk, "name": "excluded"}]) == 0
        assert await a.delete() == 0
        assert await a.without_defaults().filter(Author.id == hidden.pk).delete() == 1
    finally:
        await db.drop_tables()
        await db.close()


ORDERED = """
datasource db {
  provider = "sqlite"
}
model Topic {
  id Int @id @default(autoincrement())
  rank Int?
  name String
  notes Note[]
  @@map("order09_topics")
  @@query.order("-rank nulls last", "id")
}
model Note {
  id Int @id @default(autoincrement())
  topic_id Int
  body String
  topic Topic @relation(fields: [topic_id], references: [id], onDelete: Cascade)
  @@map("order09_notes")
  @@query.defaults(order: ["-body"])
}
"""


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="needs a native build with the query-defaults feature")
@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_default_order(dialect):
    registry = orm.Registry()
    models = orm.loads(ORDERED.replace('"sqlite"', '"postgresql"' if dialect == "postgres" else '"sqlite"'), registry=registry)
    Topic, Note = models["Topic"], models["Note"]
    url = "sqlite://:memory:" if dialect == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
    t, n = Topic.objects.using(db), Note.objects.using(db)

    def names(rows):
        return [r.name for r in rows]

    try:
        for name, rank in [("a", 1), ("b", None), ("c", 3), ("d", 3)]:
            await t.insert(name=name, rank=rank)
        assert names(await t.all()) == ["c", "d", "a", "b"]
        assert (await t.first()).name == "c" and (await t.last()).name == "b"
        assert names(await t.order_by(Topic.name.desc())) == ["d", "c", "b", "a"]
        assert names(await t.order_by(Topic.rank.asc(nulls="first"), Topic.id)) == ["b", "a", "c", "d"]
        assert "ORDER BY" not in t.without_defaults().sql() and "ORDER BY" not in t.filter(Topic.id > 0).without_defaults().sql()
        assert (await t.without_defaults().first()).name == "a"
        assert await t.count() == 4 and await t.exists()
        assert [names(b) async for b in t.batches(2)] == [["a", "b"], ["c", "d"]]
        page = await t.paginate(first=3)
        assert names(page.items) == ["c", "d", "a"]
        assert names((await t.paginate(first=3, after=page.next_cursor)).items) == ["b"]
        assert names((await t.without_defaults().paginate(first=3)).items) == ["a", "b", "c"]
        assert (await t.from_(t.cte("all_topics")).first()).name == "a"
        # Postgres rejects UPDATE ... ORDER BY, so these also show that writes take no default order.
        assert await t.filter(Topic.name == "none").update(name="x") == 0
        assert await t.filter(Topic.name == "none").delete() == 0
        c = await t.get(Topic.name == "c")
        for body in ["x", "z", "y"]:
            await n.insert(topic_id=c.pk, body=body)
        assert [x.body for x in await n.all()] == ["z", "y", "x"]
        loaded = await t.prefetch_related(Topic.notes).get(Topic.name == "c")
        assert [x.body for x in await loaded.notes] == ["z", "y", "x"]
        own = await t.prefetch_related(orm.Prefetch(Topic.notes, Note.objects.order_by(Note.body))).get(Topic.name == "c")
        assert [x.body for x in await own.notes] == ["x", "y", "z"]
        sliced = await t.prefetch_related(orm.Prefetch(Topic.notes, Note.objects.all()[:2])).get(Topic.name == "c")
        assert [x.body for x in await sliced.notes] == ["z", "y"]
    finally:
        await db.drop_tables()
        await db.close()


@pytest.mark.skipif(not QUERY_DEFAULTS, reason="needs a native build with the query-defaults feature")
def test_default_order_options_have_one_source():
    both = ORDERED.replace('@@query.defaults(order: ["-body"])', '@@query.defaults(order: ["-body"])\n  @@query.order("id")')
    with pytest.raises(Exception, match=r"Note: order is set by both @@query\.defaults\(order:\) at .* and @@query\.order at"):
        orm.loads(both, registry=orm.Registry())
    with pytest.raises(Exception, match=r"order column \"nope\" is not a field"):
        orm.loads(ORDERED.replace('"-body"', '"nope"'), registry=orm.Registry())
    with pytest.raises(Exception, match=r'Topic: order column "rank" can be NULL: write "-rank nulls first" or "-rank nulls last"'):
        orm.loads(ORDERED.replace('"-rank nulls last"', '"-rank"'), registry=orm.Registry())
    with pytest.raises(Exception, match=r'elsewhere quote it: "-name"'):
        orm.loads(ORDERED.replace('"-rank nulls last", "id"', "-name"), registry=orm.Registry())
    with pytest.raises(Exception, match=r'order column "k" is json, which the database can\'t order'):
        orm.loads(ORDERED.replace("  rank Int?\n", "  rank Int?\n  k Json @db.Json\n").replace('"-rank nulls last", "id"', '"k"').replace('"sqlite"', '"postgresql"'), registry=orm.Registry())
