"""INSERT, UPDATE and DELETE statements.

Writes are explicit statements, never side effects of touching an instance::

    user = await User.objects.insert(email="a@b.c", name="A")
    users = await User.objects.insert_many([{"email": ..., "name": ...}, ...]).returning()
    n = await User.objects.insert_many(rows)
    users = await User.objects.insert_many(rows).returning()
    user = await User.objects.insert(email="a@b.c", name="A2").on_conflict(User.email, update=True).returning()
    n = await User.objects.insert_many(rows).on_conflict(User.email, update=False)
    n = await Post.objects.filter(...).update(views=Post.views + 1)
    posts = await Post.objects.filter(...).update(views=Post.views + 1).returning()
    n = await Post.objects.filter(...).delete()
    n = await Post.objects.update_many([{"id": 1, "title": "a"}, {"id": 2, "title": "b"}])

A statement runs when awaited. ``insert()`` is one ``INSERT ... RETURNING`` that also
fills in database defaults (ids, timestamps) on the returned instance. ``insert_many()``,
upserts, updates and deletes return a row count, and use ``RETURNING`` only when
``.returning()`` asks for the rows.
"""

from __future__ import annotations

import warnings
from collections.abc import Generator, Iterable, Mapping
from typing import TYPE_CHECKING, Any, Generic, Literal, NoReturn, TypeVar, overload

from . import _native
from .expr import ColumnRef, ConditionLike, Expression, IRContext, Ordering, as_condition
from .errors import QueryError
from .fields import BelongsTo

if TYPE_CHECKING:
    from .model import Model
    from .query import QuerySet

R_co = TypeVar("R_co", covariant=True)
M = TypeVar("M", bound="Model")

__all__ = [
    "InsertOne",
    "InsertMany",
    "UpsertOne",
    "InsertReturning",
    "CopyInsert",
    "Update",
    "UpdateMany",
    "Delete",
    "Returning",
]


def _key_of(value: Any, name: str) -> Any:
    """The key field ``name`` of a related instance, loaded or private."""
    return value._field_value(name) if hasattr(value, "_field_value") else getattr(value, name)


def prepare_rows(
    model: type[Model], rows: Iterable[Mapping[str, Any]]
) -> tuple[list[str], list[list[Any]], set[str]]:
    """Validates rows and aligns them on one column list.

    Returns (fields, rows, provided): the fields any row sets, each row as values in
    that order (``DEFAULT`` where a row leaves a field out), and the fields the caller
    gave explicitly (the default ``on_conflict(..., update=True)`` columns).
    """
    meta = model._meta
    normalized: list[dict[str, Any]] = []
    provided: set[str] = set()
    for row in rows:
        values: dict[str, Any] = {}
        for key, value in row.items():
            if isinstance(value, Expression):
                raise TypeError(f"{meta.name}.{key}: insert takes plain values, not expressions")
            if isinstance(value, Ordering):
                raise TypeError(f"{key}={value!r} is an ordering, for order_by(); write 0 - {value.expr!r} to negate a value")
            if key in meta.input_fields:
                values[key] = value
            elif isinstance(rel := meta.relations.get(key), BelongsTo):
                values[rel.via] = None if value is None else _key_of(value, rel.to)
            else:
                raise TypeError(f"{meta.name} has no field {key!r}")
        provided.update(values)
        for name, field in meta.input_fields.items():
            if name in values:
                continue
            if callable(field.default):
                values[name] = field.default()
            elif not (field.has_insert_default or field.nullable):
                raise ValueError(f"{meta.name}.{name} is required")
        normalized.append(values)
    fields = [n for n in meta.field_names if any(n in v for v in normalized)]
    aligned = [[v.get(n, _native.DEFAULT) for n in fields] for v in normalized]
    return fields, aligned, provided


