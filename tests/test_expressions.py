"""JSON paths, containment and merge, on a schema loaded at runtime."""

import os

import pytest

import orm
from orm import QueryError

SOURCE = """
model Doc {
  id    BigInt  @id @default(autoincrement())
  title String
  meta  Json
  tags  String[] @default([])
  @@map("expr_docs")
}
"""
URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")


def load(source: str = SOURCE):
    registry = orm.Registry()
    return registry, orm.loads(source, registry=registry)["Doc"]


@pytest.fixture
async def docs():
    registry, Doc = load()
    try:
        db = await orm.connect(URL, registry=registry, default=False, max_connections=2)
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {URL}: {e}")
    await db.drop_tables()
    await db.create_tables()
    objects = Doc.objects.using(db)
    await objects.insert_many(
        [
            {"title": "a", "meta": {"kind": "post", "n": 5, "author": {"name": "Ann"}, "tags": ["x", "y"]}},
            {"title": "b", "meta": {"kind": "page", "n": 1, "author": {"name": "Bob"}, "tags": ["y"]}},
            {"title": "c", "meta": {"kind": "post", "n": 9}},
        ]
    )
    yield Doc, objects
    await db.drop_tables()
    await db.close()


def test_json_path_sql():
    _, Doc = load()
    assert Doc.objects.filter(Doc.meta["author"]["name"] == "Ann").sql().endswith(
        """WHERE (("expr_docs"."meta" -> 'author') -> 'name') = '"Ann"'"""
    )
    assert Doc.objects.filter(Doc.meta["tags"][0].as_text() == "x").sql().endswith(
        """WHERE (("expr_docs"."meta" -> 'tags') ->> 0) = 'x'"""
    )
    assert Doc.objects.filter(Doc.meta.json_contains({"kind": "post"})).sql().endswith(
        """WHERE "expr_docs"."meta" @> '{"kind":"post"}'"""
    )
    assert Doc.objects.filter(Doc.meta.has_key("tags")).sql().endswith("""WHERE "expr_docs"."meta" ? 'tags'""")
    # On an array column, an int index stays SQL's array element access.
    assert '("expr_docs"."tags")[1]' in Doc.objects.select(Doc.tags[1]).sql()
    with pytest.raises(TypeError, match="not a JSON column"):
        Doc.title["a"]  # type: ignore[index]
    with pytest.raises(TypeError, match="ends a JSON path"):
        Doc.meta["a"].as_text()["b"]  # type: ignore[index]


async def test_json_paths(docs):
    Doc, objects = docs
    assert await objects.filter(Doc.meta["author"]["name"] == "Ann").select(Doc.title).scalars() == ["a"]
    assert await objects.filter(Doc.meta["n"] > 3).order_by(Doc.id).select(Doc.title).scalars() == ["a", "c"]
    assert await objects.filter(Doc.meta["tags"][0].as_text() == "y").select(Doc.title).scalars() == ["b"]
    assert await objects.filter(Doc.meta["author"]["name"].as_text().startswith("B")).select(Doc.title).scalars() == ["b"]
    rows = await objects.order_by(Doc.id).select(Doc.meta["author"].label("author"), Doc.meta["kind"].as_text().label("kind"))
    assert [tuple(r) for r in rows] == [({"name": "Ann"}, "post"), ({"name": "Bob"}, "page"), (None, "post")]
    assert await objects.order_by(Doc.meta["n"].desc()).select(Doc.title).scalars() == ["c", "a", "b"]


async def test_json_containment_and_keys(docs):
    Doc, objects = docs
    assert await objects.filter(Doc.meta.json_contains({"kind": "post"})).count() == 2
    assert await objects.filter(Doc.meta["tags"].json_contains(["y"])).count() == 2
    assert await objects.filter(Doc.meta.json_contained_by({"kind": "post", "n": 9, "x": 1})).select(Doc.title).scalars() == ["c"]
    assert await objects.filter(Doc.meta.has_key("author")).count() == 2
    assert await objects.filter(~Doc.meta.has_key("tags")).select(Doc.title).scalars() == ["c"]


async def test_json_merge_in_update(docs):
    Doc, objects = docs
    await objects.filter(Doc.title == "c").update(meta=Doc.meta.json_merge({"n": 10, "seen": True}))
    doc = await objects.get(Doc.title == "c")
    assert doc.meta == {"kind": "post", "n": 10, "seen": True}


