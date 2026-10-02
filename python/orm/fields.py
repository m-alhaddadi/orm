"""Column and relation descriptors used by model classes.

Model modules are generated from the schema (see ``examples/blog``); these classes are
what the generated code is made of.

Column fields are *non-data* descriptors: on the class they return a
:class:`~orm.expr.ColumnRef` for building queries, while on an instance the value lives
in the instance ``__dict__``, so reading ``user.email`` is a plain attribute lookup.
Relations are data descriptors because they guard access to related objects that were
not loaded (async code can't lazy-load on attribute access).
"""

from __future__ import annotations

from collections.abc import Callable
from typing import TYPE_CHECKING, Any, ClassVar, Generic, Literal, Never, TypeVar, cast, overload

from .errors import NotLoaded
from .expr import ColumnRef, RelationPath
from .schema import Sql

if TYPE_CHECKING:
    from .model import Model
    from .query import RelatedSet

T = TypeVar("T")
# Related model type; for a nullable to-one relation it is `Target | None`.
M = TypeVar("M")
MM = TypeVar("MM", bound="Model")
P = TypeVar("P", bound="RelationPath[Any]")
OnDelete = Literal["cascade", "set_null", "set_default", "restrict", "no_action"]


class _Missing:
    def __repr__(self) -> str:
        return "MISSING"


MISSING: Any = _Missing()


class Field(Generic[T]):
    """A column. ``T`` is the Python type of its value (``int | None`` if nullable).

    Schema options beyond the basics: ``check`` (a ``CHECK`` expression in SQL),
    ``comment``, ``renamed_from`` (the previous column name, so the migration renames
    instead of dropping) and ``default=Sql("...")`` for a server-side SQL default.

    Extension column types (see :mod:`orm.ext`) set ``db_type`` (the SQL type) and, if
    the driver can't exchange values of that type directly, ``read_sql`` /
    ``write_sql`` templates where ``{}`` stands for the column / the bound value.
    """

    type_name: ClassVar[str]
    db_type: str | None = None
    read_sql: ClassVar[str | None] = None
    write_sql: str | None = None
    requires: ClassVar[tuple[str, ...]] = ()

    name: str
    column: str
    model: type[Model]

    def __init__(
        self,
        *,
        primary_key: bool = False,
        auto_increment: bool = False,
        nullable: bool = False,
        unique: bool = False,
        index: bool = False,
        column: str | None = None,
        default: T | Callable[[], T] | Sql = MISSING,
        default_now: bool = False,
        check: str | None = None,
        comment: str | None = None,
        renamed_from: str | None = None,
    ) -> None:
        self.primary_key = primary_key
        self.auto_increment = auto_increment
        self.nullable = nullable
        self.unique = unique
        self.index = index
        self._column = column
        self.default = default
        self.default_now = default_now
        self.check = check
        self.comment = comment
        self.renamed_from = renamed_from

    def __set_name__(self, owner: type[Model], name: str) -> None:
        self.name = name
        self.column = self._column or name
        self.model = owner

    @overload
    def __get__(self, obj: None, owner: type[Any]) -> ColumnRef[T]: ...
    @overload
    def __get__(self, obj: object, owner: type[Any]) -> T: ...
    def __get__(self, obj: object | None, owner: type[Any]) -> ColumnRef[T] | T:
        if obj is None:
            return ColumnRef(owner, (), self)
        # Only reached when the value is absent from the instance __dict__.
        raise AttributeError(f"{owner.__name__}.{self.name} was not loaded")

    if TYPE_CHECKING:
        # Instances are read-only. Declared for type checkers only: a runtime __set__
        # would make this a data descriptor and turn every attribute read into a call.
        def __set__(self, obj: object, value: Never) -> None: ...

    @property
    def has_server_value(self) -> bool:
        """True if the database fills the column when the insert leaves it out."""
        server = self.default is not MISSING and (isinstance(self.default, Sql) or not callable(self.default))
        return self.auto_increment or self.default_now or server

    def ir(self) -> dict[str, Any]:
        out: dict[str, Any] = {"name": self.name, "column": self.column, "type": self.type_name}
        for flag in ("nullable", "primary_key", "auto_increment", "unique", "index", "default_now"):
            if getattr(self, flag):
                out[flag] = True
        if isinstance(self.default, Sql):
            out["default_sql"] = self.default.sql
        elif self.default is not MISSING and not callable(self.default):
            out["default"] = self.default
        for key in ("check", "comment", "renamed_from", "db_type", "read_sql", "write_sql"):
            if (value := getattr(self, key)) is not None:
                out[key] = value
        if self.requires:
            out["requires"] = list(self.requires)
        return out

    def __repr__(self) -> str:
        owner = getattr(self, "model", None)
        where = f"{owner.__name__}.{self.name}" if owner else "unbound"
        return f"<{type(self).__name__} {where}>"


class BigInt(Field[T]):
    type_name = "big_int"


class Integer(Field[T]):
    type_name = "int"


class Float(Field[T]):
    type_name = "float"


class Boolean(Field[T]):
    type_name = "bool"


