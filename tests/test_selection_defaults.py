"""Partial output shapes and compiled defaults on both supported databases."""
import os

import pytest
import orm


def schema(dialect):
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
        ]},
    }


@pytest.mark.parametrize("dialect", ["sqlite", "postgres"])
async def test_selection_defaults(dialect):
    registry = orm.Registry()
    classes = orm.define(schema(dialect), registry=registry)
    Author, Book = classes["SelectedAuthor"], classes["SelectedBook"]
    url = "sqlite://:memory:" if dialect == "sqlite" else os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")
    db = await orm.connect(url, registry=registry, default=False)
    await db.drop_tables()
    await db.create_tables()
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
        bypassed = await b.without_defaults().select_related(Book.author).order_by(Book.id)
        assert bypassed[0].author.name == "changed" and bypassed[0].author.visible is True
        await joined[1].update(author_id=hidden.pk)
        assert await a.in_bulk() == {visible.pk: partial}
        changed_return = await a.only(Author.name).update(name="again").returning()
        assert changed_return[0].to_dict() == {"name": "again"}
        cleared = await b.without_related().first()
        with pytest.raises(orm.NotLoaded):
            cleared.author
        prefetched = await a.prefetch_related(Author.books).get()
        assert prefetched.books.cached[0].title == "one"
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