def lookup_values(model: type[Model], values: Mapping[str, Any]) -> dict[str, Any]:
    """``get_or_insert``'s lookup by field name: plain, non-null values; a to-one
    relation (``author=user``) gives its key field."""
    meta = model._meta
    out: dict[str, Any] = {}
    for key, value in values.items():
        if isinstance(value, Expression):
            raise TypeError(f"{meta.name}.{key}: get_or_insert takes plain values, not expressions")
        if isinstance(rel := meta.relations.get(key), BelongsTo):
            key, value = rel.via, (None if value is None else _key_of(value, rel.to))
        elif key not in meta.input_fields:
            raise TypeError(f"{meta.name} has no field {key!r}")
        if value is None:
            raise ValueError(f"get_or_insert: {meta.name}.{key} is None; NULL never conflicts, so the row is not unique")
        out[key] = value
    if not out:
        raise TypeError("get_or_insert() needs the fields of a unique constraint")
    return out


def prepare_update_rows(model: type[Model], rows: Iterable[Mapping[str, Any]]) -> tuple[list[str], list[list[Any]]]:
    """Validates ``update_many`` rows: each has the primary key and the same fields,
    plain values only, no primary key twice. Returns (fields, rows) with the primary key
    first and the other fields in schema order."""
    meta = model._meta
    pk = meta.pk.name
    normalized: list[dict[str, Any]] = []
    seen: set[Any] = set()
    fields: set[str] | None = None
    for n, row in enumerate(rows):
        values: dict[str, Any] = {}
        for key, value in row.items():
            if isinstance(value, Expression):
                raise TypeError(f"{meta.name}.{key}: update_many takes plain values, not expressions")
            if key in meta.input_fields:
                values[key] = value
            elif isinstance(rel := meta.relations.get(key), BelongsTo):
                values[rel.via] = None if value is None else _key_of(value, rel.to)
            else:
                raise TypeError(f"{meta.name} has no field {key!r}")
        if values.get(pk) is None:
            raise ValueError(f"update_many row {n} has no {pk}")
        if values[pk] in seen:
            raise ValueError(f"update_many: {pk}={values[pk]!r} appears twice")
        seen.add(values[pk])
        keys = set(values)
        if fields is None:
            if keys == {pk}:
                raise ValueError(f"update_many rows need a field to set besides {pk}")
            fields = keys
        elif keys != fields:
            diff = ", ".join(sorted(keys ^ fields))
            raise ValueError(f"update_many rows must set the same fields; row {n} differs in {diff}")
        normalized.append(values)
    if fields is None:
        return [], []
    names = [pk, *(f for f in meta.field_names if f in fields and f != pk)]
    return names, [[v[f] for f in names] for v in normalized]


def assignments(model: type[Model], values: Mapping[str, Any], ctx: IRContext) -> list[dict[str, Any]]:
    """``SET`` items as IR: plain values become parameters, expressions compile in
    ``ctx``. A to-one relation (``author=user``) sets its key column."""
    meta = model._meta
    out = []
    for name, value in values.items():
        rel = meta.relations.get(name)
        if isinstance(rel, BelongsTo):
            name, value = rel.via, (None if value is None else _key_of(value, rel.to))
        if name not in meta.input_fields:
            raise TypeError(f"{meta.name} has no field {name!r}")
        if isinstance(value, Ordering):
            raise TypeError(f"{name}={value!r} is an ordering, for order_by(); write 0 - {value.expr!r} to negate a value")
        node = value._ir(ctx) if isinstance(value, Expression) else ctx.param(value)
        out.append({"field": name, "value": node})
    return out


def _field_names(model: type[Model], columns: tuple[ColumnRef[Any], ...], what: str) -> list[str]:
    names = []
    for c in columns:
        if not isinstance(c, ColumnRef) or c._root is not model or c._path:
            raise TypeError(f"{what} expects columns of {model.__name__}, got {c!r}")
        names.append(c._field.name)
    return names


