"""Connections and transactions."""

from __future__ import annotations

import json
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from contextvars import ContextVar
from typing import Any

from . import _native
from .errors import NotConnected
from .model import Registry, registry

__all__ = ["Database", "connect", "get_database"]

_default: Database | None = None
# The innermost open transaction of the current task, with the database it belongs to.
_current_tx: ContextVar[tuple[Database, _native.Transaction] | None] = ContextVar(
    "orm_current_tx", default=None
)


class Database:
    """A connection pool. Created by :func:`connect`."""

    def __init__(self, engine: _native.Engine, url: str) -> None:
        self._engine = engine
        self.url = url

    def _tx(self) -> _native.Transaction | None:
        cur = _current_tx.get()
        return cur[1] if cur is not None and cur[0] is self else None

    async def _run(self, ir: dict[str, Any], params: list[Any]) -> Any:
        return await self._engine.run(json.dumps(ir), params, self._tx())

    async def _insert(
        self,
        model: str,
        fields: list[str],
        rows: list[list[Any]],
        conflict: list[str] | None = None,
        update: list[str] | None = None,
    ) -> list[tuple[Any, ...]]:
        return await self._engine.insert(model, fields, rows, conflict, update, self._tx())

    @asynccontextmanager
    async def transaction(self) -> AsyncIterator[None]:
        """``async with db.transaction():`` — commits on success, rolls back on error.

        Queries on this database inside the block (in this task, and in tasks it starts)
        run in the transaction. Nested blocks use savepoints.
        """
        tx = await self._engine.begin(self._tx())
        token = _current_tx.set((self, tx))
        try:
            yield
        except BaseException:
            _current_tx.reset(token)
            await tx.rollback()
            raise
        _current_tx.reset(token)
        await tx.commit()

    async def execute(self, sql: str) -> int:
        """Run raw SQL (one or more statements); returns the number of rows affected."""
        return await self._engine.execute(sql, self._tx())

    async def _fetch_text(self, sql: str) -> list[tuple[str | None, ...]]:
        return await self._engine.fetch_text(sql, self._tx())

    async def create_tables(self) -> None:
        """Create the whole schema (extensions, tables, constraints, indexes, functions,
        triggers) with ``IF NOT EXISTS`` / ``OR REPLACE`` DDL, in one transaction.

        A development and test helper: it never alters what exists. Use migrations
        (:mod:`orm.migrations`) for databases that evolve.
        """
        await self._engine.create_tables()

    async def drop_tables(self) -> None:
        """``DROP TABLE ... CASCADE`` every model table and drop generated functions;
        extensions stay."""
        await self._engine.drop_tables()

    async def close(self) -> None:
        global _default
        await self._engine.close()
        if _default is self:
            _default = None

    def __repr__(self) -> str:
        return f"<Database {self.url.split('@')[-1]}>"


async def connect(
    url: str, *, max_connections: int = 10, default: bool = True, registry: Registry = registry
) -> Database:
    """Open a connection pool. Import your model modules first: the schema is compiled
    from the models registered at this point (in ``registry``, the default one unless
    given).

    With ``default=True`` (the default) queries use this database unless
    ``.using(db)`` says otherwise.
    """
    global _default
    engine = await _native.connect(url, registry.native(), max_connections)
    db = Database(engine, url)
    if default:
        _default = db
    return db


def get_database() -> Database:
    if _default is None:
        raise NotConnected("no default database; call `await orm.connect(url)` first")
    return _default


def resolve(db: Database | None) -> Database:
    return db if db is not None else get_database()
