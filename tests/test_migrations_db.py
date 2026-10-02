"""Migrations and extension types against Postgres.

These run in a database of their own: migrations create and drop extensions, which
are per database, and the blog tables of the other tests depend on some of them.
"""

import uuid
from urllib.parse import urlsplit, urlunsplit

import pytest
from conftest import DATABASE_URL
from test_migrations import V1, V2, models

import orm
from orm import Registry
from orm.migrations import MigrationError, Migrations, Migrator


def v1() -> Registry:
    return models(V1)


def v2() -> Registry:
    return models(V2)


_url = urlsplit(DATABASE_URL)
MIGRATIONS_DB = _url.path.lstrip("/") + "_migrations"
MIGRATIONS_URL = urlunsplit(_url._replace(path="/" + MIGRATIONS_DB))


@pytest.fixture(scope="module", autouse=True)
async def migrations_database():
    """Creates the database these tests use, once."""
    try:
        admin = await orm.connect(DATABASE_URL, max_connections=1, default=False, registry=Registry())
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {DATABASE_URL}: {e}")
    try:
        if not await admin._fetch_text(f"SELECT 1 FROM pg_database WHERE datname = '{MIGRATIONS_DB}'"):
            await admin.execute(f'CREATE DATABASE "{MIGRATIONS_DB}"')
    finally:
        await admin.close()


async def connect(reg: Registry) -> orm.Database:
    try:
        return await orm.connect(MIGRATIONS_URL, max_connections=2, default=False, registry=reg)
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {MIGRATIONS_URL}: {e}")


async def reset(db: orm.Database) -> None:
    await db.execute(
        "DROP TABLE IF EXISTS books, authors, orm_migrations CASCADE;"
        "DROP FUNCTION IF EXISTS books_touch() CASCADE"
    )


async def scalar(db: orm.Database, sql: str) -> str | None:
    rows = await db._fetch_text(sql)
    return rows[0][0] if rows else None


async def test_upgrade_downgrade_round_trip(tmp_path):
    reg1, reg2 = v1(), v2()
    db = await connect(reg2)
    await reset(db)
    try:
        Migrations(tmp_path, reg1).make()
        migrator = Migrator(db, Migrations(tmp_path, reg2))
        assert [m.name for m in await migrator.upgrade()] == ["0001_initial"]
        await db.execute("INSERT INTO authors (email) VALUES ('Ann@Example.com');"
                         "INSERT INTO books (author_id, title) VALUES (1, 'Dune')")

        Migrations(tmp_path, reg2).make("v2")
        assert [s.applied for s in await migrator.status()] == [True, False]
        await migrator.upgrade()

        Author, Book = reg2.get("Author"), reg2.get("Book")
        # renamed column kept its data; citext compares case-insensitively
        book = await Book.objects.using(db).get(Book.name == "Dune")
        assert await Author.objects.using(db).filter(Author.email == "ann@example.COM").count() == 1
        # trigger
        before = book.updated_at
        await book.update(pages=10)
        assert book.updated_at > before
        # constraints raise IntegrityError
        with pytest.raises(orm.IntegrityError):
            await book.update(pages=-1)
        with pytest.raises(orm.IntegrityError):
            await Author.objects.using(db).insert(email="ANN@example.com")
        with pytest.raises(orm.IntegrityError):
            await Book.objects.using(db).insert(author_id=1, name="Dune")

        # back to zero and up again: the down migrations are valid DDL too
        assert [m.name for m in await migrator.downgrade(target="zero")] == ["0002_v2", "0001_initial"]
        assert await scalar(db, "SELECT to_regclass('books')::text") is None
        assert len(await migrator.upgrade()) == 2
        assert await migrator.upgrade() == []
        assert [m.name for m in await migrator.downgrade()] == ["0002_v2"]
        assert await scalar(db, "SELECT data_type::text FROM information_schema.columns "
                                "WHERE table_name = 'books' AND column_name = 'title'") == "character varying"
    finally:
        await reset(db)
        await db.close()


async def test_edited_applied_migration_is_refused(tmp_path):
    db = await connect(v1())
    await reset(db)
    try:
        migs = Migrations(tmp_path, v1())
        m = migs.make()
        migrator = Migrator(db, migs)
        await migrator.upgrade()
        (m.path / "up.sql").write_text(m.up_sql + "\n-- edited\n")
        Migrations(tmp_path, v2()).make()
        with pytest.raises(MigrationError, match="changed after it was applied"):
            await migrator.upgrade()
    finally:
        await reset(db)
        await db.close()


async def test_failed_migration_rolls_back(tmp_path):
    db = await connect(v1())
    await reset(db)
    try:
        migs = Migrations(tmp_path, v1())
        m = migs.make()
        (m.path / "up.sql").write_text(m.up_sql + "\nSELECT no_such_function();\n")
        with pytest.raises(orm.DatabaseError):
            await Migrator(db, migs).upgrade()
        assert await scalar(db, "SELECT to_regclass('authors')::text") is None
        assert [s.applied for s in await Migrator(db, migs).status()] == [False]
    finally:
        await reset(db)
        await db.close()


async def test_extension_types_at_runtime():
    reg = models("""
        model Doc {
          id        String                    @id @default(dbgenerated("gen_random_uuid()")) @db.Uuid
          email     String                    @db.Citext
          attrs     Json?
          embedding Unsupported("vector(3)")?

          @@map("ext_docs")
        }
    """)
    Doc = reg.get("Doc")

    db = await connect(reg)
    try:
        await db.execute("DROP TABLE IF EXISTS ext_docs")
        try:
            await db.create_tables()
        except orm.DatabaseError as e:
            pytest.skip(f"extension not installed: {e}")
        await db.create_tables()  # idempotent
        d = await Doc.objects.using(db).insert(
            email="Bob@X.io", attrs={"tags": ["a", "b"], "n": 1, "ok": True, "x": None}, embedding=[1, 2.5, 3]
        )
        assert isinstance(d.id, uuid.UUID)
        assert d.attrs == {"tags": ["a", "b"], "n": 1, "ok": True, "x": None}
        assert d.embedding == [1, 2.5, 3]
        got = await Doc.objects.using(db).get(Doc.email == "bob@x.IO")
        assert got == d and got.embedding == [1, 2.5, 3]
        assert await Doc.objects.using(db).filter(Doc.id == str(d.id)).exists()
        assert await Doc.objects.using(db).filter(Doc.email.in_(["BOB@x.io"])).count() == 1
        await got.update(embedding=[0, 0, 1], attrs=[1, 2])
        assert got.embedding == [0, 0, 1] and got.attrs == [1, 2]
        await Doc.objects.using(db).insert(email="no@x.io")
        assert await Doc.objects.using(db).filter(Doc.embedding == None).count() == 1  # noqa: E711
    finally:
        await db.execute("DROP TABLE IF EXISTS ext_docs")
        await db.close()
