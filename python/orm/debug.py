"""Debug tools: find N+1 query patterns.

``with orm.debug.n_plus_one(threshold=5, fail=True):`` counts the queries of the block
by statement shape. A shape that runs more than ``threshold`` times is an N+1: the
scope reports the shape, the count and the call site of the first query, and names the
fix when a relation load sent the queries.
"""

from __future__ import annotations

import os
import sys
import warnings
from collections.abc import Callable, Generator
from contextlib import contextmanager
from contextvars import ContextVar
from dataclasses import dataclass, field
from types import FrameType

from .errors import ORMError

__all__ = ["NPlusOne", "NPlusOneWarning", "Shape", "Report", "n_plus_one"]


class NPlusOne(ORMError):
    """A query shape ran more than the threshold of :func:`n_plus_one` times."""

    def __init__(self, report: Report) -> None:
        super().__init__(report.message())
        self.report = report


class NPlusOneWarning(UserWarning):
    """The warning :func:`n_plus_one` gives with ``fail=False``."""


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
            lines.append(f"{s.count} queries with one shape `{s.sql}`")
            lines.append(f"  at {s.site}" + (f"; use {s.fix}" if s.fix else ""))
        return "\n".join(lines)


# The report of the innermost open scope; None outside a scope.
_scope: ContextVar[Report | None] = ContextVar("orm_n_plus_one", default=None)
# The relation load that sends the next queries, and where user code asked for it.
_hint: ContextVar[tuple[str, str] | None] = ContextVar("orm_n_plus_one_hint", default=None)

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
    token = _scope.set(report)
    try:
        yield report
    finally:
        _scope.reset(token)
    # A block that raised its own error gets no report.
    if report.repeated:
        if fail:
            raise NPlusOne(report)
        warnings.warn(report.message(), NPlusOneWarning, stacklevel=3)


def active() -> bool:
    return _scope.get() is not None


def call_site() -> str:
    """The first frame outside the ORM and the standard library: where user code sent the query."""
    frame: FrameType | None = sys._getframe(1)
    while frame is not None:
        path = frame.f_code.co_filename
        if not path.startswith((_PACKAGE, _STDLIB)) and not path.startswith("<frozen"):
            return f"{os.path.relpath(path) if not path.startswith('<') else path}:{frame.f_lineno}"
        frame = frame.f_back
    return "<unknown>"


@contextmanager
def relation_load(model: str, relation: str, fix: str) -> Generator[None]:
    """Mark the queries of a relation load, so the report names ``fix``."""
    token = _hint.set((f"{fix}({model}.{relation})", call_site()))
    try:
        yield
    finally:
        _hint.reset(token)


def record(key: str, sql: Callable[[], str]) -> None:
    """Count one query of shape ``key``; ``sql()`` gives its SQL text when it is new."""
    report = _scope.get()
    if report is None:
        return
    shape = report.shapes.get(key)
    if shape is None:
        hint = _hint.get()
        shape = report.shapes[key] = Shape(key, sql(), site=hint[1] if hint else call_site(), fix=hint[0] if hint else None)
    shape.count += 1
