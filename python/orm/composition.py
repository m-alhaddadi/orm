"""Explicit composition operations; imported only when attach is requested."""
from __future__ import annotations

from collections.abc import Mapping
from typing import TYPE_CHECKING, Any, TypeVar, cast

from .expr import Expression
from .protection import allowed_writes

if TYPE_CHECKING:
    from .model import Model
    from .query import QuerySet

M = TypeVar("M", bound="Model")

async def attach(qs: QuerySet[M], parent_id: Any, values: Mapping[str, Any]) -> M:
    from .db import resolve

    meta = qs.model._meta
    fields = meta.attach_fields
    if fields is None:
        raise TypeError(f"{meta.name} is not a composed child")
    local: dict[str, Any] = {}
    for name, value in values.items():
        if name not in fields:
            raise TypeError(f"{meta.name}.{name}: attach accepts only local child fields")
        if isinstance(value, Expression):
            raise TypeError("attach takes plain values, not expressions")
        local[name] = value
    for name, field in fields.items():
        if name in local:
            continue
        if callable(field.default):
            local[name] = field.default()
        elif not (field.has_server_value or field.nullable):
            raise ValueError(f"{meta.name}.{name} is required")
    names = list(local)
    db = resolve(qs._db)
    rows = await db._engine.attach(meta.name, parent_id, names, [[local[n] for n in names]], db._tx(), qs._db, allowed_writes())
    return cast(M, rows[0])