class _Insert:
    __slots__ = ("_qs", "_fields", "_rows", "_provided", "_conflict", "_where", "_update", "_set", "_batch_size", "_used")

    def __init__(
        self,
        qs: QuerySet[Any],
        fields: list[str],
        rows: list[list[Any]],
        provided: set[str],
        conflict: list[str] | None = None,
        update: list[str] | None = None,
        set_: tuple[list[dict[str, Any]], list[Any]] | None = None,
        batch_size: int | None = None,
        where: dict[str, Any] | None = None,
    ) -> None:
        if batch_size is not None and (isinstance(batch_size, bool) or not isinstance(batch_size, int) or batch_size < 1):
            raise ValueError("batch_size must be an integer of at least 1")
        self._qs = qs
        self._fields = fields
        self._rows = rows
        self._provided = provided
        self._conflict = conflict
        self._where = where  # partial unique index predicate (IR), parameters in `_set`
        self._update = update  # None: DO NOTHING
        self._set = set_  # DO UPDATE assignments (IR) and their parameters
        self._batch_size = batch_size
        self._used = False

    def _with_conflict(
        self,
        cls: type[Any],
        columns: tuple[ColumnRef[Any], ...],
        where: ConditionLike | None,
        update: bool,
        update_fields: Iterable[ColumnRef[Any]] | None,
        update_values: Mapping[str, Any] | None,
    ) -> Any:
        self._used = True
        if self._conflict is not None:
            raise TypeError("on_conflict() is already given")
        model = self._qs.model
        target = _field_names(model, columns, "on_conflict")
        if not target:
            raise TypeError("on_conflict() needs the column(s) of a unique constraint")
        if not isinstance(update, bool):
            raise TypeError(f"on_conflict(update=...) must be True or False, got {update!r}")
        if not update and (update_fields is not None or update_values is not None):
            raise TypeError("on_conflict(update=False) skips conflicting rows; it takes no update_fields or update_values")
        params: list[Any] = []
        where_ir = None if where is None else as_condition(where)._ir(IRContext(model, params))
        set_: tuple[list[dict[str, Any]], list[Any]] | None = None if where_ir is None else ([], params)
        if not update:
            return cls(self._qs, self._fields, self._rows, self._provided, target, None, set_, self._batch_size, where_ir)
        fields = None if update_fields is None else _field_names(model, tuple(update_fields), "update_fields")
        if fields is not None and not fields:
            raise TypeError("on_conflict(update_fields=[]) updates nothing; use update=False to skip conflicting rows")
        if update_values is not None and not update_values:
            raise TypeError("on_conflict(update_values={}) updates nothing; use update=False to skip conflicting rows")
        if fields is None and update_values is None:
            pk = model._meta.pk.name
            fields = [f for f in self._fields if f in self._provided and f not in target and f != pk]
        if update_values is not None:
            set_ = (assignments(model, update_values, IRContext(model, params)), params)
            overlap = set(fields or ()) & {a["field"] for a in set_[0]}
            if overlap:
                raise TypeError(f"on_conflict() updates {', '.join(sorted(overlap))} in both update_fields and update_values")
        return cls(self._qs, self._fields, self._rows, self._provided, target, fields or [], set_, self._batch_size, where_ir)

    async def _execute(self, returning: bool) -> Any:
        from .db import resolve

        if not self._rows:
            return [] if returning else 0
        db = resolve(self._qs._db)
        return await db._insert(
            self._qs.model._meta.name, self._fields, self._rows, self._conflict, self._update, self._set, self._qs._db,
            self._batch_size, self._where, returning=returning,
        )

    def __del__(self) -> None:
        if not getattr(self, "_used", True):
            warnings.warn(f"{self!r} was never awaited, so nothing was inserted", RuntimeWarning, stacklevel=2)

    def __repr__(self) -> str:
        clause = ""
        if self._conflict is not None:
            options = [f"update={self._update is not None}"]
            if self._update:
                options.append(f"update_fields=[{', '.join(self._update)}]")
            if self._set and self._set[0]:
                options.append(f"update_values={{{', '.join(a['field'] + ': ...' for a in self._set[0])}}}")
            if self._where is not None:
                options.append("where=...")
            clause = f" on_conflict({', '.join([*self._conflict, *options])})"
        return f"<{type(self).__name__} {self._qs.model.__name__} x{len(self._rows)}{clause}>"


