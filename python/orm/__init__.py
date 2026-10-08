"""Async Python ORM with a Rust core.

Models come from a schema file (``schema.prisma``): ``orm.load("schema.prisma")``, or a
module generated from it (``python -m orm generate``, see ``examples/blog``). Queries are
expressions over model attributes, Django-style managers on top::

    yesterday = datetime.now(timezone.utc) - timedelta(days=1)
    users = await User.objects.filter(User.posts.created_at < yesterday)
"""

from . import debug, fields
from .db import Database, connect, get_database, scope
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
    WriteProtected,
)
from .pagination import Page
from .expr import (
    ColumnRef,
    Condition,
    Expression,
    Func,
    Ordering,
    RelationPath,
    ScalarSubquery,
    Window,
    WindowDef,
    and_,
    excluded,
    exists,
    func,
    not_,
    or_,
    outer,
    param,
    window,
)
from .model import Model, Registry, define, load, loads, registry
from .protection import allow_writes
from .query import ManyRelatedSet, Prefetch, Prepared, QuerySet, RelatedSet
from .cte import Cte, CteColumn
from .select import Row, Select
from .write import CopyInsert, Delete, InsertMany, InsertOne, OnConflictMany, OnConflictOne, Returning, Update, UpdateMany

__all__ = [
    "debug",
    "fields",
    "Registry",
    "define",
    "load",
    "loads",
    "Database",
    "connect",
    "scope",
    "get_database",
    "Model",
    "registry",
    "QuerySet",
    "RelatedSet",
    "ManyRelatedSet",
    "Prefetch",
    "Prepared",
    "param",
    "Cte",
    "CteColumn",
    "Select",
    "Row",
    "InsertOne",
    "InsertMany",
    "CopyInsert",
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
    "Page",
    "and_",
    "or_",
    "not_",
    "excluded",
    "exists",
    "outer",
    "func",
    "Func",
    "Window",
    "WindowDef",
    "window",
    "ScalarSubquery",
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
    "allow_writes",
]
