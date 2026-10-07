"""Exceptions raised by the ORM."""

from __future__ import annotations

from ._native import DatabaseError, IntegrityError, LockNotAvailable, QueryError, SchemaError, WriteProtected

__all__ = [
    "ORMError",
    "DatabaseError",
    "IntegrityError",
    "LockNotAvailable",
    "QueryError",
    "SchemaError",
    "DoesNotExist",
    "MultipleObjectsReturned",
    "NotLoaded",
    "NotConnected",
    "TransactionRequired",
    "WriteProtected",
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


class TransactionRequired(ORMError, RuntimeError):
    """A lock was asked for outside ``db.transaction()``, where it would be released
    as soon as the statement ends."""