class InsertOne(_Insert, Generic[M]):
    """``await`` gives the inserted instance. ``on_conflict()`` makes it an upsert."""

    __slots__ = ()

    @overload
    def on_conflict(
        self,
        *columns: ColumnRef[Any],
        where: ConditionLike | None = None,
        update: Literal[True],
        update_fields: Iterable[ColumnRef[Any]] | None = None,
        update_values: Mapping[str, Any] | None = None,
    ) -> UpsertOne[M]: ...
    @overload
    def on_conflict(
        self,
        *columns: ColumnRef[Any],
        where: ConditionLike | None = None,
        update: bool,
        update_fields: Iterable[ColumnRef[Any]] | None = None,
        update_values: Mapping[str, Any] | None = None,
    ) -> UpsertOne[M | None]: ...
    def on_conflict(
        self,
        *columns: ColumnRef[Any],
        where: ConditionLike | None = None,
        update: bool,
        update_fields: Iterable[ColumnRef[Any]] | None = None,
        update_values: Mapping[str, Any] | None = None,
    ) -> UpsertOne[Any]:
        """Handle a row that violates the unique constraint on ``columns``; ``where`` is
        the predicate of a partial unique index (``ON CONFLICT (...) WHERE ...``).

        ``update=False`` keeps the existing row (``DO NOTHING``). ``update=True`` updates
        it: ``update_fields`` copy the new values, ``update_values`` set values or
        expressions such as ``{"views": Post.views + excluded(Post.views)}``. With
        neither, every given field except the conflict columns is overwritten.
        ``await`` gives the affected-row count; ``.returning()`` the row."""
        return self._with_conflict(UpsertOne, columns, where, update, update_fields, update_values)  # type: ignore[no-any-return]

    def __await__(self) -> Generator[Any, None, M]:
        self._used = True
        return self._one().__await__()

    async def _one(self) -> M:
        objs: list[M] = await self._execute(True)
        return objs[0]


class UpsertOne(_Insert, Generic[R_co]):
    """``insert(...).on_conflict(...)``: ``await`` gives the affected-row count (0 or 1);
    ``.returning()`` the inserted or updated instance (``None`` for a skipped row)."""

    __slots__ = ()

    def returning(self) -> InsertReturning[R_co]:
        """``RETURNING`` the row: ``await`` gives the instance, ``None`` when
        ``update=False`` skipped the row."""
        self._used = True
        return InsertReturning(self, one=True)

    def __await__(self) -> Generator[Any, None, int]:
        self._used = True
        return self._execute(False).__await__()


class InsertMany(_Insert, Generic[M]):
    """``await`` gives the number of rows inserted or updated; ``.returning()`` the
    instances. Rows beyond the parameter limit, or beyond ``batch_size``, go to further
    statements in one transaction."""

    __slots__ = ()

    def on_conflict(
        self,
        *columns: ColumnRef[Any],
        where: ConditionLike | None = None,
        update: bool,
        update_fields: Iterable[ColumnRef[Any]] | None = None,
        update_values: Mapping[str, Any] | None = None,
    ) -> InsertMany[M]:
        """Handle rows that violate the unique constraint on ``columns``; ``where`` is
        the predicate of a partial unique index (``ON CONFLICT (...) WHERE ...``).

        ``update=False`` skips conflicting rows (``DO NOTHING``). ``update=True``
        updates them: ``update_fields`` copy the new values, ``update_values`` set values
        or expressions such as ``{"views": Post.views + excluded(Post.views)}``. With
        neither, every given field except the conflict columns is overwritten."""
        return self._with_conflict(InsertMany, columns, where, update, update_fields, update_values)  # type: ignore[no-any-return]

    def returning(self) -> InsertReturning[list[M]]:
        """``RETURNING`` the rows: ``await`` gives the instances in input order; rows
        that ``update=False`` skipped are left out."""
        self._used = True
        return InsertReturning(self, one=False)

    def __await__(self) -> Generator[Any, None, int]:
        self._used = True
        return self._execute(False).__await__()