class String(Field[T]):
    type_name = "string"

    def __init__(self, max_length: int | None = None, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self.max_length = max_length

    def ir(self) -> dict[str, Any]:
        out = super().ir()
        if self.max_length is not None:
            out["max_length"] = self.max_length
        return out


class Text(Field[T]):
    type_name = "text"


class DateTime(Field[T]):
    """``timestamptz``. Values are timezone-aware ``datetime`` objects."""

    type_name = "date_time"


class Date(Field[T]):
    type_name = "date"


class Uuid(Field[T]):
    """``uuid``. Values are ``uuid.UUID``; strings are accepted as input.
    ``Uuid(primary_key=True, default=Sql("gen_random_uuid()"))`` for server-side keys."""

    type_name = "uuid"


class Json(Field[T]):
    """``jsonb``. Values are dicts, lists, strings, numbers, booleans or None."""

    type_name = "json"


# -- relations ----------------------------------------------------------------------------


class Relation(Generic[M, P]):
    kind: ClassVar[str]

    name: str
    model: type[Model]

    def __init__(self, target: str | type[M]) -> None:
        self._target = target

    def __set_name__(self, owner: type[Model], name: str) -> None:
        self.name = name
        self.model = owner

    @property
    def target(self) -> type[M]:
        if isinstance(self._target, str):
            self._target = self.model._meta.registry.get(self._target)  # type: ignore[assignment]
        return self._target  # type: ignore[return-value]

    @property
    def target_name(self) -> str:
        return self._target if isinstance(self._target, str) else self._target.__name__

    def ir(self) -> dict[str, Any]:
        raise NotImplementedError

    def __repr__(self) -> str:
        return f"<{type(self).__name__} {self.model.__name__}.{self.name} -> {self.target_name}>"


class BelongsTo(Relation[M, P]):
    """To-one relation through a foreign key column on this model.

    ``Post.author = BelongsTo("User", via="author_id")``: ``post.author_id`` holds the
    key, ``post.author`` the related object once loaded with ``select_related``.
    """

    kind = "one"

    def __init__(
        self,
        target: str | type[M],
        *,
        via: str,
        to: str | None = None,
        on_delete: OnDelete = "cascade",
        on_update: OnDelete | None = None,
        deferrable: Literal["immediate", "deferred"] | None = None,
    ) -> None:
        super().__init__(target)
        self.via = via
        self._to = to
        self.on_delete = on_delete
        self.on_update = on_update
        self.deferrable = deferrable

    @property
    def to(self) -> str:
        return self._to or cast("type[Model]", self.target)._meta.pk.name

    @overload
    def __get__(self, obj: None, owner: type[Any]) -> P: ...
    @overload
    def __get__(self, obj: object, owner: type[Any]) -> M: ...
    def __get__(self, obj: object | None, owner: type[Any]) -> Any:
        if obj is None:
            return RelationPath(owner, (self.name,), cast("type[Model]", self.target))
        d = obj.__dict__
        key = d.get(self.via)
        if self.name in d:
            related = d[self.name]
            if related is None and key is None:
                return None
            if related is not None and getattr(related, self.to, None) == key:
                return related
        elif key is None and self.via in d:
            return None
        raise NotLoaded(
            f"{owner.__name__}.{self.name} is not loaded; use "
            f"select_related({owner.__name__}.{self.name}) or query "
            f"{self.target_name} by {owner.__name__}.{self.via}"
        )

    def __set__(self, obj: Model, value: Never) -> None:
        raise AttributeError(
            f"{self.model.__name__}.{self.name} is read-only; write the key with "
            f"`await obj.update({self.name}=...)`"
        )

    def ir(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "kind": "one",
            "target": self.target_name,
            "from": self.via,
            "to": self.to,
            "foreign_key": True,
            "on_delete": self.on_delete,
            **({"on_update": self.on_update} if self.on_update else {}),
            **({"deferrable": self.deferrable} if self.deferrable else {}),
        }


class HasMany(Relation[MM, P]):
    """To-many relation: rows of ``target`` whose ``via`` column points at this model.

    ``User.posts = HasMany("Post", via="author_id")``. On an instance, ``user.posts``
    is a :class:`~orm.query.RelatedSet` (a query over the user's posts that also serves
    rows loaded by ``prefetch_related``).
    """

    kind = "many"

    def __init__(self, target: str | type[MM], *, via: str, from_: str | None = None) -> None:
        super().__init__(target)
        self.via = via
        self._from = from_

    @property
    def from_(self) -> str:
        return self._from or self.model._meta.pk.name

    @overload
    def __get__(self, obj: None, owner: type[Any]) -> P: ...
    @overload
    def __get__(self, obj: object, owner: type[Any]) -> RelatedSet[MM]: ...
    def __get__(self, obj: object | None, owner: type[Any]) -> Any:
        if obj is None:
            return RelationPath(owner, (self.name,), self.target)
        from .query import RelatedSet

        return RelatedSet(self, obj)  # type: ignore[arg-type]

    def __set__(self, obj: Model, value: Never) -> None:
        raise AttributeError(
            f"{self.model.__name__}.{self.name} is read-only; insert related rows with "
            f"`await {self.model.__name__.lower()}.{self.name}.insert(...)`"
        )

    def ir(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "kind": "many",
            "target": self.target_name,
            "from": self.from_,
            "to": self.via,
        }
