"""Exceptions raised by the ORM."""

from __future__ import annotations

from ._native import DatabaseError, IntegrityError, QueryError

__all__ = [
    "ORMError",
    "DatabaseError",
    "IntegrityError",
    "QueryError",
    "DoesNotExist",
    "MultipleObjectsReturned",
    "NotLoaded",
    "NotConnected",
]


class ORMError(Exception):
    """Base class for errors raised by the Python layer."""


class DoesNotExist(ORMError, LookupError):
    """``get()`` matched no row. Each model has a subclass: ``User.DoesNotExist``."""


class MultipleObjectsReturned(ORMError, LookupError):
    """``get()`` matched more than one row."""


class NotLoaded(ORMError, AttributeError):
    """A relation was accessed on an instance without being loaded first.

    Async code can't load it implicitly on attribute access; use ``select_related`` /
    ``prefetch_related``, or ``await obj.relation`` for to-many relations.
    """


class NotConnected(ORMError, RuntimeError):
    """No database: call ``await orm.connect(url)`` first."""
