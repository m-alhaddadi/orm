"""INSERT, UPDATE and DELETE statements.

Writes are explicit statements, never side effects of touching an instance::

    user = await User.objects.insert(email="a@b.c", name="A")
    users = await User.objects.insert_many([{"email": ..., "name": ...}, ...])
    user = await User.objects.insert(email="a@b.c", name="A2").on_conflict(User.email).do_update()
    await User.objects.insert_many(rows).on_conflict(User.email).do_nothing()
    n = await Post.objects.filter(...).update(views=Post.views + 1)
    posts = await Post.objects.filter(...).update(views=Post.views + 1).returning()
    n = await Post.objects.filter(...).delete()
    n = await Post.objects.update_many([{"id": 1, "title": "a"}, {"id": 2, "title": "b"}])

A statement runs when awaited. Inserts are one ``INSERT ... RETURNING`` that also fills
in database defaults (ids, timestamps) on the returned instances. Updates and deletes
return a row count, and use ``RETURNING`` only when ``.returning()`` asks for the rows.
"""

from __future__ import annotations

import warnings
from collections.abc import Generator, Iterable, Mapping
from typing import TYPE_CHECKING, Any, Generic, NoReturn, TypeVar

from . import _native
from .expr import ColumnRef, ConditionLike, Expression, IRContext, Ordering, as_condition
from .errors import QueryError
from .fields import BelongsTo

if TYPE_CHECKING:
    from .model import Model
    from .query import QuerySet

R = TypeVar("R")
M = TypeVar("M", bound="Model")

