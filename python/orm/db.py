"""Connections and transactions."""

from __future__ import annotations

import json
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from contextvars import ContextVar
from typing import Any

from . import _native
from .errors import NotConnected, QueryError, TransactionRequired
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

    async def _run(
        self, ir: dict[str, Any], params: list[Any], row_cls: type | None = None, db: Database | None = None
    ) -> Any:
        """Runs a query; instances it builds get ``db`` to write back to (``using()``)."""
        return await self._engine.run(json.dumps(ir), params, self._tx(), row_cls, db)

    async def _insert(
        self,
        model: str,
        fields: list[str],
        rows: list[list[Any]],
        conflict: list[str] | None = None,
        update: list[str] | None = None,
        set_: tuple[list[dict[str, Any]], list[Any]] | None = None,
        db: Database | None = None,
    ) -> list[Any]:
        set_json, params = (json.dumps(set_[0]), set_[1]) if set_ is not None else (None, [])
        return await self._engine.insert(model, fields, rows, conflict, update, set_json, params, self._tx(), db)

    async def _update_many(
        self,
        model: str,
        fields: list[str],
        rows: list[list[Any]],
        filters: list[dict[str, Any]],
        params: list[Any],
        returning: bool,
        batch_size: int | None,
        db: Database | None = None,
    ) -> Any:
        return await self._engine.update_many(
            model, fields, rows, json.dumps(filters), params, returning, batch_size, self._tx(), db
        )

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

    async def lock(self, key: int | str, exclusive: bool = True, *, nowait: bool = False) -> bool:
        """Take an advisory lock on ``key`` until the transaction ends: a lock on a
        name rather than on rows, e.g. "only one worker imports this file at a time".

        ``exclusive=False`` takes a shared lock (any number of shared holders, but no
        exclusive one). Waits for the lock unless ``nowait``, in which case it returns
        ``False`` instead of waiting. A ``str`` key is hashed to a 64-bit one (the first
        8 bytes of its BLAKE2b digest, signed big-endian). Must run inside
        ``db.transaction()``.
        """
        if self.url.startswith("sqlite://"):
            raise QueryError("sqlite does not support advisory locks")
        tx = self._tx()
        if tx is None:
            raise TransactionRequired(
                "db.lock() outside a transaction would release the lock at once; "
                "run it inside `async with db.transaction():`"
            )
        if isinstance(key, bool) or not isinstance(key, (int, str)):
            raise TypeError(f"lock key must be an int or a str, got {key!r}")
        name = None
        if isinstance(key, str):
            name = key.encode()
            key = 0
        if not -(2**63) <= key < 2**63:
            raise ValueError("lock key must fit in 64 bits")
        return await self._engine.advisory_lock(int(key), name, bool(exclusive), bool(nowait), tx)

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
    url: str,
    *,
    max_connections: int = 10,
    default: bool = True,
    registry: Registry = registry,
    _disable: tuple[str, ...] = (),
) -> Database:
    """Open a connection pool. Import your model modules first: the schema is compiled
    from the models registered at this point (in ``registry``, the default one unless
    given).

    With ``default=True`` (the default) queries use this database unless
    ``.using(db)`` says otherwise. ``_disable`` switches database capabilities off
    (``"ilike"``, ``"update_from_values"``, ...) to test the SQL other databases get.
    """
    global _default
    engine = await _native.connect(url, registry.native(), max_connections, list(_disable))
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
