"""Cursor pagination: keyset pages over a unique order, with opaque cursors.

A cursor is base64url JSON ``{"o": <order fingerprint>, "v": [<order values>]}``. It
is not signed: a client can change it to start at any position of the same order.
"""

from __future__ import annotations

import base64
import binascii
import json
import math
import re
from datetime import date, datetime
from decimal import Decimal, InvalidOperation
from hashlib import blake2b
from typing import TYPE_CHECKING, Any, Generic, TypeVar
from uuid import UUID

from .errors import QueryError
from .expr import ColumnRef, Condition, Ordering, and_, or_

if TYPE_CHECKING:
    from .fields import Field
    from .model import Model

M = TypeVar("M", bound="Model")

__all__ = ["Page"]

# Column types with a cursor value; JSON, arrays and enums have none.
_CURSOR_TYPES = {"big_int", "int", "float", "bool", "string", "text", "date_time", "date", "uuid", "decimal"}


class Page(Generic[M]):
    """One page of :meth:`QuerySet.paginate`.

    ``next_cursor`` is the cursor of the last item and ``previous_cursor`` that of the
    first; both are ``None`` on an empty page. ``paginate(first=n, after=page.next_cursor)``
    reads on, ``paginate(last=n, before=page.previous_cursor)`` reads back.
    """

    __slots__ = ("items", "has_next", "has_previous", "next_cursor", "previous_cursor")

    def __init__(self, items: list[M], has_next: bool, has_previous: bool, next_cursor: str | None, previous_cursor: str | None) -> None:
        self.items = items
        self.has_next = has_next
        self.has_previous = has_previous
        self.next_cursor = next_cursor
        self.previous_cursor = previous_cursor

    def __repr__(self) -> str:
        return f"<Page items={len(self.items)} has_next={self.has_next} has_previous={self.has_previous}>"


Key = tuple["Field[Any]", Ordering]


def stored_nullable(model: type[Model], f: Field[Any]) -> bool:
    """Whether the column can hold NULL; a proxy can declare a nullable column non-null."""
    return f.nullable or f.name in model._meta.narrowed


def keyset(model: type[Model], order: tuple[Ordering, ...]) -> list[Key]:
    """The order columns of a page, with the primary key last when the order is not unique."""
    keys: list[Key] = []
    for o in order:
        e = o.expr
        if not isinstance(e, ColumnRef) or e._path or e._root is not model:
            raise QueryError(f"paginate() orders by columns of {model.__name__} itself, not by {e!r}")
        f = e._field
        nullable = stored_nullable(model, f)
        if f.type_name not in _CURSOR_TYPES:
            raise QueryError(f"paginate() can't order by {e!r}: {f.type_name} columns have no cursor value")
        if nullable and o.nulls is None:
            direction = "desc" if o.desc else "asc"
            raise QueryError(f"{e!r} is nullable: order by {e!r}.{direction}(nulls='first') or (nulls='last') to paginate")
        keys.append((f, o))
    if not any(f.primary_key or (f.unique and not stored_nullable(model, f)) for f, _ in keys):
        keys.append((model._meta.pk, Ordering(model._meta.pk_ref(), desc=False)))
    return keys


def fingerprint(model: type[Model], keys: list[Key]) -> str:
    text = model.__name__ + ":" + ",".join(f"{'-' if o.desc else ''}{f.name}{'' if o.nulls is None else ' nulls ' + o.nulls}" for f, o in keys)
    return blake2b(text.encode(), digest_size=8).hexdigest()


_INT_RANGE = {"int": 2**31, "big_int": 2**63}
# JSON has no non-finite numbers; the cursor writes them as text in both bindings.
_NONFINITE = {"Infinity": float("inf"), "-Infinity": float("-inf"), "NaN": float("nan")}


