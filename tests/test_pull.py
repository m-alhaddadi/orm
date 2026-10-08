"""`orm pull`, baseline and drift against a live database.

The Postgres tests run in a database of their own (``<test database>_pull``): they drop
and recreate its ``public`` schema.
"""

import os
import subprocess
import sys
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit

import pytest
from conftest import DATABASE_URL

import orm
from orm import Registry
from orm.migrations import Migrations, Migrator, pull

FIXTURE = Path(__file__).parent / "fixtures" / "pull_live.sql"
BLOG = Path(__file__).resolve().parents[1] / "examples" / "blog" / "schema.prisma"

_url = urlsplit(DATABASE_URL)
PULL_DB = _url.path.lstrip("/") + "_pull"
PULL_URL = urlunsplit(_url._replace(path="/" + PULL_DB))

# What the fixture holds that the schema language can't say (see the `gap:` lines).
EXPECTED_DIFFERENCES = [
    "drop table bundles_tag",
    "drop default of bundles_bundle.id",
    "make bundles_bundle.id an identity column",
    "drop column bundles_slot.during",
]


@pytest.fixture(scope="module", autouse=True)
async def pull_database():
    try:
        admin = await orm.connect(DATABASE_URL, max_connections=1, default=False, registry=Registry())
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {DATABASE_URL}: {e}")
    try:
        if not await admin._fetch_text(f"SELECT 1 FROM pg_database WHERE datname = '{PULL_DB}'"):
            await admin.execute(f'CREATE DATABASE "{PULL_DB}"')
    finally:
        await admin.close()


async def fresh(sql: str = "") -> orm.Database:
    db = await orm.connect(PULL_URL, max_connections=2, default=False, registry=Registry())
    await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public;" + sql)
    return db


async def test_pull_baseline_and_drift_of_a_django_style_database(tmp_path):
    db = await fresh(FIXTURE.read_text())
    try:
        pulled = await pull(db)
        text = pulled.schema
        # Django's names survive: foreign keys, uniques, like-indexes, checks.
        assert 'map: "bundles_bundle_shop_id_8f3e_fk_account_shop_id"' in text
        assert 'deferrable: deferred' in text
        assert '@@unique([domain], map: "account_shop_domain_9a1b2c3d_uniq")' in text
        assert 'ops: raw("varchar_pattern_ops")' in text
        assert "with: { fillfactor: 70 }" in text
        # a trigger with REFERENCING transition tables is left out, not written without them
        assert "@@trigger(account_shop_updated_at" not in text
        assert any("account_shop_updated_at has REFERENCING transition tables" in g for g in pulled.gaps)
        assert "function set_updated_at {" in text and "volatility = immutable" in text
        assert "status ShopStatus @default(active)" in text
        assert '@relation("BundlesBundle_parent"' in text
        assert "rank Int @default(-1)" in text
        gaps = "\n".join(pulled.gaps)
        for what in ["rule bundles_bundle_soft_delete", "view live_bundles", "composite primary key", "tstzrange", "uses a sequence"]:
            assert what in gaps
            assert what in text  # also listed at the end of the file
        assert [s.summary for s in pulled.differences] == EXPECTED_DIFFERENCES

        schema = tmp_path / "schema.prisma"
        pulled.write(schema)
        migrations = Migrations(tmp_path / "migrations", schema)
        migrator = Migrator(db, migrations)
        first = await migrator.baseline()
        assert first.name == "0001_initial"
        assert [(s.migration.name, s.applied) for s in await migrator.status()] == [("0001_initial", True)]
        assert await migrator.upgrade() == []
        assert not migrations.plan()

        drift = await migrator.drift()
        assert drift.migration == "0001_initial"
        assert [s.summary for s in drift.steps] == EXPECTED_DIFFERENCES
        assert any("rule bundles_bundle_soft_delete" in g for g in drift.gaps)

        await db.execute("ALTER TABLE account_shop ADD COLUMN extra integer; DROP INDEX account_shop_live_idx")
        drift = await migrator.drift()
        assert "drop column account_shop.extra" in [s.summary for s in drift.steps]
        assert "create index account_shop_live_idx on account_shop" in [s.summary for s in drift.steps]

        with pytest.raises(orm.migrations.MigrationError, match="already has applied migrations"):
            await migrator.baseline()
    finally:
        await db.close()


