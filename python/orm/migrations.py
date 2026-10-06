"""Migrations: generated from the schema file, stored as SQL, applied in order.

A migrations directory holds one folder per migration::

    migrations/
        0001_initial/
            up.sql          # forward DDL, with a comment per step (and warnings)
            down.sql        # reverse DDL
            snapshot.json   # the database schema after up.sql

Generating a migration (the Rust core: ``core/src/migrate``) diffs the schema against
the newest ``snapshot.json``, so it needs no database. The SQL is plain DDL and can be
edited before it is applied. Applying and reverting (``engine/src/migrate.rs``) records
each migration with a checksum in the ``orm_migrations`` table and refuses to continue
if an applied file changed. Both are the same Rust code the ``orm`` command line and the
TypeScript package run, so every tool writes and applies the same files.

From the command line: ``python -m orm makemigrations / migrate / rollback /
showmigrations / sqlmigrate`` (see ``python -m orm --help``).
"""

from __future__ import annotations

import hashlib
import json
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from . import _native
from .db import Database
from ._native import MigrationError
from .model import Registry

__all__ = ["Migration", "Migrations", "Migrator", "MigrationError", "Plan", "Status", "Step"]


@dataclass(frozen=True)
class Step:
    summary: str
    sql: str
    warning: str | None = None


@dataclass(frozen=True)
class Plan:
    up: list[Step]
    down: list[Step]
    snapshot: dict[str, Any]

    @property
    def warnings(self) -> list[str]:
        return [s.warning for s in self.up if s.warning]

    def __bool__(self) -> bool:
        return bool(self.up)


@dataclass(frozen=True)
class Migration:
    name: str
    path: Path

    @property
    def up_sql(self) -> str:
        return (self.path / "up.sql").read_text()

    @property
    def down_sql(self) -> str:
        return (self.path / "down.sql").read_text()

    @property
    def snapshot(self) -> str:
        return (self.path / "snapshot.json").read_text()

    @property
    def checksum(self) -> str:
        return hashlib.sha256(self.up_sql.encode()).hexdigest()

    def __str__(self) -> str:
        return self.name


Schema = Registry | _native.Schema | str | os.PathLike[str]


def _native_schema(schema: Schema) -> _native.Schema:
    if isinstance(schema, _native.Schema):
        return schema
    if isinstance(schema, Registry):
        return schema.prepare()
    return _native.Schema(_native.compile_schema_file(os.fspath(schema)))


class Migrations:
    """A migrations directory and the schema it migrates to: a schema file path, a
    :class:`~orm.Registry` (e.g. of models from ``orm.load``), or a compiled schema."""

    def __init__(self, directory: str | os.PathLike[str], schema: Schema) -> None:
        self.directory = Path(directory)
        self._schema = schema

    @property
    def schema(self) -> _native.Schema:
        return _native_schema(self._schema)

    def all(self) -> list[Migration]:
        return [Migration(name, path) for name, path in _native.list_migrations(str(self.directory))]

    def get(self, name: str) -> Migration:
        """A migration by folder name or number (``"3"``, ``"0003_add_tags"``)."""
        found, path = _native.find_migration(str(self.directory), name)
        return Migration(found, path)

    def plan(self) -> Plan:
        """What the next migration would contain (empty if the schema is unchanged)."""
        raw = json.loads(self.schema.plan_migration(str(self.directory)))
        return Plan(
            up=[Step(**s) for s in raw["up"]],
            down=[Step(**s) for s in raw["down"]],
            snapshot=raw["snapshot"],
        )

    def make(self, name: str | None = None, *, empty: bool = False) -> Migration | None:
        """Write the next migration; returns None if there is nothing to migrate.

        ``empty=True`` writes one even without schema changes (for data migrations or
        hand-written SQL); its snapshot is the current schema.
        """
        folder = self.schema.make_migration(str(self.directory), name, empty)
        return None if folder is None else Migration(folder, self.directory / folder)


@dataclass(frozen=True)
class Status:
    migration: Migration
    applied: bool
    applied_at: str | None


class Migrator:
    """Applies and reverts the migrations of a directory on a database.

    Each migration runs in its own transaction (on a connection of the pool, not in
    the caller's transaction) together with its ``orm_migrations`` row, under an
    advisory lock: a failing migration leaves the database at the previous one, and
    concurrent migrators apply each migration once.
    """

    def __init__(self, db: Database, migrations: Migrations) -> None:
        self.db = db
        self.migrations = migrations

    @property
    def _dir(self) -> str:
        return str(self.migrations.directory)

    async def status(self) -> list[Status]:
        rows = await self.db._engine.migration_status(self._dir)
        return [Status(Migration(name, self.migrations.directory / name), applied, at) for name, applied, at in rows]

    async def upgrade(self, target: str | None = None) -> list[Migration]:
        """Apply pending migrations up to and including ``target`` (default: all)."""
        names = await self.db._engine.migrate_up(self._dir, target)
        return [Migration(n, self.migrations.directory / n) for n in names]

    async def downgrade(self, steps: int = 1, *, target: str | None = None) -> list[Migration]:
        """Revert the last ``steps`` applied migrations, or every one after ``target``
        (``target="zero"`` reverts all)."""
        names = await self.db._engine.migrate_down(self._dir, max(steps, 0), target)
        return [Migration(n, self.migrations.directory / n) for n in names]
