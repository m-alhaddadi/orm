"""Schema files and migration generation: no database needed."""

import json
from pathlib import Path

import pytest

import orm
from orm import Registry, SchemaError
from orm.__main__ import main as cli
from orm.migrations import Migrations

ROOT = Path(__file__).resolve().parent.parent

V1 = """
model Author {
  id    BigInt @id @default(autoincrement())
  email String @unique @db.VarChar(254)
  books Book[]

  @@map("authors")
}

model Book {
  id        BigInt @id @default(autoincrement())
  author_id BigInt
  title     String @db.VarChar(200)
  pages     Int    @default(0)
  author    Author @relation(fields: [author_id], references: [id], onDelete: Cascade)

  @@index([author_id])
  @@map("books")
}
"""

# v1 + an extension type, a renamed column, new columns, indexes, constraints, a trigger
V2 = r"""
model Author {
  id    BigInt @id @default(autoincrement())
  email String @unique @db.Citext
  books Book[]

  @@map("authors")
}

model Book {
  id         BigInt   @id @default(autoincrement())
  author_id  BigInt
  name       String   @renamed_from("title") @db.VarChar(200)
  pages      Int      @default(0) @check("pages >= 0")
  meta       Json?    @comment("free-form attributes")
  updated_at DateTime @default(now())
  author     Author   @relation(fields: [author_id], references: [id], onDelete: Cascade)

  @@index([author_id])
  @@index([author_id, updated_at(sort: Desc)], where: raw("pages > 0"))
  @@index([name(ops: raw("gin_trgm_ops"))], type: Gin)
  @@unique([author_id, name])
  @@map("books")
  @@trigger(touch, before: [update], body: "BEGIN\n    NEW.updated_at := now();\n    RETURN NEW;\nEND;")
}
"""


def models(source: str) -> Registry:
    reg = Registry()
    orm.loads(source, registry=reg)
    return reg


def test_loads_builds_models_from_schema_text():
    reg = models(V2)
    Book = reg.get("Book")
    assert Book._meta.table == "books"
    assert list(Book._meta.fields) == ["id", "author_id", "name", "pages", "meta", "updated_at"]
    assert Book._meta.fields["meta"].nullable
    assert Book._meta.fields["updated_at"].has_server_value
    assert Book._meta.relations["author"].target is reg.get("Author")
    # schema objects travel in the IR untouched by Python
    ir = Book._meta.ir()
    assert ir["triggers"][0]["body"].startswith("BEGIN\n    NEW.updated_at")
    assert ir["indexes"][1] == {"columns": [{"field": "name", "opclass": "gin_trgm_ops"}], "method": "gin"}
    email = reg.get("Author")._meta.ir()["fields"][1]
    assert email["db_type"] == "citext" and email["write_sql"] == "CAST({} AS citext)"


def test_schema_errors_name_the_line():
    with pytest.raises(SchemaError, match=r"<schema>:3:9: Book.title: unknown type Strin"):
        orm.loads("model Book {\n  id    BigInt @id\n  title Strin\n}")
    with pytest.raises(SchemaError, match="unknown argument `wher`"):
        orm.loads("model Book {\n  id BigInt @id\n  @@index([id], wher: \"x\")\n}")


def test_first_migration(tmp_path):
    migs = Migrations(tmp_path, models(V1))
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
    Migrations(tmp_path, models(V1)).make()
    migs = Migrations(tmp_path, models(V2))
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
    assert sql[-2] == 'CREATE TRIGGER "touch" BEFORE UPDATE ON "books" FOR EACH ROW EXECUTE FUNCTION "books_touch"()'
    assert sql[-1] == "COMMENT ON COLUMN \"books\".\"meta\" IS 'free-form attributes'"
    assert any("changes authors.email from varchar(254) to citext" in w for w in plan.warnings)
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
    Migrations(tmp_path, models(V1)).make()
    v1b = V1.replace('@@map("authors")', '@@map("writers")\n  @@renamed_from("authors")')
    up = [s.sql for s in Migrations(tmp_path, models(v1b)).plan().up]
    assert up == [
        'ALTER TABLE "authors" RENAME TO "writers"',
        'ALTER TABLE "writers" RENAME CONSTRAINT "authors_pkey" TO "writers_pkey"',
        'ALTER TABLE "writers" RENAME CONSTRAINT "authors_email_key" TO "writers_email_key"',
    ]


def test_migrations_from_a_schema_file(tmp_path):
    (tmp_path / "schema.prisma").write_text(V1)
    m = Migrations(tmp_path / "migrations", tmp_path / "schema.prisma").make()
    assert m is not None and (m.path / "snapshot.json").is_file()


def _without_pass_state(module: str) -> list[object]:
    """A composition artifact records its lowering passes in the embedded schema;
    the blog schema uses no extension, so the rest must equal the default output."""
    lines: list[object] = list(module.splitlines())
    at = lines.index('_SCHEMA = r"""') + 1
    schema = json.loads(str(lines[at]))
    assert not schema.pop("behavior", {}).get("declarations")
    lines[at] = schema
    return lines


def test_generated_blog_module_is_current():
    module, stub = orm._native.generate_python(str(ROOT / "examples/blog/schema.prisma"))
    committed = (ROOT / "examples/blog/models.py").read_text()
    if json.loads(orm._native.profile_metadata())["capabilities"].get("composition"):
        assert _without_pass_state(committed) == _without_pass_state(module), "run `python -m orm generate`"
    else:
        assert committed == module, "run `python -m orm generate`"
    assert (ROOT / "examples/blog/models.pyi").read_text() == stub, "run `python -m orm generate`"


def test_cli(tmp_path, capfd, monkeypatch):
    (tmp_path / "schema.prisma").write_text(V1)
    monkeypatch.chdir(tmp_path)
    assert cli(["check"]) == 0
    assert cli(["generate", "-o", "app/models.py"]) == 0
    assert "class Book(Model):" in (tmp_path / "app/models.pyi").read_text()
    assert cli(["makemigrations", "--check"]) == 1
    assert cli(["makemigrations"]) == 0
    assert "Created " in capfd.readouterr().out
    assert cli(["makemigrations", "--check"]) == 0
    assert cli(["sqlmigrate", "1"]) == 0
    assert 'CREATE TABLE "books"' in capfd.readouterr().out
    (tmp_path / "schema.prisma").write_text("model X {")
    assert cli(["check"]) == 1
    assert "schema.prisma:1:1: model X is not closed" in capfd.readouterr().err