async def test_a_database_made_by_the_orm_pulls_back_without_differences(tmp_path):
    blog = orm.load(BLOG, registry=Registry())
    db = await fresh()
    try:
        migrations = Migrations(tmp_path / "blog", BLOG)
        migrations.make()
        assert await Migrator(db, migrations).upgrade()
        assert not await Migrator(db, migrations).drift()

        pulled = await pull(db)
        assert pulled.differences == []
        assert pulled.gaps == []
        assert "@@check(" in pulled.schema and "extensions = [pg_trgm]" in pulled.schema
        assert blog  # the blog models compile from the same file
    finally:
        await db.close()


SQLITE = """
datasource db {
  provider = "sqlite"
}

enum Color {
  red
  green
  @@storage(text)
}

model Owner {
  id    BigInt @id @default(autoincrement())
  email String @unique
  color Color  @default(red)
  pets  Pet[]
}

model Pet {
  id       BigInt @id @default(autoincrement())
  owner_id BigInt
  name     String
  age      Int    @default(0)
  owner    Owner  @relation(fields: [owner_id], references: [id], onDelete: Cascade)

  @@index([owner_id, -age])
  @@index([sql("lower(name)")], name: "pet_name_lower", where: raw("age > 0"))
  @@check("age >= 0", name: "pet_age")
}
"""


async def test_sqlite_pull_baseline_and_drift(tmp_path):
    source = tmp_path / "schema.prisma"
    source.write_text(SQLITE)
    url = f"sqlite://{tmp_path / 'live.db'}"
    registry = Registry()
    orm.load(source, registry=registry)
    db = await orm.connect(url, default=False, registry=registry)
    try:
        made = Migrations(tmp_path / "made", source)
        made.make()
        await Migrator(db, made).upgrade()

        pulled = await pull(db)
        assert pulled.differences == []
        assert 'provider = "sqlite"' in pulled.schema
        assert '@@check("age >= 0", name: "pet_age")' in pulled.schema

        schema = tmp_path / "pulled.prisma"
        pulled.write(schema)
        await db.execute("DROP TABLE orm_migrations")
        migrator = Migrator(db, Migrations(tmp_path / "migrations", schema))
        await migrator.baseline()
        assert not await migrator.drift()

        await db.execute("CREATE INDEX extra_idx ON pet (name)")
        assert [s.summary for s in (await migrator.drift()).steps] == ["drop index extra_idx"]
    finally:
        await db.close()


LOSSY = """
DROP SCHEMA IF EXISTS other CASCADE; CREATE SCHEMA other;
CREATE TABLE other.users (id uuid PRIMARY KEY);
CREATE TABLE users (id bigint PRIMARY KEY);
CREATE TABLE profiles (id bigint PRIMARY KEY, user_id uuid REFERENCES other.users, owner_id bigint REFERENCES users);
ALTER TABLE profiles ENABLE ROW LEVEL SECURITY;
CREATE TABLE booking (id bigint PRIMARY KEY, a int, b int, CONSTRAINT booking_a_uniq UNIQUE (a) INCLUDE (b));
CREATE UNLOGGED TABLE cache (id bigint PRIMARY KEY) WITH (fillfactor = 80);
CREATE TABLE a (id bigint PRIMARY KEY, n int CONSTRAINT positive CHECK (n > 0), p bigint CONSTRAINT fk_p REFERENCES users);
CREATE TABLE b (id bigint PRIMARY KEY, n int CONSTRAINT positive CHECK (n > 0), p bigint CONSTRAINT fk_p REFERENCES users);
CREATE TABLE t (id bigint PRIMARY KEY, c int CONSTRAINT t_c_key UNIQUE, d int CONSTRAINT t_d_key UNIQUE, "class" int,
    price numeric(10, 2) NOT NULL DEFAULT 0.00);
ALTER TABLE t ADD CONSTRAINT uq2 UNIQUE NULLS NOT DISTINCT (c);
CREATE INDEX t_d_idx ON t (d);
CREATE FUNCTION g() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN -- \"\"\"
RETURN NEW; END $$;
CREATE TRIGGER t_g BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION g();
"""