async def test_json_on_sqlite_is_an_error():
    registry, Doc = load('datasource db { provider = "sqlite" }\n' + SOURCE.replace("  tags  String[] @default([])\n", ""))
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        for cond in (Doc.meta["a"] == 1, Doc.meta.json_contains({"a": 1}), Doc.meta.has_key("a")):
            with pytest.raises(QueryError, match="sqlite does not support"):
                await Doc.objects.using(db).filter(cond).count()
    finally:
        await db.close()


# -- full-text search -------------------------------------------------------------------------

ARTICLES = """
model Article {
  id   BigInt @id @default(autoincrement())
  body String
  @@index([sql("to_tsvector('english', body)")], type: Gin, name: "articles_body_search")
  @@map("expr_articles")
}
"""


def load_articles(source: str = ARTICLES):
    registry = orm.Registry()
    return registry, orm.loads(source, registry=registry)["Article"]


def test_full_text_search_sql():
    from orm import func

    registry, Article = load_articles()
    vector = func.to_tsvector("english", Article.body)
    assert Article.objects.filter(vector.matches("running dogs")).sql().endswith(
        """WHERE TO_TSVECTOR('english'::regconfig, "expr_articles"."body") @@ PLAINTO_TSQUERY('english'::regconfig, 'running dogs')"""
    )
    sql = Article.objects.order_by(func.ts_rank(vector, func.websearch_to_tsquery("english", "fox -lazy")).desc()).sql()
    assert "ORDER BY CAST(TS_RANK(TO_TSVECTOR('english'::regconfig, " in sql and "WEBSEARCH_TO_TSQUERY('english'::regconfig, 'fox -lazy')" in sql
    assert 'TO_TSVECTOR("expr_articles"."body") @@ TO_TSQUERY(\'cat & !dog\')' in Article.objects.filter(
        func.to_tsvector(Article.body).matches(func.to_tsquery("cat & !dog"))
    ).sql()
    # The GIN index on the same expression, from the schema.
    assert any(
        'CREATE INDEX IF NOT EXISTS "articles_body_search" ON "expr_articles" USING gin ((to_tsvector(\'english\', body)))' in s
        for s in registry.native().ddl()
    ), registry.native().ddl()
    with pytest.raises(QueryError, match="text search configuration"):
        Article.objects.filter(func.to_tsvector("english'; drop", Article.body).matches("x")).sql()


async def test_full_text_search():
    from orm import func

    registry, Article = load_articles()
    try:
        db = await orm.connect(URL, registry=registry, default=False, max_connections=2)
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {URL}: {e}")
    try:
        await db.drop_tables()
        await db.create_tables()
        objects = Article.objects.using(db)
        await objects.insert_many(
            [{"body": "The quick brown fox jumps"}, {"body": "A lazy dog sleeps"}, {"body": "Dogs are running; the fox and the dog"}]
        )
        vector = func.to_tsvector("english", Article.body)
        found = await objects.filter(vector.matches("running dogs")).select(Article.id).scalars()
        assert found == [3]  # stemmed: "running" is "run", "dogs" is "dog"
        query = func.websearch_to_tsquery("english", "fox -lazy")
        ranked = await objects.filter(vector.matches(query)).order_by(func.ts_rank(vector, query).desc(), Article.id).select(Article.id).scalars()
        assert sorted(ranked) == [1, 3]
        rank = await objects.filter(Article.id == 3).select(func.ts_rank(vector, func.to_tsquery("english", "dog"))).scalar()
        assert isinstance(rank, float) and rank > 0
    finally:
        await db.drop_tables()
        await db.close()


async def test_full_text_search_on_sqlite_is_an_error():
    from orm import func

    registry, Article = load_articles('datasource db { provider = "sqlite" }\n' + ARTICLES.replace(
        '  @@index([sql("to_tsvector(\'english\', body)")], type: Gin, name: "articles_body_search")\n', ""
    ))
    db = await orm.connect("sqlite://:memory:", registry=registry, default=False)
    try:
        await db.create_tables()
        with pytest.raises(QueryError, match="full-text search needs PostgreSQL"):
            await Article.objects.using(db).filter(func.to_tsvector(Article.body).matches("x")).count()
    finally:
        await db.close()
