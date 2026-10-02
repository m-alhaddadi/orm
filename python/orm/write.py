"""INSERT statements.

Writes are explicit statements, never side effects of touching an instance::

    user = await User.objects.insert(email="a@b.c", name="A")
    users = await User.objects.insert_many([{"email": ..., "name": ...}, ...])
    user = await User.objects.insert(email="a@b.c", name="A2").on_conflict(User.email).do_update()
    await User.objects.insert_many(rows).on_conflict(User.email).do_nothing()

A statement runs when awaited, as one ``INSERT ... RETURNING`` that also fills in
database defaults (ids, timestamps) on the returned instances.
"""

from __future__ import annotations

import warnings
from collections.abc import Generator, Iterable, Mapping
from typing import TYPE_CHECKING, Any, Generic, TypeVar

from . import _native
from .expr import ColumnRef, Expression
from .fields import BelongsTo

if TYPE_CHECKING:
    from .model import Model
    from .query import QuerySet

R = TypeVar("R")
M = TypeVar("M", bound="Model")

__all__ = ["InsertOne", "InsertMany", "OnConflictOne", "OnConflictMany"]


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
            if key in meta.fields:
                values[key] = value
            elif isinstance(rel := meta.relations.get(key), BelongsTo):
                values[rel.via] = None if value is None else getattr(value, rel.to)
            else:
                raise TypeError(f"{meta.name} has no field {key!r}")
        provided.update(values)
        for name, field in meta.fields.items():
            if name in values:
                continue
            if callable(field.default):
                values[name] = field.default()
            elif not (field.has_server_value or field.nullable):
                raise ValueError(f"{meta.name}.{name} is required")
        normalized.append(values)
    fields = [n for n in meta.field_names if any(n in v for v in normalized)]
    aligned = [[v.get(n, _native.DEFAULT) for n in fields] for v in normalized]
    return fields, aligned, provided


def _field_names(model: type[Model], columns: tuple[ColumnRef[Any], ...], what: str) -> list[str]:
    names = []
    for c in columns:
        if not isinstance(c, ColumnRef) or c._root is not model or c._path:
            raise TypeError(f"{what} expects columns of {model.__name__}, got {c!r}")
        names.append(c._field.name)
    return names


class _Insert:
    __slots__ = ("_qs", "_fields", "_rows", "_provided", "_conflict", "_update", "_used")

    def __init__(
        self,
        qs: QuerySet[Any],
        fields: list[str],
        rows: list[list[Any]],
        provided: set[str],
        conflict: list[str] | None = None,
        update: list[str] | None = None,
    ) -> None:
        self._qs = qs
        self._fields = fields
        self._rows = rows
        self._provided = provided
        self._conflict = conflict
        self._update = update
        self._used = False

    def _derive(self, cls: type[Any], conflict: list[str], update: list[str] | None) -> Any:
        self._used = True
        return cls(self._qs, self._fields, self._rows, self._provided, conflict, update)

    async def _execute(self) -> list[Any]:
        from .db import resolve

        if not self._rows:
            return []
        model = self._qs.model
        db = resolve(self._qs._db)
        rows = await db._insert(model._meta.name, self._fields, self._rows, self._conflict, self._update)
        make = model._from_row
        objs = [make(r) for r in rows]
        if self._qs._db is not None:
            for o in objs:
                o.__dict__["_db"] = self._qs._db
        return objs

    def __del__(self) -> None:
        if not getattr(self, "_used", True):
            warnings.warn(f"{self!r} was never awaited, so nothing was inserted", RuntimeWarning, stacklevel=2)

    def __repr__(self) -> str:
        clause = ""
        if self._conflict is not None:
            action = "do_nothing()" if self._update is None else f"do_update({', '.join(self._update)})"
            clause = f" on_conflict({', '.join(self._conflict)}).{action}"
        return f"<{type(self).__name__} {self._qs.model.__name__} x{len(self._rows)}{clause}>"


class InsertOne(_Insert, Generic[R]):
    """``await`` gives the inserted instance (``None`` if skipped by ``do_nothing()``)."""

    __slots__ = ()

    def on_conflict(self, *columns: ColumnRef[Any]) -> OnConflictOne[R]:
        """Handle rows that violate the unique constraint on ``columns``."""
        return OnConflictOne(self, _field_names(self._qs.model, columns, "on_conflict"))

    def __await__(self) -> Generator[Any, None, R]:
        self._used = True
        return self._one().__await__()

    async def _one(self) -> R:
        objs = await self._execute()
        return objs[0] if objs else None  # type: ignore[return-value]


class InsertMany(_Insert, Generic[M]):
    """``await`` gives the inserted instances, in input order (rows skipped by
    ``do_nothing()`` are left out)."""

    __slots__ = ()

    def on_conflict(self, *columns: ColumnRef[Any]) -> OnConflictMany[M]:
        """Handle rows that violate the unique constraint on ``columns``."""
        return OnConflictMany(self, _field_names(self._qs.model, columns, "on_conflict"))

    def __await__(self) -> Generator[Any, None, list[M]]:
        self._used = True
        return self._execute().__await__()


class _OnConflict:
    __slots__ = ("_insert", "_target")

    def __init__(self, insert: _Insert, target: list[str]) -> None:
        if not target:
            raise TypeError("on_conflict() needs the column(s) of a unique constraint")
        insert._used = True
        self._insert = insert
        self._target = target

    def _update_fields(self, columns: tuple[ColumnRef[Any], ...]) -> list[str]:
        model = self._insert._qs.model
        if columns:
            return _field_names(model, columns, "do_update")
        pk = model._meta.pk.name
        return [f for f in self._insert._fields if f in self._insert._provided and f not in self._target and f != pk]


class OnConflictOne(_OnConflict, Generic[R]):
    __slots__ = ()

    def do_update(self, *columns: ColumnRef[Any]) -> InsertOne[R]:
        """``ON CONFLICT DO UPDATE``: overwrite ``columns`` (default: every field given
        to ``insert``, except the conflict columns) with the new values."""
        return self._insert._derive(InsertOne, self._target, self._update_fields(columns))  # type: ignore[no-any-return]

    def do_nothing(self) -> InsertOne[R | None]:
        """``ON CONFLICT DO NOTHING``: keep the existing row; ``await`` gives ``None``."""
        return self._insert._derive(InsertOne, self._target, None)  # type: ignore[no-any-return]


class OnConflictMany(_OnConflict, Generic[M]):
    __slots__ = ()

    def do_update(self, *columns: ColumnRef[Any]) -> InsertMany[M]:
        """``ON CONFLICT DO UPDATE``: overwrite ``columns`` (default: every field given
        in the rows, except the conflict columns) with the new values."""
        return self._insert._derive(InsertMany, self._target, self._update_fields(columns))  # type: ignore[no-any-return]

    def do_nothing(self) -> InsertMany[M]:
        """``ON CONFLICT DO NOTHING``: skip conflicting rows."""
        return self._insert._derive(InsertMany, self._target, None)  # type: ignore[no-any-return]
