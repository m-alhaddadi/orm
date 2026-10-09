"""Public hooks for packages that change model writes and reads from outside the ORM.

A package such as ``orm-file-storage`` must check a write before it does I/O, run
the write later with some values replaced, and decode loaded values. These hooks
do that through the same conversion and planning as ``insert()`` and ``update()``.
"""
from __future__ import annotations

import json
from collections.abc import Callable, Mapping
from typing import TYPE_CHECKING, Any, Generic, TypeVar

from .protection import allowed_writes
from .write import Update, prepare_rows

if TYPE_CHECKING:
    from .model import Model
    from .query import QuerySet

M = TypeVar("M", bound="Model")

__all__ = ["PreparedInsert", "PreparedUpdate", "decode_field", "prepare_insert", "prepare_update"]


class PreparedInsert(Generic[M]):
    """A single-row insert that conversion and planning accepted, without SQL or I/O."""

    __slots__ = ("_qs", "_values")

    def __init__(self, qs: QuerySet[M], values: Mapping[str, Any]) -> None:
        self._qs = qs
        self._values = dict(values)

    async def execute(self, values: Mapping[str, Any] | None = None) -> M:
        """Insert ``values`` (by default the checked values) and give the new instance."""
        from .db import resolve

        fields, rows, _ = prepare_rows(self._qs.model, [self._values if values is None else values])
        objs: list[M] = await resolve(self._qs._db)._insert(self._qs.model._meta.name, fields, rows, db=self._qs._db)
        return objs[0]


class PreparedUpdate(Generic[M]):
    """An update over a query set that planning accepted, without SQL or I/O."""

    __slots__ = ("_qs", "_values", "unique")

    def __init__(self, qs: QuerySet[M], values: Mapping[str, Any], unique: bool) -> None:
        self._qs = qs
        self._values = dict(values)
        #: The filters pin one row by a non-null primary key or unique field.
        self.unique = unique

    async def execute(self, values: Mapping[str, Any] | None = None, *, returning: bool = False) -> int | list[M]:
        """Update the matching rows to ``values`` (by default the checked values);
        with ``returning``, give the rows."""
        statement = Update.build(self._qs, self._values if values is None else values)
        if returning:
            return await statement.returning()
        return await statement


def prepare_insert(qs: QuerySet[M], values: Mapping[str, Any]) -> PreparedInsert[M]:
    """Check one ``insert(**values)`` as ``insert()`` does, without SQL or I/O."""
    from .db import resolve

    resolve(qs._db)
    fields, rows, _ = prepare_rows(qs.model, [values])
    qs.model._meta.registry.native().validate_insert(qs.model._meta.name, fields, rows, list(allowed_writes()))
    return PreparedInsert(qs, values)


def prepare_update(qs: QuerySet[M], values: Mapping[str, Any]) -> PreparedUpdate[M]:
    """Check ``qs.update(**values)`` as ``update()`` does, without SQL or I/O."""
    from .db import _with_scope, resolve

    resolve(qs._db)
    params: list[Any] = []
    ir = qs._mutation_ir("update", params, values)
    op, params = _with_scope(json.dumps(ir), params)
    unique: bool = qs.model._meta.registry.native().unique_row_update(op, params, list(allowed_writes()))
    return PreparedUpdate(qs, values, unique)


class _DecodedField:
    """A loaded value read through ``decode``; the class-side value stays the column."""

    __slots__ = ("column", "name", "decode")

    def __init__(self, column: Any, name: str, decode: Callable[[Any], Any]) -> None:
        self.column = column
        self.name = name
        self.decode = decode

    def __get__(self, obj: Any, owner: type[Any]) -> Any:
        if obj is None:
            return self.column.__get__(None, owner)
        try:
            value = obj.__dict__[self.name]
        except KeyError:
            return self.column.__get__(obj, owner)
        return self.decode(value)

    # A data descriptor, so that it reads before the native row values in the instance __dict__.
    def __set__(self, obj: Any, value: Any) -> None:
        raise AttributeError(f"{type(obj).__name__}.{self.name} is read-only")


def decode_field(model: type[Model], name: str, decode: Callable[[Any], Any]) -> None:
    """Read the loaded value of field ``name`` as ``decode(value)``, when it is read.

    Instances keep the stored value, so writes and ``refresh()`` see it unchanged."""
    if name not in model._meta.fields:
        raise TypeError(f"{model._meta.name} has no field {name!r}")
    column = next(vars(cls)[name] for cls in model.__mro__ if name in vars(cls))
    setattr(model, name, _DecodedField(column, name, decode))
