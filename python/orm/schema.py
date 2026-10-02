"""Database objects beyond columns: indexes, constraints, triggers, functions, extensions.

They are declared on a model's ``Meta`` class (or, for stand-alone functions and
extensions, on the registry) and only matter to migrations and ``create_tables()``;
queries never look at them::

    class Post(Model, table="posts"):
        ...

        class Meta:
            indexes = [
                Index("author_id", "-created_at", where="published"),
                Index(Sql("lower(title)"), name="posts_title_lower_idx"),
            ]
            constraints = [
                Check("views >= 0", name="posts_views_positive"),
                Unique("author_id", "slug"),
            ]
            triggers = [
                Trigger("touch", before=("update",), body="BEGIN NEW.updated_at := now(); RETURN NEW; END;"),
            ]

Raw SQL (predicates, expressions, function bodies) is written as it should appear in
the DDL, with column names, not Python attribute names. Field names *are* used where a
plain column is expected (index keys, ``Unique`` fields, ``include``, ``update_of``).
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any, Literal

__all__ = [
    "Sql",
    "Key",
    "Index",
    "Unique",
    "Check",
    "Exclude",
    "Trigger",
    "Function",
    "Extension",
]

TriggerEvent = Literal["insert", "update", "delete", "truncate"]
Deferrable = Literal["immediate", "deferred"]


@dataclass(frozen=True)
class Sql:
    """A raw SQL expression: an expression index key or a server default
    (``f.Uuid(default=Sql("gen_random_uuid()"))``)."""

    sql: str

    def __str__(self) -> str:
        return self.sql


@dataclass(frozen=True)
class Key:
    """One index key with options. Plain strings are shorthand: ``"title"`` and
    ``"-created_at"`` (descending)."""

    target: str | Sql
    opclass: str | None = None
    desc: bool = False
    nulls: Literal["first", "last"] | None = None
    collation: str | None = None

    def ir(self) -> dict[str, Any]:
        out: dict[str, Any] = {}
        if isinstance(self.target, Sql):
            out["expr"] = self.target.sql
        else:
            out["field"] = self.target
        for k in ("opclass", "nulls", "collation"):
            if (v := getattr(self, k)) is not None:
                out[k] = v
        if self.desc:
            out["desc"] = True
        return out


def _key(k: str | Sql | Key) -> Key:
    if isinstance(k, Key):
        return k
    if isinstance(k, Sql):
        return Key(k)
    if isinstance(k, str):
        return Key(k[1:], desc=True) if k.startswith("-") else Key(k)
    raise TypeError(f"index keys are field names, Sql(...) or Key(...), got {k!r}")


def _set(out: dict[str, Any], **values: Any) -> dict[str, Any]:
    out.update({k: v for k, v in values.items() if v not in (None, False, [], ())})
    return out


class Index:
    """``CREATE [UNIQUE] INDEX``.

    ``method`` is the access method (``"gin"``, ``"gist"``, ``"brin"``, ``"hnsw"``...),
    ``where`` makes it partial, ``include`` adds covering columns and ``with_`` sets
    storage parameters. Unnamed indexes are named ``<table>_<columns>_idx``.
    """

    def __init__(
        self,
        *keys: str | Sql | Key,
        name: str | None = None,
        unique: bool = False,
        method: str | None = None,
        where: str | None = None,
        include: Sequence[str] = (),
        with_: Mapping[str, Any] | None = None,
        nulls_not_distinct: bool = False,
        requires: Iterable[str] = (),
    ) -> None:
        if not keys:
            raise TypeError("Index needs at least one key")
        self.keys = tuple(_key(k) for k in keys)
        self.name = name
        self.unique = unique
        self.method = method
        self.where = where
        self.include = tuple(include)
        self.with_ = dict(with_ or {})
        self.nulls_not_distinct = nulls_not_distinct
        self.requires = tuple(requires)

    def ir(self) -> dict[str, Any]:
        out = _set(
            {"columns": [k.ir() for k in self.keys]},
            name=self.name,
            unique=self.unique,
            method=self.method,
            where=self.where,
            include=list(self.include),
            nulls_not_distinct=self.nulls_not_distinct,
            requires=list(self.requires),
        )
        if self.with_:
            out["with"] = [[k, _param(v)] for k, v in self.with_.items()]
        return out

    def __repr__(self) -> str:
        return f"Index({', '.join(repr(k.target) for k in self.keys)}, name={self.name!r})"


def _param(v: Any) -> str:
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (int, float)):
        return str(v)
    return "'" + str(v).replace("'", "''") + "'"


class Constraint:
    def ir(self) -> dict[str, Any]:
        raise NotImplementedError


class Unique(Constraint):
    """Multi-column ``UNIQUE`` constraint (single columns: ``f.String(unique=True)``).

    For a unique *expression* or a partial unique rule use ``Index(..., unique=True)``.
    """

    def __init__(
        self,
        *fields: str,
        name: str | None = None,
        nulls_not_distinct: bool = False,
        deferrable: Deferrable | None = None,
    ) -> None:
        if not fields:
            raise TypeError("Unique needs at least one field")
        self.fields = fields
        self.name = name
        self.nulls_not_distinct = nulls_not_distinct
        self.deferrable = deferrable

    def ir(self) -> dict[str, Any]:
        return _set(
            {"kind": "unique", "fields": list(self.fields)},
            name=self.name,
            nulls_not_distinct=self.nulls_not_distinct,
            deferrable=self.deferrable,
        )


class Check(Constraint):
    """``CHECK (<expr>)``. Unnamed checks get a name derived from the expression."""

    def __init__(self, expr: str, *, name: str | None = None) -> None:
        self.expr = expr
        self.name = name

    def ir(self) -> dict[str, Any]:
        return _set({"kind": "check", "expr": self.expr}, name=self.name)


class Exclude(Constraint):
    """``EXCLUDE USING <method> (<key> WITH <operator>, ...)``, e.g. no two bookings of
    one room overlap::

        Exclude(("room_id", "="), (Sql("tstzrange(starts_at, ends_at)"), "&&"),
                requires=["btree_gist"])

    Scalar equality inside a GiST exclusion needs the ``btree_gist`` extension.
    """

    def __init__(
        self,
        *elements: tuple[str | Sql | Key, str],
        name: str | None = None,
        method: str = "gist",
        where: str | None = None,
        deferrable: Deferrable | None = None,
        requires: Iterable[str] = (),
    ) -> None:
        if not elements:
            raise TypeError("Exclude needs at least one (key, operator) element")
        self.elements = tuple((_key(k), op) for k, op in elements)
        self.name = name
        self.method = method
        self.where = where
        self.deferrable = deferrable
        self.requires = tuple(requires)

    def ir(self) -> dict[str, Any]:
        return _set(
            {
                "kind": "exclude",
                "method": self.method,
                "elements": [k.ir() | {"operator": op} for k, op in self.elements],
            },
            name=self.name,
            where=self.where,
            deferrable=self.deferrable,
            requires=list(self.requires),
        )


class Trigger:
    """``CREATE TRIGGER``. Exactly one of ``before`` / ``after`` / ``instead_of`` lists
    the events; exactly one of ``body`` (a function ``<table>_<name>`` is generated)
    or ``function`` (an existing or :class:`Function`-declared one) says what runs::

        Trigger("touch", before=("update",), body="BEGIN NEW.updated_at := now(); RETURN NEW; END;")
        Trigger("audit", after=("insert", "update", "delete"), function="audit_row", args=("posts",))
    """

    def __init__(
        self,
        name: str,
        *,
        before: Sequence[TriggerEvent] = (),
        after: Sequence[TriggerEvent] = (),
        instead_of: Sequence[TriggerEvent] = (),
        update_of: Sequence[str] = (),
        for_each: Literal["row", "statement"] = "row",
        when: str | None = None,
        body: str | None = None,
        language: str = "plpgsql",
        function: str | Function | None = None,
        args: Sequence[str] = (),
    ) -> None:
        timings = [(t, ev) for t, ev in (("before", before), ("after", after), ("instead_of", instead_of)) if ev]
        if len(timings) != 1:
            raise TypeError(f"trigger {name}: give exactly one of before= / after= / instead_of=")
        if (body is None) == (function is None):
            raise TypeError(f"trigger {name}: give exactly one of body= / function=")
        self.name = name
        self.timing, events = timings[0]
        self.events = tuple(events)
        self.update_of = tuple(update_of)
        self.for_each = for_each
        self.when = when
        self.body = body
        self.language = language
        self.function = function.name if isinstance(function, Function) else function
        self.args = tuple(args)

    def ir(self) -> dict[str, Any]:
        out = _set(
            {"name": self.name, "timing": self.timing, "events": list(self.events), "for_each": self.for_each},
            update_of=list(self.update_of),
            when=self.when,
            function=self.function,
            args=list(self.args),
        )
        if self.body is not None:
            out |= {"body": self.body, "language": self.language}
        return out


@dataclass(frozen=True)
class Function:
    """A stand-alone SQL function, created with ``CREATE OR REPLACE FUNCTION``.

    Register it with ``registry.add(fn)`` (or list it in a model's ``Meta.functions``).
    """

    name: str
    body: str
    returns: str = "trigger"
    args: str = ""
    language: str = "plpgsql"
    volatility: Literal["immutable", "stable", "volatile"] | None = None
    security_definer: bool = False

    def ir(self) -> dict[str, Any]:
        return _set(
            {"name": self.name, "args": self.args, "returns": self.returns, "body": self.body},
            language=self.language,
            volatility=self.volatility,
            security_definer=self.security_definer,
        )


@dataclass(frozen=True)
class Extension:
    """A database extension (``CREATE EXTENSION``).

    Known extensions (``pg_trgm``, ``vector``, ``citext``, ``postgis``, ...) are pulled in
    automatically when a column type, index method, operator class or function of
    theirs is used; declare them only to pin a schema or version. For an extension the
    engine doesn't know, ``provides`` lists the names that should pull it in.
    """

    name: str
    schema: str | None = None
    version: str | None = None
    types: tuple[str, ...] = field(default=())
    index_methods: tuple[str, ...] = field(default=())
    opclasses: tuple[str, ...] = field(default=())
    functions: tuple[str, ...] = field(default=())

    def ir(self) -> dict[str, Any]:
        provides = _set(
            {},
            types=list(self.types),
            index_methods=list(self.index_methods),
            opclasses=list(self.opclasses),
            functions=list(self.functions),
        )
        return _set({"name": self.name}, schema=self.schema, version=self.version, provides=provides or None)
