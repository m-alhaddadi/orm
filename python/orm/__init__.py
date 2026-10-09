"""Async Python ORM with a Rust core.

Models come from a schema file (``schema.prisma``): ``orm.load("schema.prisma")``, or a
module generated from it (``python -m orm generate``, see ``examples/blog``). Queries are
expressions over model attributes, Django-style managers on top::

    yesterday = datetime.now(timezone.utc) - timedelta(days=1)
    users = await User.objects.filter(User.posts.created_at < yesterday)
"""

from . import debug, fields
from .db import Database, QueryEvent, connect, get_database, scope
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
    VersionConflict,
    WriteProtected,
)
from .pagination import Page
from .expr import (
    ColumnRef,
    Condition,
    Expression,
    Case,
    JsonPath,
    TsQuery,
    TsVector,
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
from .model import Model, Registry, column, define, describe, load, loads, registry
from .protection import allow_writes
from .query import ManyRelatedSet, Prepared, QuerySet, RelatedSet, prefetch, use_query_set
from .cte import Cte, CteColumn
from .select import Row, Select
from .write import CopyInsert, Delete, InsertMany, InsertOne, InsertReturning, Returning, Update, UpdateMany, UpsertOne

__all__ = [
    "debug",
    "fields",
    "Registry",
    "column",
    "define",
    "describe",
    "load",
    "loads",
    "Database",
    "QueryEvent",
    "connect",
    "scope",
    "get_database",
    "Model",
    "registry",
    "QuerySet",
    "RelatedSet",
    "ManyRelatedSet",
    "Prepared",
    "prefetch",
    "use_query_set",
    "param",
    "Cte",
    "CteColumn",
    "Select",
    "Row",
    "InsertOne",
    "InsertMany",
    "CopyInsert",
    "UpsertOne",
    "InsertReturning",
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
    "Case",
    "JsonPath",
    "TsQuery",
    "TsVector",
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
    "VersionConflict",
    "NotConnected",
    "TransactionRequired",
    "WriteProtected",
    "allow_writes",
]
