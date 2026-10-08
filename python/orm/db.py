"""Connections and transactions."""

from __future__ import annotations

import inspect
import json
import math
from collections.abc import AsyncIterator, Awaitable, Callable, Coroutine, Iterator, Sequence
from contextlib import AbstractAsyncContextManager, asynccontextmanager, contextmanager
from contextvars import ContextVar
from typing import Any, Literal, overload

from . import _native, debug
from .errors import LockNotAvailable, NotConnected, QueryError, TransactionRequired
from .protection import allowed_writes
from .model import Registry, registry

__all__ = ["Database", "connect", "get_database", "scope"]

# Statements a replica may answer.
_READS = frozenset({"select", "count", "exists"})

_default: Database | None = None
# For each database (its root) with an open transaction in the current task: the innermost one.
_current_tx: ContextVar[dict[Database, _native.Transaction]] = ContextVar("orm_current_tx", default={})
# For each database in a `tenant()` block: its primary and replica engines with the tenant set.
_tenants: ContextVar[dict[Database, tuple[_native.Engine, list[_native.Engine]]]] = ContextVar("orm_tenants", default={})
# The `scope.<name>` values of default filters.
_scope: ContextVar[dict[str, Any]] = ContextVar("orm_scope", default={})
# For each database with an open transaction: the on_commit callbacks of the innermost one.
_callbacks: ContextVar[dict[Database, _Callbacks]] = ContextVar("orm_on_commit", default={})


class _Callbacks(list[Callable[[], Any]]):
    """The on_commit callbacks of one transaction. ``ended`` is set when it commits or rolls
    back: a task that it started can still see it after that."""

    ended = False


