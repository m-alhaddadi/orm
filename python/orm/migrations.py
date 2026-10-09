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
TypeScript package run, so every tool writes and applies the same files. A migration
may also hold a data step (``data.py`` or ``data.ts``), which only its own host runs.

A live database under migrations: :func:`pull` writes its schema file, :meth:`Migrator.baseline`
marks the first migration as applied (it does not run it), and :meth:`Migrator.drift`
compares the database with the newest migration's snapshot.

From the command line: ``python -m orm makemigrations / migrate / rollback /
showmigrations / sqlmigrate / pull / baseline / drift`` (see ``python -m orm --help``).
"""

from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import inspect
import json
import os
import sys
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from . import _native
from .db import Database, _callbacks, _Callbacks, _current_tx
from .db import connect as orm_connect
from ._native import MigrationError
from .model import Registry

__all__ = ["Drift", "Migration", "Migrations", "Migrator", "MigrationError", "Plan", "Pulled", "Status", "Step", "pull"]


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
class Pulled:
    """A schema file read from a live database (:func:`pull`)."""

    schema: str
    #: What the schema leaves out (rules, views, unsupported types, ...), one line each.
    gaps: list[str]
    #: What a migration from ``schema`` would still change on the database; empty when
    #: the schema reproduces it.
    differences: list[Step]

    def write(self, path: str | os.PathLike[str]) -> None:
        Path(path).write_text(self.schema)


async def pull(db: Database) -> Pulled:
    """Reads the database ``db`` connects to (Postgres: its current schema) as a schema
    file, and checks the result by creating it again in a shadow schema that is rolled
    back (SQLite: an in-memory database)."""
    schema, gaps, steps = await db._engine.pull_schema()
    return Pulled(schema, gaps, [Step(*step) for step in steps])


@dataclass(frozen=True)
class Drift:
    """The live database against the newest migration's snapshot (:meth:`Migrator.drift`)."""

    #: The migration compared with, or None for an empty directory.
    migration: str | None
    #: Steps that bring the database to the snapshot; empty when they match.
    steps: list[Step]
    #: Live objects drift does not compare (rules, views, ...).
    gaps: list[str]

    def __bool__(self) -> bool:
        return bool(self.steps)


def _data_step(m: Migration) -> Callable[[Database], Awaitable[Any]] | None:
    """``run`` of the migration's ``data.py``, or None without one."""
    file = m.path / "data.py"
    if not file.is_file():
        return None
    # unique per file, so two directories with the same folder names do not share it
    name = f"orm_migration_{hashlib.sha256(os.fsencode(file.resolve())).hexdigest()[:12]}_{m.name}"
    spec = importlib.util.spec_from_file_location(name, file)
    if spec is None or spec.loader is None:
        raise MigrationError(f"{file}: can't be loaded")
    module = importlib.util.module_from_spec(spec)
    # dataclasses and pickle look the module up in sys.modules while it runs
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    finally:
        del sys.modules[name]
    run = getattr(module, "run", None)
    if not inspect.iscoroutinefunction(run):
        raise MigrationError(f"{file}: needs `async def run(db)`")
    return run


def has_data_steps(directory: str | os.PathLike[str]) -> bool:
    """Whether a migration of ``directory`` has a ``data.py``."""
    return any((Path(path) / "data.py").is_file() for _, path in _native.list_migrations(os.fspath(directory)))


async def migrate_command(schema: str | os.PathLike[str], directory: str | os.PathLike[str], url: str, target: str | None) -> int:
    """``python -m orm migrate`` for a directory with data steps: applies with
    :meth:`Migrator.upgrade`. The data modules of the pending migrations are imported
    first, so the models they import are in the default registry when the database
    connects."""
    from .model import registry, load

    dialect = "sqlite" if url.startswith("sqlite:") else None
    probe = await orm_connect(url, max_connections=1, default=False, registry=Registry(dialect=dialect))
    try:
        pending = await Migrator(probe, Migrations(directory, schema))._load_pending(target)
    finally:
        await probe.close()
    if not any(registry) and Path(schema).is_file():
        load(schema)
    if any(registry):
        db = await orm_connect(url, max_connections=2)
    else:
        # data steps that use db.execute only, and no schema file (e.g. a deploy image)
        db = await orm_connect(url, max_connections=2, registry=Registry(dialect=dialect))
    try:
        done = await Migrator(db, Migrations(directory, schema))._upgrade(target, pending)
    finally:
        await db.close()
    for m in done:
        print(f"Applied {m.name}")
    if not done:
        print("Nothing to apply.")
    return 0


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
        """Apply pending migrations up to and including ``target`` (default: all).

        A migration folder may hold a ``data.py`` with ``async def run(db)``. It runs after
        ``up.sql``, in the same transaction and under the same lock, before the migration
        is recorded; queries on ``db`` inside it go into that transaction. An error rolls
        back the SQL, the data changes and the record.
        """
        return await self._upgrade(target, await self._load_pending(target))

    async def _load_pending(self, target: str | None) -> list[tuple[Migration, Callable[[Database], Awaitable[Any]] | None]]:
        """The pending migrations with their loaded data steps; refuses before anything runs."""
        todo = [Migration(name, Path(path)) for name, path in await self.db._engine.migration_pending(self._dir, target)]
        for m in todo:
            if (m.path / "data.ts").is_file():
                raise MigrationError(f"{m.name} has a data step (data.ts); apply it with npx orm migrate")
        return [(m, _data_step(m)) for m in todo]

    async def _upgrade(
        self, target: str | None, pending: list[tuple[Migration, Callable[[Database], Awaitable[Any]] | None]]
    ) -> list[Migration]:
        done = []
        for m, run in pending:
            tx = await self.db._engine.migration_begin(m.name, str(m.path))
            if tx is None:
                continue
            root = self.db._root
            token = _current_tx.set({**_current_tx.get(), root: tx})
            callbacks = _Callbacks()
            cb_token = _callbacks.set({**_callbacks.get(), root: callbacks})
            try:
                if run is not None:
                    await run(self.db)
            except BaseException:
                with contextlib.suppress(Exception):
                    await tx.rollback()
                raise
            finally:
                callbacks.ended = True
                _callbacks.reset(cb_token)
                _current_tx.reset(token)
            await self.db._engine.migration_finish(tx, m.name, str(m.path))
            await self.db._after_commit(callbacks, True)
            done.append(m)
        return done

    async def drift(self) -> Drift:
        """Compares the database with the snapshot of the newest migration. The snapshot is
        created in a shadow (a Postgres schema in a transaction that is rolled back, or an
        in-memory SQLite database) and read back, so both sides use the database's text."""
        migration, steps, gaps = await self.db._engine.migration_drift(self._dir)
        return Drift(migration, [Step(*step) for step in steps], gaps)

    async def baseline(self) -> Migration:
        """Marks the first migration as applied and does not run it, for a database that
        already has the schema (after :func:`pull`). Writes the first migration from the
        schema when the directory has none. Fails once any migration is applied."""
        if not self.migrations.all():
            await self.status()  # fails first if the database has applied migrations
            self.migrations.make()
        name = await self.db._engine.migrate_baseline(self._dir)
        return Migration(name, self.migrations.directory / name)

    async def downgrade(self, steps: int = 1, *, target: str | None = None) -> list[Migration]:
        """Revert the last ``steps`` applied migrations, or every one after ``target``
        (``target="zero"`` reverts all)."""
        names = await self.db._engine.migrate_down(self._dir, max(steps, 0), target)
        return [Migration(n, self.migrations.directory / n) for n in names]
