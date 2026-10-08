"""Data migrations: a ``data.py`` with ``async def run(db)`` next to ``up.sql``."""

import asyncio
import os
import subprocess
import sys
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit

import pytest
from conftest import DATABASE_URL

import orm
from orm import Registry
from orm.migrations import MigrationError, Migrations, Migrator

_url = urlsplit(DATABASE_URL)
DATA_DB = _url.path.lstrip("/") + "_data"
DATA_URL = urlunsplit(_url._replace(path="/" + DATA_DB))

V1 = """
model Author {
  id   BigInt @id @default(autoincrement())
  name String
}
"""

V2 = """
model Author {
  id   BigInt @id @default(autoincrement())
  name String
  slug String @default("")
}
"""

FILL = """
async def run(db):
    # the new column is visible only inside the migration's transaction
    await db.execute("UPDATE author SET slug = lower(name)")
    rows = await db._fetch_text("SELECT count(*) FROM author WHERE slug <> ''")
    assert rows == [("2",)], rows
"""

FAIL = """
async def run(db):
    await db.execute("INSERT INTO extra (id) VALUES (1)")
    raise RuntimeError("data step failed")
"""


@pytest.fixture(scope="module", autouse=True)
async def data_database():
    try:
        admin = await orm.connect(DATABASE_URL, max_connections=1, default=False, registry=Registry())
    except orm.DatabaseError as e:
        pytest.skip(f"Postgres not reachable at {DATABASE_URL}: {e}")
    try:
        if not await admin._fetch_text(f"SELECT 1 FROM pg_database WHERE datname = '{DATA_DB}'"):
            await admin.execute(f'CREATE DATABASE "{DATA_DB}"')
    finally:
        await admin.close()


def write_versions(tmp_path: Path) -> Migrations:
    schema = tmp_path / "schema.prisma"
    migrations = Migrations(tmp_path / "migrations", schema)
    schema.write_text(V1)
    migrations.make()
    schema.write_text(V2)
    second = migrations.make("slug")
    assert second is not None
    (second.path / "data.py").write_text(FILL)
    return migrations


async def test_data_step_runs_in_the_migration_transaction(tmp_path):
    migrations = write_versions(tmp_path)
    db = await orm.connect(DATA_URL, max_connections=2, default=False, registry=Registry())
    try:
        await db.execute("DROP TABLE IF EXISTS author, extra, orm_migrations CASCADE")
        migrator = Migrator(db, migrations)
        assert [m.name for m in await migrator.upgrade("1")] == ["0001_initial"]
        await db.execute("INSERT INTO author (name) VALUES ('Ann'), ('Bob')")

        # the engine's own runner (and the standalone binary) refuses a data step
        with pytest.raises(MigrationError, match=r"0002_slug has a data step \(data.py\)"):
            await db._engine.migrate_up(str(migrations.directory), None)

        assert [m.name for m in await migrator.upgrade()] == ["0002_slug"]
        assert await db._fetch_text("SELECT slug FROM author ORDER BY id") == [("ann",), ("bob",)]

        # a failing data step rolls back its SQL and is not recorded
        schema = tmp_path / "schema.prisma"
        schema.write_text(V2 + "\nmodel Extra {\n  id BigInt @id\n}\n")
        third = migrations.make("extra")
        assert third is not None
        (third.path / "data.py").write_text(FAIL)
        with pytest.raises(RuntimeError, match="data step failed"):
            await migrator.upgrade()
        assert await db._fetch_text("SELECT to_regclass('extra')") == [(None,)]
        assert [s.applied for s in await migrator.status()] == [True, True, False]

        (third.path / "data.py").write_text("def run(db):\n    pass\n")
        with pytest.raises(MigrationError, match="needs `async def run"):
            await migrator.upgrade()
    finally:
        await db.close()


SQLITE = 'datasource db {\n  provider = "sqlite"\n}\n'

SEED = """
async def run(db):
    await db.execute("INSERT INTO author (name, slug) VALUES ('Ann', '')")
    await db.execute("UPDATE author SET slug = lower(name)")
"""


