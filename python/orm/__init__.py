"""Async Python ORM with a Rust core.

Models come from a schema file (``schema.orm``): ``orm.load("schema.orm")``, or a
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
    MultipleObjectsReturned,
    NotConnected,
    NotLoaded,
    ORMError,
    QueryError,
    SchemaError,
)
from .expr import ColumnRef, Condition, Expression, Ordering, RelationPath, and_, not_, or_
from .model import Model, Registry, define, load, loads, registry
from .query import QuerySet, RelatedSet
from .write import InsertMany, InsertOne, OnConflictMany, OnConflictOne

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
    "SchemaError",
    "DoesNotExist",
    "MultipleObjectsReturned",
    "NotLoaded",
    "NotConnected",
]