async def test_pull_reports_what_it_cannot_write(tmp_path):
    db = await fresh(LOSSY)
    try:
        pulled = await pull(db)
        text, gaps = pulled.schema, "\n".join(pulled.gaps)
        # a foreign key to another schema is left out, not pointed at the local table
        assert "references other.users, outside the schema; it is left out" in gaps
        assert "fields: [user_id]" not in text
        assert "table profiles has row-level security" in gaps
        assert "booking.booking_a_uniq has INCLUDE columns" in gaps
        assert "table cache is UNLOGGED" in gaps and "table cache has storage options (fillfactor=80)" in gaps
        # check and foreign-key names are unique per table only
        assert text.count('name: "positive")') == 2 and text.count('map: "fk_p")') == 2
        # the @unique shorthand keeps the other objects on its column
        assert 'map: "uq2", nulls_not_distinct: true' in text
        assert '@@index([d], name: "t_d_idx")' in text
        assert 'class_ Int? @map("class")' in text
        assert '@default(dbgenerated("0.00"))' in text
        # a trigger whose function is not written is left out with it
        assert "@@trigger(t_g" not in text and "trigger t.t_g calls g, which is not pulled" in gaps
        assert [s.summary for s in pulled.differences] == ["drop trigger t_g on t", "drop function g()"]

        pulled.write(tmp_path / "schema.prisma")
        made = Migrations(tmp_path / "migrations", tmp_path / "schema.prisma")
        made.make()
        other = await fresh()
        try:
            await Migrator(other, made).upgrade()
        finally:
            await other.close()
    finally:
        await db.close()


async def test_comments_cannot_escape_the_shadow(tmp_path):
    db = await fresh("CREATE TABLE victim (id bigint PRIMARY KEY); CREATE TABLE t (id bigint PRIMARY KEY);")
    await db.execute(f"ALTER DATABASE \"{PULL_DB}\" SET standard_conforming_strings = off")
    try:
        await db.execute("SET standard_conforming_strings = on; COMMENT ON TABLE t IS 'x\\''; COMMIT; DROP TABLE victim; --'")
        legacy = await orm.connect(PULL_URL, max_connections=1, default=False, registry=Registry())
        try:
            pulled = await pull(legacy)
        finally:
            await legacy.close()
        assert await db._fetch_text("SELECT to_regclass('victim') IS NOT NULL") == [("t",)]
        assert await db._fetch_text("SELECT count(*) FROM pg_namespace WHERE nspname LIKE 'orm_shadow%'") == [("0",)]
        assert pulled.differences == []
    finally:
        await db.execute(f"ALTER DATABASE \"{PULL_DB}\" RESET standard_conforming_strings")
        await db.close()


async def test_drift_after_an_enum_value_recreates_the_columns(tmp_path):
    db = await fresh("CREATE TYPE order_state AS ENUM ('new', 'paid'); CREATE TABLE orders (id bigint PRIMARY KEY, state order_state);")
    try:
        (await pull(db)).write(tmp_path / "schema.prisma")
        made = Migrations(tmp_path / "migrations", tmp_path / "schema.prisma")
        await Migrator(db, made).baseline()
        await db.execute("ALTER TYPE order_state ADD VALUE 'refunded'")
        drift = await Migrator(db, made).drift()
        sql = "\n".join(s.sql for s in drift.steps)
        assert 'ALTER COLUMN "state" TYPE' in sql, sql
    finally:
        await db.close()