def test_python_m_orm_migrate_runs_data_steps(tmp_path):
    schema = tmp_path / "schema.prisma"
    migrations = Migrations(tmp_path / "migrations", schema)
    schema.write_text(SQLITE + V1)
    migrations.make()
    schema.write_text(SQLITE + V2)
    second = migrations.make("slug")
    assert second is not None
    (second.path / "data.py").write_text(SEED)

    env = {**os.environ, "ORM_DATABASE_URL": f"sqlite://{tmp_path / 'app.db'}"}
    base = [sys.executable, "-m", "orm", "--schema", "schema.prisma", "--dir", "migrations"]

    def orm_cli(*args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run([*base, *args], cwd=tmp_path, env=env, capture_output=True, text=True)

    done = orm_cli("migrate")
    assert done.returncode == 0, done.stderr
    assert done.stdout.splitlines() == ["Applied 0001_initial", "Applied 0002_slug"]
    shown = orm_cli("showmigrations").stdout
    assert "[x] 0001_initial" in shown and "[x] 0002_slug" in shown

    (tmp_path / "migrations" / "0003_bad").mkdir()
    (tmp_path / "migrations" / "0003_bad" / "up.sql").write_text("SELECT 1;\n")
    (tmp_path / "migrations" / "0003_bad" / "data.py").write_text("async def run(db):\n    raise ValueError('bad data')\n")
    failed = orm_cli("migrate")
    assert failed.returncode == 1
    assert "error: bad data" in failed.stderr


NESTED = """
import orm


async def run(db):
    async def after():
        # another connection sees the row only after the commit
        other = await orm.connect(db.url, max_connections=1, default=False, registry=orm.Registry())
        try:
            await other.execute("INSERT INTO seen (n) SELECT count(*) FROM author")
        finally:
            await other.close()

    async with db.transaction():
        await db.execute("INSERT INTO author (name) VALUES ('Cy')")
    await db.on_commit(after)
"""

ROLLED_BACK = """
async def run(db):
    async def after():
        raise AssertionError("on_commit ran for a rolled-back migration")

    await db.on_commit(after)
    async with db.transaction():
        await db.execute("UPDATE author SET name = name")
    raise RuntimeError("after on_commit")
"""

DATACLASS = """
from __future__ import annotations

from dataclasses import dataclass


@dataclass
class Row:
    name: str


async def run(db):
    await db.execute(f"INSERT INTO author (name) VALUES ('{Row('Dee').name}')")
"""


async def fresh(tmp_path: Path) -> tuple[orm.Database, Migrations]:
    schema = tmp_path / "schema.prisma"
    migrations = Migrations(tmp_path / "migrations", schema)
    schema.write_text(V1)
    migrations.make()
    db = await orm.connect(DATA_URL, max_connections=3, default=False, registry=Registry())
    await db.execute("DROP TABLE IF EXISTS author, extra, seen, orm_migrations CASCADE; CREATE TABLE seen (n bigint)")
    return db, migrations


def data_migration(migrations: Migrations, name: str, file: str, text: str, sql: str = "SELECT 1;\n") -> Path:
    folder = migrations.directory / name
    folder.mkdir()
    (folder / "up.sql").write_text(sql)
    (folder / file).write_text(text)
    return folder


async def test_data_step_transactions_and_on_commit(tmp_path):
    db, migrations = await fresh(tmp_path)
    try:
        data_migration(migrations, "0002_nested", "data.py", NESTED)
        assert [m.name for m in await Migrator(db, migrations).upgrade()] == ["0001_initial", "0002_nested"]
        assert await db._fetch_text("SELECT name FROM author") == [("Cy",)]
        assert await db._fetch_text("SELECT n FROM seen") == [("1",)]

        # a failing data step drops its on_commit callbacks and releases the migration lock
        data_migration(migrations, "0003_rolled_back", "data.py", ROLLED_BACK)
        for _ in range(2):
            with pytest.raises(RuntimeError, match="after on_commit"):
                await asyncio.wait_for(Migrator(db, migrations).upgrade(), 10)
        assert [s.applied for s in await Migrator(db, migrations).status()] == [True, True, False]
    finally:
        await db.close()


async def test_data_steps_are_checked_before_anything_runs(tmp_path):
    db, migrations = await fresh(tmp_path)
    try:
        data_migration(migrations, "0002_ts", "data.ts", "export async function run(db) {}\n")
        with pytest.raises(MigrationError, match=r"0002_ts has a data step \(data.ts\)"):
            await Migrator(db, migrations).upgrade()
        assert await db._fetch_text("SELECT to_regclass('author')") == [(None,)]
    finally:
        await db.close()


async def test_data_module_is_a_real_module(tmp_path):
    db, migrations = await fresh(tmp_path)
    try:
        data_migration(migrations, "0002_dataclass", "data.py", DATACLASS)
        await Migrator(db, migrations).upgrade()
        assert await db._fetch_text("SELECT name FROM author") == [("Dee",)]
    finally:
        await db.close()


SLOW = """
import asyncio


async def run(db):
    await db.execute("INSERT INTO author (name) VALUES ('once')")
    await asyncio.sleep(0.5)
"""


async def test_concurrent_upgrades_run_a_data_step_once(tmp_path):
    db, migrations = await fresh(tmp_path)
    try:
        await Migrator(db, migrations).upgrade()
        data_migration(migrations, "0002_slow", "data.py", SLOW)
        results = await asyncio.gather(Migrator(db, migrations).upgrade(), Migrator(db, migrations).upgrade())
        assert sorted([m.name for m in r] for r in results) == [[], ["0002_slow"]]
        assert await db._fetch_text("SELECT count(*) FROM author") == [("1",)]
    finally:
        await db.close()


def test_python_m_orm_migrate_needs_no_schema_and_skips_applied_steps(tmp_path):
    schema = tmp_path / "schema.prisma"
    migrations = Migrations(tmp_path / "migrations", schema)
    schema.write_text(SQLITE + V1)
    migrations.make()
    # an applied data step whose import fails today must not stop later migrations
    data_migration(migrations, "0002_old", "data.py", "import gone_module\n\nasync def run(db):\n    pass\n")
    env = {**os.environ, "ORM_DATABASE_URL": f"sqlite://{tmp_path / 'app.db'}"}
    base = [sys.executable, "-m", "orm", "--schema", "schema.prisma", "--dir", "migrations"]
    (tmp_path / "migrations" / "0002_old" / "data.py").write_text("async def run(db):\n    pass\n")
    assert subprocess.run([*base, "migrate"], cwd=tmp_path, env=env, capture_output=True, text=True).returncode == 0
    (tmp_path / "migrations" / "0002_old" / "data.py").write_text("import gone_module\n\nasync def run(db):\n    pass\n")
    data_migration(
        migrations, "0003_new", "data.py", "async def run(db):\n    await db.execute(\"INSERT INTO author (name) VALUES ('x')\")\n"
    )
    schema.unlink()
    done = subprocess.run([*base, "migrate"], cwd=tmp_path, env=env, capture_output=True, text=True)
    assert done.returncode == 0, done.stderr
    assert done.stdout.splitlines() == ["Applied 0003_new"]
