"""Migrations: generated from the schema file, stored as SQL, applied in order.

A migrations directory holds one folder per migration::

    migrations/
        0001_initial/
            up.sql          # forward DDL, with a comment per step (and warnings)
            down.sql        # reverse DDL
            snapshot.json   # the database schema after up.sql

Generating a migration (the Rust core: ``core/src/migrate``) diffs the schema against
the newest ``snapshot.json``, so it needs no database and gives the same files as the
standalone ``orm makemigrations``. The SQL is plain DDL and can be edited before it is
applied; :class:`Migrator` records applied migrations with a checksum in the
``orm_migrations`` table and refuses to continue if an applied file changed.

From the command line: ``python -m orm makemigrations / migrate / rollback /
showmigrations / sqlmigrate`` (see ``python -m orm --help``).
"""

from __future__ import annotations

import hashlib
import json
import os
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from . import _native
from .db import Database
from .errors import ORMError
from .model import Registry

__all__ = ["Migration", "Migrations", "Migrator", "MigrationError", "Step"]

TABLE = "orm_migrations"
# Arbitrary constant: the advisory lock serializing concurrent migrators.
LOCK_ID = 0x6F726D6D


class MigrationError(ORMError):
    pass


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


def _lit(text: str) -> str:
    return "'" + text.replace("'", "''") + "'"


Schema = Registry | _native.Schema | str | os.PathLike[str]


def _native_schema(schema: Schema) -> _native.Schema:
    if isinstance(schema, _native.Schema):
        return schema
    if isinstance(schema, Registry):
        return schema.native()
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
        if not self.directory.is_dir():
            return []
        found = [
            Migration(p.name, p)
            for p in sorted(self.directory.iterdir())
            if p.is_dir() and (p / "up.sql").is_file() and re.match(r"\d{4}_", p.name)
        ]
        numbers = [m.name[:4] for m in found]
        if len(set(numbers)) != len(numbers):
            dupes = sorted({n for n in numbers if numbers.count(n) > 1})
            raise MigrationError(f"several migrations share the number(s) {', '.join(dupes)}; renumber them")
        return found

    def get(self, name: str) -> Migration:
        for m in self.all():
            if m.name == name or m.name[:4] == name.zfill(4):
                return m
        raise MigrationError(f"no migration {name!r} in {self.directory}")

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
    """Applies and reverts the migrations of a directory on a database."""

    def __init__(self, db: Database, migrations: Migrations) -> None:
        self.db = db
        self.migrations = migrations

    async def _ensure_table(self) -> None:
        await self.db.execute(
            f"CREATE TABLE IF NOT EXISTS {TABLE} ("
            " name text PRIMARY KEY,"
            " checksum text NOT NULL,"
            " applied_at timestamptz NOT NULL DEFAULT now())"
        )

    async def _applied(self) -> dict[str, tuple[str, str]]:
        rows = await self.db._fetch_text(f"SELECT name, checksum, applied_at::text FROM {TABLE} ORDER BY name")
        return {r[0]: (r[1], r[2]) for r in rows}  # type: ignore[misc]

    async def _locked(self) -> None:
        await self.db.execute(f"SELECT pg_advisory_xact_lock({LOCK_ID})")

    async def status(self) -> list[Status]:
        await self._ensure_table()
        applied = await self._applied()
        out = [Status(m, m.name in applied, applied.get(m.name, (None, None))[1]) for m in self.migrations.all()]
        unknown = set(applied) - {s.migration.name for s in out}
        if unknown:
            raise MigrationError(f"the database has migrations missing from the directory: {', '.join(sorted(unknown))}")
        return out

    def _verify(self, statuses: list[Status], applied: dict[str, tuple[str, str]]) -> None:
        seen_pending = None
        for s in statuses:
            if not s.applied:
                seen_pending = seen_pending or s.migration.name
            elif seen_pending:
                raise MigrationError(
                    f"{s.migration.name} is applied but the earlier {seen_pending} is not; "
                    "renumber the unapplied migration after the applied ones"
                )
            elif applied[s.migration.name][0] != s.migration.checksum:
                raise MigrationError(f"{s.migration.name}/up.sql changed after it was applied")

    async def upgrade(self, target: str | None = None) -> list[Migration]:
        """Apply pending migrations up to and including ``target`` (default: all).

        Each migration runs in its own transaction together with its bookkeeping row, so
        a failing migration leaves the database at the previous one.
        """
        statuses = await self.status()
        self._verify(statuses, await self._applied())
        pending = [s.migration for s in statuses if not s.applied]
        if target is not None:
            stop = self.migrations.get(target).name
            pending = [m for m in pending if m.name <= stop]
        done = []
        for m in pending:
            async with self.db.transaction():
                await self._locked()
                if m.name in await self._applied():  # applied concurrently
                    continue
                await self.db.execute(m.up_sql)
                await self.db.execute(
                    f"INSERT INTO {TABLE} (name, checksum) VALUES ({_lit(m.name)}, {_lit(m.checksum)})"
                )
            done.append(m)
        return done

    async def downgrade(self, steps: int = 1, *, target: str | None = None) -> list[Migration]:
        """Revert the last ``steps`` applied migrations, or every one after ``target``
        (``target="zero"`` reverts all)."""
        statuses = await self.status()
        applied = [s.migration for s in statuses if s.applied]
        if target is not None:
            keep = "" if target == "zero" else self.migrations.get(target).name
            revert = [m for m in applied if m.name > keep]
        else:
            revert = applied[-steps:] if steps > 0 else []
        done = []
        for m in reversed(revert):
            async with self.db.transaction():
                await self._locked()
                await self.db.execute(m.down_sql)
                await self.db.execute(f"DELETE FROM {TABLE} WHERE name = {_lit(m.name)}")
            done.append(m)
        return done