async def test_pull_without_create_writes_the_schema(tmp_path):
    db = await fresh("CREATE TABLE t (id bigint PRIMARY KEY);")
    try:
        await db.execute(
            "DROP ROLE IF EXISTS orm_pull_reader; CREATE ROLE orm_pull_reader LOGIN PASSWORD 'r';"
            f"REVOKE CREATE ON DATABASE \"{PULL_DB}\" FROM PUBLIC; GRANT USAGE ON SCHEMA public TO orm_pull_reader;"
            "GRANT SELECT ON ALL TABLES IN SCHEMA public TO orm_pull_reader"
        )
        url = urlunsplit(_url._replace(path="/" + PULL_DB, netloc=f"orm_pull_reader:r@{_url.hostname}:{_url.port}"))
        reader = await orm.connect(url, max_connections=1, default=False, registry=Registry())
        try:
            pulled = await pull(reader)
        finally:
            await reader.close()
        assert "model T {" in pulled.schema
        assert any(g.startswith("the schema was not checked against the database") for g in pulled.gaps)

        # baseline marks the migration; a drift check it cannot run is a warning
        await db.execute("GRANT CREATE ON SCHEMA public TO orm_pull_reader")
        pulled.write(tmp_path / "schema.prisma")
        env = {**os.environ, "ORM_DATABASE_URL": url}
        done = subprocess.run(
            [sys.executable, "-m", "orm", "--schema", "schema.prisma", "--dir", "migrations", "baseline"],
            cwd=tmp_path, env=env, capture_output=True, text=True,
        )
        assert done.returncode == 0, done.stderr
        assert "warning: the database was not compared with 0001_initial" in done.stdout, done.stdout
    finally:
        await db.execute(
            f"GRANT CREATE ON DATABASE \"{PULL_DB}\" TO PUBLIC; DROP TABLE IF EXISTS orm_migrations;"
            "DROP OWNED BY orm_pull_reader; DROP ROLE orm_pull_reader"
        )
        await db.close()


DJANGO_SQLITE = """
CREATE TABLE "auth_user" ("id" integer NOT NULL PRIMARY KEY AUTOINCREMENT, "is_staff" bool NOT NULL,
    "joined" datetime NOT NULL, "age" integer unsigned NOT NULL CHECK ("age" >= 0), "price" decimal NOT NULL,
    CHECK (price > 0));
CREATE TABLE "app_item" ("id" integer NOT NULL PRIMARY KEY AUTOINCREMENT,
    "user_id" integer NOT NULL REFERENCES "auth_user" ("id") DEFERRABLE INITIALLY DEFERRED,
    "owner_id" integer NULL REFERENCES auth_user);
INSERT INTO auth_user (is_staff, joined, age, price) VALUES (1, '2024-01-02 03:04:05', 3, 1.5);
"""


async def test_sqlite_pull_reads_column_clauses(tmp_path):
    url = f"sqlite://{tmp_path / 'django.db'}"
    db = await orm.connect(url, default=False, registry=Registry(dialect="sqlite"))
    try:
        await db.execute(DJANGO_SQLITE)
        pulled = await pull(db)
    finally:
        await db.close()
    text = pulled.schema
    assert '@@check("\\"age\\" >= 0", name: "auth_user_age_check")' in text, text
    assert '@@check("price > 0", name: "auth_user_check")' in text
    assert "is_staff Boolean" in text and "joined DateTime" in text and "price Float" in text
    assert 'user AuthUser @relation("AppItem_user", fields: [user_id], references: [id], deferrable: deferred)' in text
    assert 'owner AuthUser? @relation("AppItem_owner", fields: [owner_id], references: [id])' in text

    # the pulled models read the rows another tool wrote
    registry = Registry()
    User = orm.loads(text, registry=registry)["AuthUser"]
    db = await orm.connect(url, default=False, registry=registry)
    try:
        user = await User.objects.using(db).get()
        assert user.is_staff is True and user.price == 1.5
    finally:
        await db.close()
