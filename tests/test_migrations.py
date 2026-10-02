"""Migration generation: no database needed."""

import json

import pytest

import orm
from orm import Check, Exclude, Extension, Function, Index, Key, Model, QueryError, Registry, Sql, Trigger, Unique
from orm import fields as f
from orm.__main__ import main as cli
from orm.ext import btree_gist, citext, pg_trgm, pgvector, postgis
from orm.migrations import Migrations


def v1() -> Registry:
    reg = Registry()

    class Author(Model, table="authors", registry=reg):
        id = f.BigInt(primary_key=True, auto_increment=True)
        email = f.String(254, unique=True)
        books = f.HasMany("Book", via="author_id")

    class Book(Model, table="books", registry=reg):
        id = f.BigInt(primary_key=True, auto_increment=True)
        author_id = f.BigInt(index=True)
        title = f.String(200)
        pages = f.Integer(default=0)
        author = f.BelongsTo("Author", via="author_id")

    return reg


def v2() -> Registry:
    """v1 + a renamed column, new columns, indexes, constraints, a trigger."""
    reg = Registry()

    class Author(Model, table="authors", registry=reg):
        id = f.BigInt(primary_key=True, auto_increment=True)
        email = citext.CIText(unique=True)
        books = f.HasMany("Book", via="author_id")

    class Book(Model, table="books", registry=reg):
        id = f.BigInt(primary_key=True, auto_increment=True)
        author_id = f.BigInt(index=True)
        name = f.String(200, renamed_from="title")
        pages = f.Integer(default=0, check="pages >= 0")
        meta = f.Json(nullable=True, comment="free-form attributes")
        updated_at = f.DateTime(default_now=True)
        author = f.BelongsTo("Author", via="author_id")

        class Meta:
            indexes = [Index("author_id", "-updated_at", where="pages > 0"), pg_trgm.TrigramIndex("name")]
            constraints = [Unique("author_id", "name")]
            triggers = [
                Trigger("touch", before=("update",), body="BEGIN NEW.updated_at := now(); RETURN NEW; END;")
            ]

    return reg


def test_meta_compiles_to_schema_ir():
    ir = v2().ir()
    book = next(m for m in ir["models"] if m["name"] == "Book")
    assert book["indexes"][0] == {
        "columns": [{"field": "author_id"}, {"field": "updated_at", "desc": True}],
        "where": "pages > 0",
    }
    assert book["indexes"][1] == {"columns": [{"field": "name", "opclass": "gin_trgm_ops"}], "method": "gin"}
    assert book["constraints"] == [{"kind": "unique", "fields": ["author_id", "name"]}]
    assert book["triggers"][0]["timing"] == "before" and book["triggers"][0]["events"] == ["update"]
    name = next(x for x in book["fields"] if x["name"] == "name")
    assert name["renamed_from"] == "title"
    author = next(m for m in ir["models"] if m["name"] == "Author")
    email = next(x for x in author["fields"] if x["name"] == "email")
    assert email["db_type"] == "citext" and email["write_sql"] == "CAST({} AS citext)"


def test_meta_rejects_unknown_options_and_objects():
    with pytest.raises(TypeError, match="unknown option"):

        class Bad(Model, registry=Registry()):
            id = f.BigInt(primary_key=True)

            class Meta:
                index = []

    with pytest.raises(TypeError, match="expected Index"):

        class Bad2(Model, registry=Registry()):
            id = f.BigInt(primary_key=True)

            class Meta:
                indexes = ["id"]

    with pytest.raises(TypeError, match="exactly one of before"):
        Trigger("t", body="x")


def test_first_migration(tmp_path):
    migs = Migrations(tmp_path, v1())
    m = migs.make()
    assert m is not None and m.name == "0001_initial"
    up = m.up_sql
    assert up.index('CREATE TABLE "authors"') < up.index('CREATE TABLE "books"')
    assert 'CONSTRAINT "books_author_id_fkey" FOREIGN KEY ("author_id") REFERENCES "authors" ("id") ON DELETE CASCADE' in up
    assert 'CREATE INDEX "books_author_id_idx" ON "books" ("author_id");' in up
    assert 'DROP TABLE "books";' in m.down_sql
    snap = json.loads(m.snapshot)
    assert [t["name"] for t in snap["tables"]] == ["authors", "books"]
    assert migs.make() is None  # nothing changed