class InsertReturning(Generic[R_co]):
    """An insert with ``.returning()``: ``await`` gives the instance(s)."""

    __slots__ = ("_insert", "_one", "_used")

    def __init__(self, insert: _Insert, *, one: bool) -> None:
        self._insert = insert
        self._one = one
        self._used = False

    async def _rows(self) -> Any:
        objs: list[Any] = await self._insert._execute(True)
        if self._one:
            return objs[0] if objs else None
        return objs

    def __await__(self) -> Generator[Any, None, R_co]:
        self._used = True
        return self._rows().__await__()

    def __del__(self) -> None:
        if not getattr(self, "_used", True):
            warnings.warn(f"{self!r} was never awaited, so nothing was inserted", RuntimeWarning, stacklevel=2)

    def __repr__(self) -> str:
        return f"{self._insert!r}.returning()"


class CopyInsert(Generic[M]):
    """``insert_many(rows, copy=True)``: a Postgres ``COPY``. ``await`` gives the number
    of rows written; there is no ``RETURNING``, so no instances."""

    __slots__ = ("_qs", "_fields", "_rows", "_used")

    def __init__(self, qs: QuerySet[M], fields: list[str], rows: list[list[Any]]) -> None:
        self._qs = qs
        self._fields = fields
        self._rows = rows
        self._used = False

    def on_conflict(
        self,
        *columns: ColumnRef[Any],
        where: ConditionLike | None = None,
        update: bool,
        update_fields: Iterable[ColumnRef[Any]] | None = None,
        update_values: Mapping[str, Any] | None = None,
    ) -> NoReturn:
        self._used = True
        raise TypeError("insert_many(copy=True) can't be combined with on_conflict(); COPY stops at the first conflict")

    async def _count(self) -> int:
        from .db import resolve

        if not self._rows:
            return 0
        n: int = await resolve(self._qs._db)._copy(self._qs.model._meta.name, self._fields, self._rows)
        return n

    def __await__(self) -> Generator[Any, None, int]:
        self._used = True
        return self._count().__await__()

    def __del__(self) -> None:
        if not getattr(self, "_used", True):
            warnings.warn(f"{self!r} was never awaited, so nothing was inserted", RuntimeWarning, stacklevel=2)

    def __repr__(self) -> str:
        return f"<CopyInsert {self._qs.model.__name__} x{len(self._rows)}>"


# -- UPDATE / DELETE -----------------------------------------------------------------------


class _SetStatement(Generic[M]):
    """An UPDATE or DELETE over a query set, compiled (and validated) when created."""

    __slots__ = ("_qs", "_ir", "_params", "_used")
    _verb = ""

    def __init__(self, qs: QuerySet[M], ir: dict[str, Any] | None, params: list[Any]) -> None:
        self._qs = qs
        self._ir = ir  # None: nothing to do (an update without values)
        self._params = params
        self._used = False

    def returning(self) -> Returning[M]:
        """Run with ``RETURNING`` and give the affected rows as instances instead of a
        count."""
        self._used = True
        return Returning(self._qs, None if self._ir is None else {**self._ir, "returning": True}, self._params)

    async def _count(self) -> int:
        if self._ir is None:
            return 0
        n: int = await self._qs._run(self._ir, self._params)
        return n

    def __await__(self) -> Generator[Any, None, int]:
        self._used = True
        return self._count().__await__()

    def __del__(self) -> None:
        if not getattr(self, "_used", True):
            warnings.warn(f"{self!r} was never awaited, so nothing was {self._verb}", RuntimeWarning, stacklevel=2)

    def __repr__(self) -> str:
        return f"<{type(self).__name__} {self._qs!r}>"