class Database:
    """A connection pool. Created by :func:`connect`."""

    def __init__(
        self, engine: _native.Engine, url: str, registry: Registry = registry, replicas: Sequence[_native.Engine] = ()
    ) -> None:
        self._base = engine
        self.url = url
        self._registry = registry
        self._replicas = list(replicas)
        self._turn = 0
        # The database a `primary` view belongs to: they share transactions and callbacks.
        self._root: Database = self

    @property
    def primary(self) -> Database:
        """This database without its replicas: every statement goes to the primary."""
        view = Database(self._base, self.url, self._registry)
        view._root = self._root
        return view

    @property
    def _engine(self) -> _native.Engine:
        """The primary's engine, with the tenant of an enclosing `tenant()` block."""
        tenant = _tenants.get().get(self._root)
        return self._base if tenant is None else tenant[0]

    def _tx(self) -> _native.Transaction | None:
        return _current_tx.get().get(self._root)

    def _reader(self) -> _native.Engine:
        """The engine for a read: the next replica outside a transaction, else the primary."""
        if not self._replicas or self._tx() is not None:
            return self._engine
        tenant = _tenants.get().get(self._root)
        replicas = self._replicas if tenant is None else tenant[1]
        self._turn = (self._turn + 1) % len(replicas)
        return replicas[self._turn]

    @contextmanager
    def tenant(self, id: str | int) -> Iterator[None]:
        """``with db.tenant(id):`` — every transaction on this database in the block first
        runs ``SELECT set_config('app.tenant', <id>, true)`` (``SET LOCAL``), so Postgres
        row-level security policies can read ``current_setting('app.tenant')``. A
        statement outside a transaction runs in a transaction of its own. A transaction
        that is already open keeps its setting. Tasks started in the block get it."""
        if self.url.startswith("sqlite://"):
            raise QueryError("db.tenant() sets a Postgres setting for row-level security; sqlite has none")
        if isinstance(id, bool) or not isinstance(id, (str, int)):
            raise TypeError(f"tenant id must be a str or an int, got {id!r}")
        root, value = self._root, [str(id)]
        engines = (
            root._base.with_settings(["app.tenant"], value),
            [r.with_settings(["app.tenant"], value) for r in root._replicas],
        )
        token = _tenants.set({**_tenants.get(), root: engines})
        try:
            yield
        finally:
            _tenants.reset(token)

    async def _run(
        self, ir: dict[str, Any], params: list[Any], row_cls: type | None = None, db: Database | None = None
    ) -> Any:
        """Runs a query; instances it builds get ``db`` to write back to (``using()``)."""
        op, params = _with_scope(json.dumps(ir), params)
        if debug._scope.get() is not None:
            debug.record("run:" + op, lambda: self._registry.native().statement(op, params))
        engine = self._reader() if ir["op"] in _READS else self._engine
        return await engine.run(op, params, self._tx(), row_cls, db, allowed_writes())

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
        if debug._scope.get() is not None:
            debug.record(f"insert:{model}:{fields}:{conflict}", lambda: f"INSERT INTO {model} ({', '.join(fields)}) ...")
        return await self._engine.insert(model, fields, rows, conflict, update, set_json, params, self._tx(), db, allowed_writes())

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
        without_defaults: bool = False,
    ) -> Any:
        if debug._scope.get() is not None:
            debug.record(f"update_many:{model}:{fields}:{json.dumps(filters)}", lambda: f"UPDATE {model} SET {', '.join(fields)} ... (update_many)")
        filters_json, params = _with_scope(json.dumps(filters), params, key="filters")
        return await self._engine.update_many(
            model, fields, rows, filters_json, params, returning, batch_size, self._tx(), db, without_defaults,
            allowed_writes(),
        )

    @asynccontextmanager
    async def transaction(self) -> AsyncIterator[None]:
        """``async with db.transaction():`` — commits on success, rolls back on error.

        Queries on this database inside the block (in this task, and in tasks it starts)
        run in the transaction. Nested blocks use savepoints.
        """
        outer = self._tx() is None
        tx = await self._engine.begin(self._tx())
        token = _current_tx.set({**_current_tx.get(), self._root: tx})
        callbacks = _Callbacks()
        cb_token = _callbacks.set({**_callbacks.get(), self._root: callbacks})
        try:
            yield
        except BaseException:
            callbacks.ended = True
            _callbacks.reset(cb_token)
            _current_tx.reset(token)
            await tx.rollback()
            raise
        callbacks.ended = True
        _callbacks.reset(cb_token)
        _current_tx.reset(token)
        await tx.commit()
        await self._after_commit(callbacks, outer)

    async def _after_commit(self, callbacks: list[Callable[[], Any]], outer: bool) -> None:
        """A released savepoint hands its callbacks to the enclosing transaction."""
        if not outer:
            _callbacks.get()[self._root].extend(callbacks)
            return
        for fn in callbacks:
            result = fn()
            if inspect.isawaitable(result):
                await result

    async def on_commit(self, fn: Callable[[], Awaitable[Any] | Any]) -> None:
        """Call ``fn()`` after the outermost transaction on this database commits; a
        rollback drops it (a rolled-back savepoint drops only the callbacks registered
        inside it). Outside a transaction, ``fn()`` runs at once. An awaitable result is
        awaited. Callbacks run in order, outside the transaction; an error in one goes to
        the caller of ``transaction()`` and the later ones do not run. A task that the
        transaction started and that calls this after the transaction ended raises
        ``TransactionRequired``."""
        callbacks = _callbacks.get().get(self._root)
        if callbacks is not None and callbacks.ended:
            raise TransactionRequired("on_commit(): the transaction of this task has ended")
        if callbacks is not None:
            callbacks.append(fn)
            return
        result = fn()
        if inspect.isawaitable(result):
            await result

    @overload
    def lock(
        self, key: int | str, exclusive: bool = True, *, session: Literal[False] = False, nowait: bool = False
    ) -> Coroutine[Any, Any, bool]: ...
    @overload
    def lock(
        self,
        key: int | str,
        exclusive: bool = True,
        *,
        session: Literal[True],
        nowait: bool = False,
        timeout: float | None = None,
    ) -> AbstractAsyncContextManager[None]: ...
    def lock(
        self,
        key: int | str,
        exclusive: bool = True,
        *,
        session: bool = False,
        nowait: bool = False,
        timeout: float | None = None,
    ) -> Coroutine[Any, Any, bool] | AbstractAsyncContextManager[None]:
        """Take an advisory lock on ``key``: a lock on a name rather than on rows, e.g.
        "only one worker imports this file at a time".

        ``await db.lock(key)`` holds the lock until the transaction ends and must run
        inside ``db.transaction()``. It waits for the lock unless ``nowait``, in which
        case it returns ``False`` instead of waiting.

        ``async with db.lock(key, session=True, timeout=5):`` holds the lock for the
        block, on a connection of its own, with no transaction. It waits at most
        ``timeout`` seconds (forever when ``None``; not at all with ``nowait``) and
        raises ``LockNotAvailable`` when the lock is still held by another session.

        ``exclusive=False`` takes a shared lock (any number of shared holders, but no
        exclusive one). A ``str`` key is hashed to a 64-bit one (the first 8 bytes of its
        BLAKE2b digest, signed big-endian).
        """
        if session:
            return self._session_lock(key, exclusive, nowait, timeout)
        return self._xact_lock(key, exclusive, nowait)

    def _lock_key(self, key: int | str) -> tuple[int, bytes | None]:
        if isinstance(key, bool) or not isinstance(key, (int, str)):
            raise TypeError(f"lock key must be an int or a str, got {key!r}")
        if isinstance(key, str):
            return 0, key.encode()
        if not -(2**63) <= key < 2**63:
            raise ValueError("lock key must fit in 64 bits")
        return int(key), None

    async def _xact_lock(self, key: int | str, exclusive: bool, nowait: bool) -> bool:
        if self.url.startswith("sqlite://"):
            raise QueryError("sqlite does not support advisory locks")
        tx = self._tx()
        if tx is None:
            raise TransactionRequired(
                "db.lock() outside a transaction would release the lock at once; "
                "run it inside `async with db.transaction():` or use `session=True`"
            )
        k, name = self._lock_key(key)
        return await self._engine.advisory_lock(k, name, bool(exclusive), bool(nowait), tx)

    @asynccontextmanager
    async def _session_lock(
        self, key: int | str, exclusive: bool, nowait: bool, timeout: float | None
    ) -> AsyncIterator[None]:
        if self.url.startswith("sqlite://"):
            raise QueryError("sqlite does not support advisory locks")
        k, name = self._lock_key(key)
        if timeout is not None and not timeout >= 0:
            raise ValueError("lock timeout must be a number of seconds >= 0")
        timeout_ms = None if timeout is None else math.ceil(timeout * 1000)
        held = await self._engine.session_lock(k, name, bool(exclusive), bool(nowait), timeout_ms)
        if held is None:
            raise LockNotAvailable(
                f"advisory lock {key!r} is held by another session"
                + ("" if nowait or timeout is None else f" after {timeout}s")
            )
        try:
            yield
        finally:
            await held.release()

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
        """Close the pools of the primary and of each replica."""
        global _default
        if self._root is not self:
            return await self._root.close()
        await self._base.close()
        for replica in self._replicas:
            await replica.close()
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
    replicas: Sequence[str] = (),
    _disable: tuple[str, ...] = (),
) -> Database:
    """Open a connection pool. Import your model modules first: the schema is compiled
    from the models registered at this point (in ``registry``, the default one unless
    given).

    With ``default=True`` (the default) queries use this database unless
    ``.using(db)`` says otherwise. With ``replicas`` (URLs of read replicas of
    ``url``), reads outside a transaction go to a replica, in turn; writes and every
    statement in a transaction go to ``url``. ``_disable`` switches database capabilities off
    (``"ilike"``, ``"update_from_values"``, ...) to test the SQL other databases get.
    """
    global _default
    schema = registry.prepare()
    engine = await _native.connect(url, schema, max_connections, list(_disable))
    readers = [await _native.connect(r, schema, max_connections, list(_disable)) for r in replicas]
    db = Database(engine, url, registry, readers)
    if default:
        _default = db
    return db


@contextmanager
def scope(**values: Any) -> Iterator[None]:
    """``with orm.scope(shop=shop.id):`` — the values that ``scope.<name>`` reads in
    default filters (``@@query.filter("shop_id == scope.shop")``). A query on a model
    whose default filter reads a value that no enclosing ``scope()`` sets raises
    ``QueryError``. Inner values replace outer ones; tasks started in the block get them."""
    token = _scope.set({**_scope.get(), **values})
    try:
        yield
    finally:
        _scope.reset(token)


def _with_scope(op: str, params: list[Any], key: str | None = None) -> tuple[str, list[Any]]:
    """``op`` (a statement's IR, or with ``key`` a list wrapped under it) with the scope's
    parameter indexes, and ``params`` with the scope's values appended."""
    values = _scope.get()
    if not values:
        return op, params
    index = json.dumps({name: len(params) + i for i, name in enumerate(values)})
    op = f'{{"{key}":{op},"scope":{index}}}' if key else f'{op[:-1]},"scope":{index}}}'
    return op, [*params, *values.values()]


def get_database() -> Database:
    if _default is None:
        raise NotConnected("no default database; call `await orm.connect(url)` first")
    return _default


def resolve(db: Database | None) -> Database:
    return db if db is not None else get_database()