def test_second_migration_alters_in_place(tmp_path):
    Migrations(tmp_path, v1()).make()
    migs = Migrations(tmp_path, v2())
    plan = migs.plan()
    sql = [s.sql for s in plan.up]
    assert sql[0] == 'CREATE EXTENSION IF NOT EXISTS "citext"'
    assert sql[1] == 'CREATE EXTENSION IF NOT EXISTS "pg_trgm"'
    assert sql[2].startswith('CREATE OR REPLACE FUNCTION "books_touch"() RETURNS trigger')
    assert 'ALTER TABLE "books" RENAME COLUMN "title" TO "name"' in sql
    assert 'ALTER TABLE "books" ADD COLUMN "updated_at" timestamp with time zone DEFAULT now() NOT NULL' in sql
    assert 'ALTER TABLE "authors" ALTER COLUMN "email" TYPE citext USING "email"::citext' in sql
    assert 'ALTER TABLE "books" ADD CONSTRAINT "books_pages_check" CHECK (pages >= 0)' in sql
    assert 'CREATE INDEX "books_name_idx" ON "books" USING gin ("name" gin_trgm_ops)' in sql
    assert sql[-2] == (
        'CREATE TRIGGER "touch" BEFORE UPDATE ON "books" FOR EACH ROW EXECUTE FUNCTION "books_touch"()'
    )
    assert sql[-1] == "COMMENT ON COLUMN \"books\".\"meta\" IS 'free-form attributes'"
    assert any("changes authors.email from varchar(254) to citext" in w for w in plan.warnings)
    # reverse: the rename is undone, the trigger function and extensions dropped
    down = [s.sql for s in plan.down]
    assert 'ALTER TABLE "books" RENAME COLUMN "name" TO "title"' in down
    assert down[-3:] == [
        'DROP FUNCTION "books_touch"()',
        'DROP EXTENSION IF EXISTS "citext"',
        'DROP EXTENSION IF EXISTS "pg_trgm"',
    ]

    m = migs.make("catalog changes")
    assert m is not None and m.name == "0002_catalog_changes"
    assert "-- WARNING: changes authors.email" in m.up_sql
    assert migs.make() is None


def test_table_rename_renames_generated_constraint_names(tmp_path):
    Migrations(tmp_path, v1()).make()
    reg = Registry()

    class Author(Model, table="writers", registry=reg):
        id = f.BigInt(primary_key=True, auto_increment=True)
        email = f.String(254, unique=True)

        class Meta:
            renamed_from = "authors"

    class Book(Model, table="books", registry=reg):
        id = f.BigInt(primary_key=True, auto_increment=True)
        author_id = f.BigInt(index=True)
        title = f.String(200)
        pages = f.Integer(default=0)
        author = f.BelongsTo("Author", via="author_id")

    up = [s.sql for s in Migrations(tmp_path, reg).plan().up]
    assert up == [
        'ALTER TABLE "authors" RENAME TO "writers"',
        'ALTER TABLE "writers" RENAME CONSTRAINT "authors_pkey" TO "writers_pkey"',
        'ALTER TABLE "writers" RENAME CONSTRAINT "authors_email_key" TO "writers_email_key"',
    ]


