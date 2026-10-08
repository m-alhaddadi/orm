"""Data migrations: a ``data.py`` with ``async def run(db)`` next to ``up.sql``."""

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
