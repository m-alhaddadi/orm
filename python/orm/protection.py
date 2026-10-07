"""``@@protected_write``: ORM writes to a protected model run only inside
:func:`allow_writes`.

An application-level check in the ORM, not in the database: raw SQL (``db.execute``),
migrations and other database clients still write.
"""

from __future__ import annotations

from collections.abc import Generator
from contextlib import contextmanager
from contextvars import ContextVar
from typing import Any

__all__ = ["allow_writes"]

# The model names the current scope allows to write; scopes nest by union.
_allowed: ContextVar[tuple[str, ...]] = ContextVar("orm_allowed_writes", default=())


@contextmanager
def allow_writes(*models: Any) -> Generator[None]:
    """``with orm.allow_writes(Post):`` allows ORM writes to ``Post``, which
    ``@@protected_write`` otherwise rejects with :class:`orm.WriteProtected`.

    A sync ``with``: it does no I/O and starts no transaction. Tasks started inside it
    get the scope too. A nested scope adds its models to the outer ones.
    """
    names = []
    for model in models:
        meta = getattr(model, "_meta", None) if isinstance(model, type) else None
        if meta is None:
            raise TypeError(f"allow_writes() takes model classes, got {model!r}")
        names.append(meta.name)
    token = _allowed.set(_allowed.get() + tuple(names))
    try:
        yield
    finally:
        _allowed.reset(token)


def allowed_writes() -> tuple[str, ...]:
    """The model names the current scope allows to write."""
    return _allowed.get()
