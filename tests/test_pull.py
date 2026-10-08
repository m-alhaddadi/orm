"""`orm pull`, baseline and drift against a live database.

The Postgres tests run in a database of their own (``<test database>_pull``): they drop
and recreate its ``public`` schema.
"""

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
        assert "@@trigger(account_shop_updated_at, after: [update], for_each: statement" in text
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