__all__ = [
    "InsertOne",
    "InsertMany",
    "CopyInsert",
    "OnConflictOne",
    "OnConflictMany",
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
    gave explicitly (the default ``do_update()`` columns).
    """
    meta = model._meta
    normalized: list[dict[str, Any]] = []
    provided: set[str] = set()
    for row in rows:
        values: dict[str, Any] = {}
        for key, value in row.items():
            if isinstance(value, Expression):
                raise TypeError(f"{meta.name}.{key}: insert takes plain values, not expressions")
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
        if batch_size is not None and batch_size < 1:
            raise ValueError("batch_size must be at least 1")
        self._qs = qs
        self._fields = fields
        self._rows = rows
        self._provided = provided
        self._conflict = conflict
        self._where = where  # partial unique index predicate (IR), parameters in `_set`
        self._update = update
        self._set = set_  # DO UPDATE assignments (IR) and their parameters
        self._batch_size = batch_size
        self._used = False

    def _derive(
        self,
        cls: type[Any],
        conflict: list[str],
        update: list[str] | None,
        set_: tuple[list[dict[str, Any]], list[Any]] | None = None,
        where: dict[str, Any] | None = None,
    ) -> Any:
        self._used = True
        return cls(self._qs, self._fields, self._rows, self._provided, conflict, update, set_, self._batch_size, where)

    async def _execute(self) -> list[Any]:
        from .db import resolve

        if not self._rows:
            return []
        model = self._qs.model
        db = resolve(self._qs._db)
        objs: list[Any] = await db._insert(
            model._meta.name, self._fields, self._rows, self._conflict, self._update, self._set, self._qs._db,
            self._batch_size, self._where,
        )
        return objs

    def __del__(self) -> None:
        if not getattr(self, "_used", True):
            warnings.warn(f"{self!r} was never awaited, so nothing was inserted", RuntimeWarning, stacklevel=2)

    def __repr__(self) -> str:
        clause = ""
        if self._conflict is not None:
            names = [*(self._update or ()), *(a["field"] + "=..." for a in (self._set or ([], []))[0])]
            action = "do_nothing()" if self._update is None else f"do_update({', '.join(names)})"
            where = " where=..." if self._where is not None else ""
            clause = f" on_conflict({', '.join(self._conflict)}{where}).{action}"
        return f"<{type(self).__name__} {self._qs.model.__name__} x{len(self._rows)}{clause}>"


class InsertOne(_Insert, Generic[R]):
    """``await`` gives the inserted instance (``None`` if skipped by ``do_nothing()``)."""

    __slots__ = ()

    def on_conflict(self, *columns: ColumnRef[Any], where: ConditionLike | None = None) -> OnConflictOne[R]:
        """Handle rows that violate the unique constraint on ``columns``. ``where`` is the
        predicate of a partial unique index (``ON CONFLICT (...) WHERE ...``)."""
        return OnConflictOne(self, _field_names(self._qs.model, columns, "on_conflict"), where)

    def __await__(self) -> Generator[Any, None, R]:
        self._used = True
        return self._one().__await__()

    async def _one(self) -> R:
        objs = await self._execute()
        return objs[0] if objs else None  # type: ignore[return-value]


class InsertMany(_Insert, Generic[M]):
    """``await`` gives the inserted instances, in input order (rows skipped by
    ``do_nothing()`` are left out). Rows beyond the parameter limit, or beyond
    ``batch_size``, go to further statements in one transaction."""

    __slots__ = ()

    def on_conflict(self, *columns: ColumnRef[Any], where: ConditionLike | None = None) -> OnConflictMany[M]:
        """Handle rows that violate the unique constraint on ``columns``. ``where`` is the
        predicate of a partial unique index (``ON CONFLICT (...) WHERE ...``)."""
        return OnConflictMany(self, _field_names(self._qs.model, columns, "on_conflict"), where)

    def __await__(self) -> Generator[Any, None, list[M]]:
        self._used = True
        return self._execute().__await__()


class CopyInsert(Generic[M]):
    """``insert_many(rows, copy=True)``: a Postgres ``COPY``. ``await`` gives the number
    of rows written; there is no ``RETURNING``, so no instances."""

    __slots__ = ("_qs", "_fields", "_rows", "_used")

    def __init__(self, qs: QuerySet[M], fields: list[str], rows: list[list[Any]]) -> None:
        self._qs = qs
        self._fields = fields
        self._rows = rows
        self._used = False

    def on_conflict(self, *columns: ColumnRef[Any], where: ConditionLike | None = None) -> NoReturn:
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


class _OnConflict:
    __slots__ = ("_insert", "_target", "_where")

    def __init__(self, insert: _Insert, target: list[str], where: ConditionLike | None = None) -> None:
        if not target:
            raise TypeError("on_conflict() needs the column(s) of a unique constraint")
        insert._used = True
        self._insert = insert
        self._target = target
        self._where = None if where is None else as_condition(where)

    def _where_ir(self, params: list[Any]) -> dict[str, Any] | None:
        model = self._insert._qs.model
        return None if self._where is None else self._where._ir(IRContext(model, params))

    def _do_nothing(self, cls: type[Any]) -> Any:
        params: list[Any] = []
        where = self._where_ir(params)
        return self._insert._derive(cls, self._target, None, None if where is None else ([], params), where)

    def _do_update(self, cls: type[Any], columns: tuple[ColumnRef[Any], ...], values: dict[str, Any]) -> Any:
        model = self._insert._qs.model
        if columns or values:
            update = _field_names(model, columns, "do_update")
        else:
            pk = model._meta.pk.name
            ins = self._insert
            update = [f for f in ins._fields if f in ins._provided and f not in self._target and f != pk]
        params: list[Any] = []
        where = self._where_ir(params)
        set_: tuple[list[dict[str, Any]], list[Any]] | None = None if where is None else ([], params)
        if values:
            set_ = (assignments(model, values, IRContext(model, params)), params)
            overlap = set(update) & {a["field"] for a in set_[0]}
            if overlap:
                raise TypeError(f"do_update() sets {', '.join(sorted(overlap))} twice")
        return self._insert._derive(cls, self._target, update, set_, where)


class OnConflictOne(_OnConflict, Generic[R]):
    __slots__ = ()

    def do_update(self, *columns: ColumnRef[Any], **values: Any) -> InsertOne[R]:
        """``ON CONFLICT DO UPDATE``: overwrite ``columns`` with the new values, and set
        ``values`` (plain values or expressions such as
        ``views=Post.views + excluded(Post.views)``). With neither, every field given to
        ``insert`` is overwritten, except the conflict columns."""
        return self._do_update(InsertOne, columns, values)  # type: ignore[no-any-return]

    def do_nothing(self) -> InsertOne[R | None]:
        """``ON CONFLICT DO NOTHING``: keep the existing row; ``await`` gives ``None``."""
        return self._do_nothing(InsertOne)  # type: ignore[no-any-return]


class OnConflictMany(_OnConflict, Generic[M]):
    __slots__ = ()

    def do_update(self, *columns: ColumnRef[Any], **values: Any) -> InsertMany[M]:
        """``ON CONFLICT DO UPDATE``: overwrite ``columns`` with the new values, and set
        ``values`` (plain values or expressions such as
        ``views=Post.views + excluded(Post.views)``). With neither, every field given in
        the rows is overwritten, except the conflict columns."""
        return self._do_update(InsertMany, columns, values)  # type: ignore[no-any-return]

    def do_nothing(self) -> InsertMany[M]:
        """``ON CONFLICT DO NOTHING``: skip conflicting rows."""
        return self._do_nothing(InsertMany)  # type: ignore[no-any-return]


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