def _encode(f: Field[Any], v: Any) -> Any:
    if v is None or f.type_name in ("bool", "string", "text"):
        return v
    if f.type_name == "float":
        return v if math.isfinite(v) else {math.inf: "Infinity", -math.inf: "-Infinity"}.get(v, "NaN")
    if f.type_name in ("date_time", "date"):
        return v.isoformat()
    if f.type_name == "decimal" and not v.is_finite():
        raise QueryError(f"paginate() can't make a cursor from the decimal {v}")
    return str(v)


def _decode(f: Field[Any], nullable: bool, v: Any) -> Any:
    t = f.type_name
    if v is None:
        if not nullable:
            raise QueryError("invalid cursor")
        return None
    try:
        if t in ("big_int", "int") and isinstance(v, str) and re.fullmatch(r"-?[0-9]+", v):
            n = int(v)
            if -_INT_RANGE[t] <= n < _INT_RANGE[t]:
                return n
        elif t == "decimal" and isinstance(v, str):
            d = Decimal(v)
            if d.is_finite():
                return d
        elif t == "uuid" and isinstance(v, str):
            return UUID(v)
        elif t == "date_time" and isinstance(v, str):
            dt = datetime.fromisoformat(v)
            if dt.tzinfo is not None:
                return dt
        elif t == "date" and isinstance(v, str):
            return date.fromisoformat(v)
    except (ValueError, InvalidOperation):
        pass
    if (t in ("string", "text") and isinstance(v, str)) or (t == "bool" and isinstance(v, bool)) or (t == "float" and isinstance(v, (int, float)) and not isinstance(v, bool)):
        return v
    if t == "float" and isinstance(v, str) and v in _NONFINITE:
        return _NONFINITE[v]
    raise QueryError("invalid cursor")


def encode_cursor(fp: str, keys: list[Key], obj: Model) -> str:
    values = [_encode(f, obj._field_value(f.name)) for f, _ in keys]
    raw = json.dumps({"o": fp, "v": values}, separators=(",", ":")).encode()
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


def decode_cursor(model: type[Model], cursor: str, fp: str, keys: list[Key]) -> list[Any]:
    if not isinstance(cursor, str):
        raise TypeError(f"a cursor is a string, got {cursor!r}")
    try:
        data = json.loads(base64.urlsafe_b64decode(cursor + "=" * (-len(cursor) % 4)))
    except (ValueError, binascii.Error):
        raise QueryError("invalid cursor") from None
    if not isinstance(data, dict) or not isinstance(data.get("v"), list) or len(data["v"]) != len(keys):
        raise QueryError("invalid cursor")
    if data.get("o") != fp:
        raise QueryError("the cursor belongs to another order or model; paginate with the order that made it")
    return [_decode(f, stored_nullable(model, f), v) for (f, _), v in zip(keys, data["v"])]


def _equal(col: ColumnRef[Any], v: Any) -> Condition:
    return col.is_null() if v is None else col == v


def _beyond(col: ColumnRef[Any], o: Ordering, v: Any) -> Condition | None:
    """Rows whose `col` comes after `v` in ordering `o`; None if no row can."""
    if v is None:
        return col.is_not_null() if o.nulls == "first" else None
    step: Condition = col < v if o.desc else col > v
    return step | col.is_null() if o.nulls == "last" else step


def after(model: type[Model], order: list[Ordering], values: list[Any]) -> Condition:
    """Rows after the position `values` in `order` (the expanded form of a row comparison).

    A NOT NULL first column also gets a plain bound (`a >= v`), which an index on the
    order can use to start at the cursor; the expanded form alone reads from the start.
    """
    branches = []
    for k, (o, v) in enumerate(zip(order, values)):
        col: ColumnRef[Any] = o.expr  # type: ignore[assignment]
        step = _beyond(col, o, v)
        if step is not None:
            branches.append(and_(*(_equal(p.expr, pv) for p, pv in zip(order[:k], values[:k])), step))  # type: ignore[arg-type]
    first: ColumnRef[Any] = order[0].expr  # type: ignore[assignment]
    if len(order) > 1 and not stored_nullable(model, first._field):
        return and_(first <= values[0] if order[0].desc else first >= values[0], or_(*branches))
    return or_(*branches)