class Update(_SetStatement[M]):
    """``await`` gives the number of rows updated; ``.returning()`` the rows."""

    __slots__ = ()
    _verb = "updated"

    @classmethod
    def build(cls, qs: QuerySet[M], values: Mapping[str, Any]) -> Update[M]:
        params: list[Any] = []
        ir = qs._mutation_ir("update", params, values)
        return cls(qs, ir if ir["set"] else None, params)


class Delete(_SetStatement[M]):
    """``await`` gives the number of rows deleted; ``.returning()`` the rows."""

    __slots__ = ()
    _verb = "deleted"

    @classmethod
    def build(cls, qs: QuerySet[M], *, hard: bool = False) -> Delete[M]:
        params: list[Any] = []
        ir = qs._mutation_ir("delete", params)
        if hard:
            ir["hard"] = True
        return cls(qs, ir, params)


class Returning(Generic[M]):
    """An UPDATE or DELETE with ``RETURNING``: ``await`` gives the affected rows."""

    __slots__ = ("_qs", "_ir", "_params", "_used")

    def __init__(self, qs: QuerySet[M], ir: dict[str, Any] | None, params: list[Any]) -> None:
        self._qs = qs
        self._ir = ir
        self._params = params
        self._used = False

    async def _rows(self) -> list[M]:
        if self._ir is None:
            return []
        rows: list[M] = await self._qs._run(self._ir, self._params)
        return rows

    def __await__(self) -> Generator[Any, None, list[M]]:
        self._used = True
        return self._rows().__await__()

    def __del__(self) -> None:
        if not getattr(self, "_used", True):
            warnings.warn(f"{self!r} was never awaited, so nothing ran", RuntimeWarning, stacklevel=2)

    def __repr__(self) -> str:
        op = "?" if self._ir is None else self._ir["op"]
        return f"<Returning {op} {self._qs!r}>"


class UpdateMany(Generic[M]):
    """``update_many``: ``await`` gives the number of rows updated; ``.returning()`` the
    rows."""

    __slots__ = ("_qs", "_fields", "_rows", "_batch_size", "_used")

    def __init__(self, qs: QuerySet[M], rows: Iterable[Mapping[str, Any]], batch_size: int | None) -> None:
        if batch_size is not None and batch_size < 1:
            raise ValueError("batch_size must be at least 1")
        qs._mutation_ir("update", [])  # rejects sliced and locked query sets
        self._qs = qs
        self._fields, self._rows = prepare_update_rows(qs.model, rows)
        self._batch_size = batch_size
        self._used = False

    async def _run(self, returning: bool) -> Any:
        from .db import resolve

        if not self._rows:
            return [] if returning else 0
        params: list[Any] = []
        ir = self._qs._mutation_ir("update", params)
        if "with" in ir:
            raise QueryError("update_many() filters can't read CTEs")
        db = resolve(self._qs._db)
        return await db._update_many(
            self._qs.model._meta.name, self._fields, self._rows, ir["filters"], params, returning, self._batch_size,
            self._qs._db, self._qs._without_defaults,
        )

    def returning(self) -> _UpdateManyReturning[M]:
        """Run with ``RETURNING`` and give the updated rows instead of a count."""
        self._used = True
        return _UpdateManyReturning(self)

    async def _count(self) -> int:
        n: int = await self._run(False)
        return n

    def __await__(self) -> Generator[Any, None, int]:
        self._used = True
        return self._count().__await__()

    def __del__(self) -> None:
        if not getattr(self, "_used", True):
            warnings.warn(f"{self!r} was never awaited, so nothing was updated", RuntimeWarning, stacklevel=2)

    def __repr__(self) -> str:
        return f"<UpdateMany {self._qs.model.__name__} x{len(self._rows)} ({', '.join(self._fields[1:])})>"


class _UpdateManyReturning(Generic[M]):
    __slots__ = ("_stmt",)

    def __init__(self, stmt: UpdateMany[M]) -> None:
        self._stmt = stmt

    async def _rows(self) -> list[M]:
        rows: list[M] = await self._stmt._run(True)
        return rows

    def __await__(self) -> Generator[Any, None, list[M]]:
        return self._rows().__await__()
