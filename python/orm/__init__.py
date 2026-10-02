"""Async Python ORM with a Rust core.

Models come from the schema (generated modules, see ``examples/blog``). Queries are
expressions over model attributes, Django-style managers on top::

    yesterday = datetime.now(timezone.utc) - timedelta(days=1)
    users = await User.objects.filter(User.posts.created_at < yesterday)
"""

from . import fields
from .db import Database, connect, get_database
from .errors import (
    DatabaseError,
    DoesNotExist,
    IntegrityError,
    MultipleObjectsReturned,
    NotConnected,
    NotLoaded,
    ORMError,
    QueryError,
)
from .expr import ColumnRef, Condition, Expression, Ordering, RelationPath, and_, not_, or_
from .model import Model, registry
from .query import QuerySet, RelatedSet
from .write import InsertMany, InsertOne, OnConflictMany, OnConflictOne

__all__ = [
    "fields",
    "Database",
    "connect",
    "get_database",
    "Model",
    "registry",
    "QuerySet",
    "RelatedSet",
    "InsertOne",
    "InsertMany",
    "OnConflictOne",
    "OnConflictMany",
    "Expression",
    "ColumnRef",
    "RelationPath",
    "Condition",
    "Ordering",
    "and_",
    "or_",
    "not_",
    "ORMError",
    "DatabaseError",
    "IntegrityError",
    "QueryError",
    "DoesNotExist",
    "MultipleObjectsReturned",
    "NotLoaded",
    "NotConnected",
]
