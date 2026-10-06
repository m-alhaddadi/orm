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

import enum
from collections.abc import Callable
from typing import TYPE_CHECKING, Any, ClassVar, Generic, Literal, Never, TypeVar, cast, overload

from .errors import NotLoaded
from .expr import ColumnRef, RelationPath

if TYPE_CHECKING:
    from .model import Model
    from .query import ManyRelatedSet, RelatedSet

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

    Models normally come from a schema file (``orm.load`` / generated modules), which
    builds these descriptors from the compiled schema. Everything the database needs
    beyond what Python uses here (indexes, checks, extension types...) stays in the
    schema IR.
    """

    type_name: ClassVar[str]

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
        default: T | Callable[[], T] = MISSING,
        default_now: bool = False,
        server_default: bool = False,
    ) -> None:
        self.primary_key = primary_key
        self.auto_increment = auto_increment
        self.nullable = nullable
        self.unique = unique
        self.index = index
        self._column = column
        self.default = default
        self.default_now = default_now
        # The database computes a default (an SQL expression in the schema).
        self.server_default = server_default

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
        from .errors import NotLoaded
        raise NotLoaded(f"{owner.__name__}.{self.name} was not loaded")

    if TYPE_CHECKING:
        # Instances are read-only. Declared for type checkers only: a runtime __set__
        # would make this a data descriptor and turn every attribute read into a call.
        def __set__(self, obj: object, value: Never) -> None: ...

    @property
    def has_server_value(self) -> bool:
        """True if the database fills the column when the insert leaves it out."""
        literal = self.default is not MISSING and not callable(self.default)
        return self.auto_increment or self.default_now or self.server_default or literal

    def ir(self) -> dict[str, Any]:
        out: dict[str, Any] = {"name": self.name, "column": self.column, "type": self.type_name}
        for flag in ("nullable", "primary_key", "auto_increment", "unique", "index", "default_now"):
            if getattr(self, flag):
                out[flag] = True
        if self.default is not MISSING and not callable(self.default):
            out["default"] = self.default
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
    """``uuid``. Values are ``uuid.UUID``; strings are accepted as input."""

    type_name = "uuid"


class Json(Field[T]):
    """``jsonb``. Values are dicts, lists, strings, numbers, booleans or None."""

    type_name = "json"


class Decimal(Field[T]):
    """``numeric``. Values are exact ``decimal.Decimal``s; ints, floats and decimal
    strings are accepted as input."""

    type_name = "decimal"


class Array(Field[T]):
    """An array column (``text[]``, ``integer[]``, ...). Values are lists; ``of`` is the
    IR type of the elements and ``enum`` their enum, if any."""

    type_name = "array"

    def __init__(self, of: str, *, enum: str | None = None, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self.of = of
        self.enum_name = enum

    def ir(self) -> dict[str, Any]:
        out = super().ir()
        out["type"] = self.of
        out["array"] = True
        if self.enum_name is not None:
            out["enum"] = self.enum_name
        return out


class Enum(Field[T]):
    """A column of a schema enum. Values are members of its Python enum class
    (``StrEnum`` or ``IntEnum``); plain stored values are accepted as input too.
    ``stored`` is the IR type the values are stored as."""

    type_name = "enum"

    def __init__(self, enum: str, *, stored: str, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self.enum_name = enum
        self.stored = stored

    @property
    def enum_class(self) -> type[enum.Enum]:
        return self.model._meta.registry.get_enum(self.enum_name)

    def ir(self) -> dict[str, Any]:
        out = super().ir()
        out["type"] = self.stored
        out["enum"] = self.enum_name
        return out


# IR column type -> descriptor class, for models built from a compiled schema.
BY_TYPE: dict[str, type[Field[Any]]] = {
    c.type_name: c for c in (BigInt, Integer, Float, Boolean, String, Text, DateTime, Date, Uuid, Json, Decimal)
}


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
    ) -> None:
        super().__init__(target)
        self.via = via
        self._to = to
        self.on_delete = on_delete

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
        key = cast("Model", obj)._field_value(self.via)
        if self.name in d:
            related = d[self.name]
            if related is None:
                return None
            if related is not None and related._field_value(self.to) == key:
                return related
        elif key is None:
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


class HasOne(Relation[M, P]):
    """One-to-one relation whose key is on the other model: ``User.profile`` when
    ``Profile.user_id`` (unique) points at the user. On an instance it is the related
    object, or ``None``, once loaded with ``select_related`` / ``prefetch_related``."""

    kind = "one"

    def __init__(self, target: str | type[M], *, via: str, from_: str | None = None) -> None:
        super().__init__(target)
        self.via = via
        self._from = from_

    @property
    def from_(self) -> str:
        return self._from or self.model._meta.pk.name

    @overload
    def __get__(self, obj: None, owner: type[Any]) -> P: ...
    @overload
    def __get__(self, obj: object, owner: type[Any]) -> M: ...
    def __get__(self, obj: object | None, owner: type[Any]) -> Any:
        if obj is None:
            return RelationPath(owner, (self.name,), cast("type[Model]", self.target))
        d = obj.__dict__
        if self.name in d:
            return d[self.name]
        raise NotLoaded(
            f"{owner.__name__}.{self.name} is not loaded; use select_related({owner.__name__}.{self.name}) "
            f"or prefetch_related({owner.__name__}.{self.name})"
        )

    def __set__(self, obj: Model, value: Never) -> None:
        raise AttributeError(
            f"{self.model.__name__}.{self.name} is read-only; the key is "
            f"{self.target_name}.{self.via}"
        )

    def ir(self) -> dict[str, Any]:
        return {"name": self.name, "kind": "one", "target": self.target_name, "from": self.from_, "to": self.via}


class ManyToMany(Relation[MM, P]):
    """Many-to-many relation through a join model: ``Post.tags`` when ``PostTag`` rows
    link posts (``source``, its key column) and tags (``target_field``). On an instance,
    ``post.tags`` is a :class:`~orm.query.ManyRelatedSet`: a query over the post's tags
    that also adds and removes links."""

    kind = "many"

    def __init__(
        self,
        target: str | type[MM],
        *,
        through: str,
        source: str,
        target_field: str,
        from_: str | None = None,
        to: str | None = None,
    ) -> None:
        super().__init__(target)
        self._through = through
        self.source = source
        self.target_field = target_field
        self._from = from_
        self._to = to

    @property
    def through(self) -> type[Model]:
        return self.model._meta.registry.get(self._through)

    @property
    def from_(self) -> str:
        return self._from or self.model._meta.pk.name

    @property
    def to(self) -> str:
        return self._to or cast("type[Model]", self.target)._meta.pk.name

    @overload
    def __get__(self, obj: None, owner: type[Any]) -> P: ...
    @overload
    def __get__(self, obj: object, owner: type[Any]) -> ManyRelatedSet[MM]: ...
    def __get__(self, obj: object | None, owner: type[Any]) -> Any:
        if obj is None:
            return RelationPath(owner, (self.name,), self.target)
        from .query import ManyRelatedSet

        return ManyRelatedSet(self, obj)  # type: ignore[arg-type]

    def __set__(self, obj: Model, value: Never) -> None:
        raise AttributeError(
            f"{self.model.__name__}.{self.name} is read-only; link rows with "
            f"`await {self.model.__name__.lower()}.{self.name}.add(...)`"
        )

    def ir(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "kind": "many",
            "target": self.target_name,
            "from": self.from_,
            "to": self.to,
            "through": {"model": self._through, "source": self.source, "target": self.target_field},
        }
