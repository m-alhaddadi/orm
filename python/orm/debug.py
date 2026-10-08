"""Debug tools: find N+1 query patterns.

``with orm.debug.n_plus_one(threshold=5, fail=True):`` counts the queries of the block
by statement shape. A shape that runs more than ``threshold`` times is an N+1: the
scope reports the shape, the count and the call site of the first query, and names the
fix when a relation load sent the queries.
"""

from __future__ import annotations

import asyncio
import os
import sys
import warnings
from collections.abc import Coroutine, Generator
from contextlib import contextmanager
from contextvars import ContextVar
from dataclasses import dataclass, field
from types import FrameType
from typing import Any, TypeVar

from .errors import ORMError

T = TypeVar("T")

__all__ = ["NPlusOne", "NPlusOneWarning", "Shape", "Report", "n_plus_one"]


class NPlusOne(ORMError):
    """A query shape ran more than the threshold of :func:`n_plus_one` times."""

    def __init__(self, report: Report) -> None:
        super().__init__(report.message())
        self.report = report


class NPlusOneWarning(UserWarning):
    """The warning :func:`n_plus_one` gives with ``fail=False``."""


# Longer SQL, such as a chunk of 10 000 placeholders, is cut in the message.
_SQL_LIMIT = 500


@dataclass
class Shape:
    """One statement shape: how often it ran and where it ran first."""

    key: str
    sql: str
    count: int = 0
    site: str = "<unknown>"
    fix: str | None = None


@dataclass
class Report:
    """The queries of one :func:`n_plus_one` scope, by shape."""

    threshold: int
    shapes: dict[str, Shape] = field(default_factory=dict)

    @property
    def repeated(self) -> list[Shape]:
        """The shapes that ran more than ``threshold`` times, the most frequent first."""
        return sorted((s for s in self.shapes.values() if s.count > self.threshold), key=lambda s: -s.count)

    def message(self) -> str:
        lines = []
        for s in self.repeated:
            sql = s.sql if len(s.sql) <= _SQL_LIMIT else s.sql[:_SQL_LIMIT] + " ..."
            lines.append(f"{s.count} queries with one shape `{sql}`")
            lines.append(f"  at {s.site}" + (f"; use {s.fix}" if s.fix else ""))
        return "\n".join(lines)


# The report of the innermost open scope; None outside a scope.
_scope: ContextVar[Report | None] = ContextVar("orm_n_plus_one", default=None)
# The reports of the enclosing scopes, which count the inner queries too.
_outer: ContextVar[tuple[Report, ...]] = ContextVar("orm_n_plus_one_outer", default=())
# The relation load that sends the next queries, and where user code asked for it.
_hint: ContextVar[tuple[str, str] | None] = ContextVar("orm_n_plus_one_hint", default=None)

# The shapes that the current internal ORM loop (batches, chunked in_bulk) already counted.
_loop: ContextVar[set[str] | None] = ContextVar("orm_n_plus_one_loop", default=None)
# Where user code awaited the ORM task that runs the next queries; a task has no user frames.
_site: ContextVar[str | None] = ContextVar("orm_n_plus_one_site", default=None)

_PACKAGE = os.path.dirname(os.path.abspath(__file__)) + os.sep
_STDLIB = os.path.dirname(os.path.abspath(os.__file__)) + os.sep


@contextmanager
def n_plus_one(threshold: int = 5, *, fail: bool = False) -> Generator[Report]:
    """Count the queries of the block by statement shape (the SQL without its values).

    At the end of the block, a shape that ran more than ``threshold`` times raises
    :class:`NPlusOne` with ``fail=True``, or gives a :class:`NPlusOneWarning`. Tasks the
    block starts count too. The call site is captured only inside the scope, so code
    outside it pays one context variable read per query.
    """
    if threshold < 1:
        raise ValueError("threshold must be at least 1")
    report = Report(threshold)
    current = _scope.get()
    outer = _outer.set(_outer.get() + (current,)) if current is not None else None
    token = _scope.set(report)
    try:
        yield report
    finally:
        _scope.reset(token)
        if outer is not None:
            _outer.reset(outer)
    # A block that raised its own error gets no report.
    if report.repeated:
        if fail:
            raise NPlusOne(report)
        warnings.warn(report.message(), NPlusOneWarning, stacklevel=3)


def active() -> bool:
    return _scope.get() is not None


def call_site() -> str:
    """The first frame outside the ORM and the standard library: where user code sent the query."""
    site = _site.get()
    if site is not None:
        return site
    frame: FrameType | None = sys._getframe(1)
    while frame is not None:
        path = frame.f_code.co_filename
        if not path.startswith((_PACKAGE, _STDLIB)) and not path.startswith("<frozen"):
            return f"{os.path.relpath(path) if not path.startswith('<') else path}:{frame.f_lineno}"
        frame = frame.f_back
    return "<unknown>"


def spawn(loop: asyncio.AbstractEventLoop, coro: Coroutine[Any, Any, T]) -> asyncio.Task[T]:
    """``loop.create_task(coro)``; inside a scope, the task reports the call site of this call."""
    if _scope.get() is None:
        return loop.create_task(coro)
    token = _site.set(call_site())
    try:
        return loop.create_task(coro)
    finally:
        _site.reset(token)


@contextmanager
def internal_loop(seen: set[str]) -> Generator[None]:
    """Count each shape once for the ORM loop that owns ``seen``: its pages are one query to the user."""
    token = _loop.set(seen)
    try:
        yield
    finally:
        _loop.reset(token)


@contextmanager
def relation_load(model: str, relation: str, fix: str) -> Generator[None]:
    """Mark the queries of a relation load, so the report names ``fix``."""
    token = _hint.set((f"{fix}({model}.{relation})", call_site()))
    try:
        yield
    finally:
        _hint.reset(token)


def capture() -> tuple[str, str | None] | None:
    """Where user code sends the next queries and the fix to name; None outside a scope.

    Called before the engine runs them, while the caller's frames are on the stack."""
    if _scope.get() is None:
        return None
    hint = _hint.get()
    return (hint[1], hint[0]) if hint else (call_site(), None)


def record(sql: str, origin: tuple[str, str | None]) -> None:
    """Count one query of shape ``sql`` (the SQL with placeholders), sent at ``origin``."""
    report = _scope.get()
    if report is None:
        return
    seen = _loop.get()
    if seen is not None:
        if sql in seen:
            return
        seen.add(sql)
    shape = report.shapes.get(sql)
    if shape is None:
        shape = report.shapes[sql] = Shape(sql, sql, site=origin[0], fix=origin[1])
    shape.count += 1
    for enclosing in _outer.get():
        counted = enclosing.shapes.get(sql)
        if counted is None:
            counted = enclosing.shapes[sql] = Shape(sql, shape.sql, site=shape.site, fix=shape.fix)
        counted.count += 1
