"""Async Python ORM with a Rust core.

Models come from a schema file (``schema.prisma``): ``orm.load("schema.prisma")``, or a
module generated from it (``python -m orm generate``, see ``examples/blog``). Queries are
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
    LockNotAvailable,
    MultipleObjectsReturned,
    NotConnected,
    NotLoaded,
    ORMError,
    QueryError,
    SchemaError,
    TransactionRequired,
)
from .expr import ColumnRef, Condition, Expression, Func, Ordering, RelationPath, and_, excluded, func, not_, or_
from .model import Model, Registry, define, load, loads, registry
from .query import QuerySet, RelatedSet
from .select import Row, Select
from .write import Delete, InsertMany, InsertOne, OnConflictMany, OnConflictOne, Returning, Update, UpdateMany

__all__ = [
    "fields",
    "Registry",
    "define",
    "load",
    "loads",
    "Database",
    "connect",
    "get_database",
    "Model",
    "registry",
    "QuerySet",
    "RelatedSet",
    "Select",
    "Row",
    "InsertOne",
    "InsertMany",
    "OnConflictOne",
    "OnConflictMany",
    "Update",
    "UpdateMany",
    "Delete",
    "Returning",
    "Expression",
    "ColumnRef",
    "RelationPath",
    "Condition",
    "Ordering",
    "and_",
    "or_",
    "not_",
    "excluded",
    "func",
    "Func",
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
]