def test_extension_helpers_render_their_ddl():
    reg = Registry()
    reg.add(Extension("postgis", version="3.4"))

    class Place(Model, table="places", registry=reg):
        id = f.Uuid(primary_key=True, default=Sql("gen_random_uuid()"))
        room = f.Integer()
        starts_at = f.DateTime()
        ends_at = f.DateTime()
        location = postgis.Geography("Point", 4326)
        embedding = pgvector.Vector(3)

        class Meta:
            indexes = [
                postgis.SpatialIndex("location"),
                pgvector.HnswIndex("embedding", ops="vector_cosine_ops", m=16, ef_construction=64),
                Index(Key(Sql("lower(name)"), collation="C"), name="places_custom", method="btree"),
            ]
            constraints = [btree_gist.NoOverlap("room", start="starts_at", end="ends_at")]

    ddl = reg.native().ddl()
    assert ddl[:3] == [
        'CREATE EXTENSION IF NOT EXISTS "btree_gist"',
        "CREATE EXTENSION IF NOT EXISTS \"postgis\" VERSION '3.4'",
        'CREATE EXTENSION IF NOT EXISTS "vector"',
    ]
    table = ddl[3]
    assert '"id" uuid DEFAULT gen_random_uuid() NOT NULL' in table
    assert '"location" geography(Point, 4326) NOT NULL' in table
    assert '"embedding" vector(3) NOT NULL' in table
    assert 'EXCLUDE USING gist ("room" WITH =, (tstzrange("starts_at", "ends_at")) WITH &&)' in table
    assert ddl[4] == 'CREATE INDEX IF NOT EXISTS "places_location_idx" ON "places" USING gist ("location")'
    assert ddl[5] == (
        'CREATE INDEX IF NOT EXISTS "places_embedding_idx" ON "places" USING hnsw '
        '("embedding" vector_cosine_ops) WITH (m = 16, ef_construction = 64)'
    )
    assert ddl[6] == 'CREATE INDEX IF NOT EXISTS "places_custom" ON "places" ((lower(name)) COLLATE "C")'


def test_schema_level_functions_and_unknown_extensions(tmp_path):
    reg = Registry()
    audit = Function("audit_row", "BEGIN INSERT INTO audit_log VALUES (TG_TABLE_NAME); RETURN NULL; END;")
    reg.add(audit, Extension("acme", functions=("acme_slug",)))

    class Page(Model, table="pages", registry=reg):
        id = f.BigInt(primary_key=True, auto_increment=True)
        slug = f.Text(default=Sql("acme_slug()"))

        class Meta:
            triggers = [Trigger("audit", after=("insert", "delete"), for_each="statement", function=audit)]
            constraints = [Check("length(slug) > 0")]

    up = [s.sql for s in Migrations(tmp_path, reg).plan().up]
    assert up[0] == 'CREATE EXTENSION IF NOT EXISTS "acme"'
    assert up[1].startswith('CREATE OR REPLACE FUNCTION "audit_row"() RETURNS trigger LANGUAGE plpgsql AS $orm$')
    assert 'CONSTRAINT "pages_expr' in up[2] and "CHECK (length(slug) > 0)" in up[2]
    assert up[3] == (
        'CREATE TRIGGER "audit" AFTER INSERT OR DELETE ON "pages" FOR EACH STATEMENT EXECUTE FUNCTION "audit_row"()'
    )


def test_invalid_schema_objects_are_reported():
    reg = Registry()

    class T(Model, table="t", registry=reg):
        id = f.BigInt(primary_key=True)

        class Meta:
            constraints = [Unique("missing")]

    with pytest.raises(QueryError, match="missing"):
        reg.native().ddl()


def test_cli_makemigrations(tmp_path, capsys, monkeypatch):
    mod = tmp_path / "cli_models.py"
    mod.write_text(
        "from orm import Model, Registry, fields as f\n"
        "import orm.model\n"
        "class Tag(Model, table='cli_tags'):\n"
        "    id = f.BigInt(primary_key=True, auto_increment=True)\n"
        "    label = f.String(50, unique=True)\n"
    )
    monkeypatch.chdir(tmp_path)
    # the CLI uses the default registry; keep this test's model out of it afterwards
    before = dict(orm.registry._models)
    try:
        assert cli(["--models", "cli_models", "makemigrations", "--check"]) == 1
        assert cli(["--models", "cli_models", "makemigrations"]) == 0
        assert "Created migrations/0001_initial" in capsys.readouterr().out
        assert cli(["--models", "cli_models", "makemigrations", "--check"]) == 0
        assert cli(["sqlmigrate", "1"]) == 0
        assert 'CREATE TABLE "cli_tags"' in capsys.readouterr().out
    finally:
        orm.registry._models = before
        orm.registry._native = None
