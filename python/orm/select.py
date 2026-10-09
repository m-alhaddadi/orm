"""``select()``: queries returning chosen columns and aggregates instead of instances.

::

    rows = await (
        Post.objects.filter(Post.published)
        .select(Post.author_id, func.count().label("posts"), func.sum(Post.views).label("views"))
        .group_by(Post.author_id)
        .having(func.count() > 2)
    )
    rows[0].posts; rows[0][1]; author_id, posts, views = rows[0]

    await Post.objects.select(func.max(Post.views)).scalar()           # int | None
    await Post.objects.filter(...).select(Post.id).scalars()            # list[int]
    await Post.objects.select(Post, func.count(Post.comments))          # rows of (Post, int)

Rows are :class:`Row`: tuples whose items can also be read by name. Columns are named
after their field, ``label()``-ed expressions after the label, functions after the
function and a selected model after the model (``post``).
"""

from __future__ import annotations

import asyncio
import json
from collections.abc import AsyncIterator, Generator
from operator import itemgetter
from typing import TYPE_CHECKING, Any, Generic, TypeVar, TypeVarTuple, Unpack, cast

from ._cache import cached
from .errors import DoesNotExist, MultipleObjectsReturned, QueryError
from .expr import ColumnRef, ConditionLike, Expression, Func, IRContext, Labeled, Ordering, ScalarSubquery, _Ctes, and_

if TYPE_CHECKING:
    from collections.abc import Callable

    from typing_extensions import Self

    from .cte import Cte
    from .db import Database
    from .model import Model
    from .query import QuerySet

T = TypeVar("T")
Ts = TypeVarTuple("Ts")

__all__ = ["Row", "Select"]


# -- rows --------------------------------------------------------------------------------

class Row(tuple[Unpack[Ts]]):
    """A result row: a tuple (``row[0]``, ``a, b = row``) whose items can also be read
    by name (``row.total``; typed ``Any``). ``row._asdict()`` gives a dict."""

    __slots__ = ()
    _fields: tuple[str, ...] = ()

    if TYPE_CHECKING:

        def __getattr__(self, name: str) -> Any: ...

    def _values(self) -> tuple[Any, ...]:
        """The items as a plain tuple."""
        return tuple(cast("tuple[Any, ...]", self))

    def _asdict(self) -> dict[str, Any]:
        return dict(zip(self._fields, self._values()))

    def __repr__(self) -> str:
        return "Row(" + ", ".join(f"{n}={v!r}" for n, v in zip(self._fields, self._values())) + ")"

    def __reduce__(self) -> tuple[Any, ...]:
        return (_make_row, (self._fields, self._values()))


_row_classes: dict[tuple[str, ...], type] = {}


def row_class(names: tuple[str, ...]) -> type:
    """The ``Row`` subclass for these column names (created once, then cached)."""
    cls = _row_classes.get(names)
    if cls is None:
        ns: dict[str, Any] = {"__slots__": (), "_fields": names}
        ns.update({n: property(itemgetter(i)) for i, n in enumerate(names)})
        cls = _row_classes[names] = type("Row", (Row,), ns)
    return cls


def _make_row(names: tuple[str, ...], values: tuple[Any, ...]) -> Any:
    return tuple.__new__(row_class(names), values)


def _column_name(item: Any, i: int) -> str:
    if isinstance(item, Labeled):
        return item._name
    if isinstance(item, ColumnRef):
        return item._field.name
    if isinstance(item, Func):
        return item._name
    from .cte import CteColumn

    if isinstance(item, CteColumn):
        return item._name
    if isinstance(item, type):
        return item.__name__.lower()
    return f"_{i}"


# -- the query ---------------------------------------------------------------------------

class Select(Generic[Unpack[Ts]]):
    """A query returning rows of chosen columns. Built by ``QuerySet.select(...)``;
    every method returns a new ``Select``, and nothing runs until it is awaited. Awaiting
    the same ``Select`` again gives the same rows without querying again."""

    __slots__ = ("_qs", "_items", "_names", "_group", "_having", "_distinct", "_distinct_on", "_result")

    def __init__(self, qs: QuerySet[Any], items: tuple[Any, ...]) -> None:
        if not items:
            raise TypeError("select() needs at least one column")
        if qs._related or qs._prefetch:
            raise QueryError("select() can't be combined with load() of relations")
        for item in items:
            if isinstance(item, type):
                if item is not qs.model:
                    raise TypeError(f"select() takes {qs.model.__name__} itself or expressions, not {item.__name__}")
            elif not isinstance(item, Expression):
                raise TypeError(f"select() takes columns and expressions, got {item!r}")
        names = tuple(_column_name(item, i) for i, item in enumerate(items))
        dupes = sorted({n for n in names if names.count(n) > 1})
        if dupes:
            raise ValueError(f"select() has several columns named {', '.join(dupes)}; name them with .label()")
        self._qs = qs
        self._items = items
        self._names = names
        self._group: tuple[Expression[Any], ...] = ()
        self._having: tuple[Any, ...] = ()
        self._distinct = False
        self._distinct_on: tuple[ColumnRef[Any], ...] = ()
        self._result: asyncio.Task[list[Any]] | None = None

    def _clone(self, **changes: Any) -> Self:
        new = object.__new__(type(self))
        for k in self.__slots__:
            setattr(new, k, changes.get(k, getattr(self, k)))
        new._result = None
        return new

    # -- building (the WHERE / ORDER BY / LIMIT part goes to the query set) ---------------

    def filter(self, *conditions: ConditionLike) -> Self:
        return self._clone(_qs=self._qs.filter(*conditions))

    def exclude(self, *conditions: ConditionLike) -> Self:
        return self._clone(_qs=self._qs.exclude(*conditions))

    def order_by(self, *items: Expression[Any] | Ordering) -> Self:
        return self._clone(_qs=self._qs.order_by(*items))

    def limit(self, n: int | None) -> Self:
        return self._clone(_qs=self._qs.limit(n))

    def offset(self, n: int | None) -> Self:
        return self._clone(_qs=self._qs.offset(n))

    def __getitem__(self, s: slice) -> Self:
        return self._clone(_qs=self._qs[s])

    def using(self, db: Database | None) -> Self:
        return self._clone(_qs=self._qs.using(db))

    def group_by(self, *exprs: Expression[Any] | type[Model]) -> Self:
        """``GROUP BY``: aggregates in the select list then summarize each group. The
        model itself (``group_by(Post)``) groups by its primary key."""
        out = []
        for e in exprs:
            if isinstance(e, type):
                if e is not self._qs.model:
                    raise TypeError(f"group_by() takes {self._qs.model.__name__} itself or expressions")
                out.append(e._meta.pk_ref())
            elif isinstance(e, Expression):
                out.append(e)
            else:
                raise TypeError(f"group_by() takes columns and expressions, got {e!r}")
        return self._clone(_group=(*self._group, *out))

    def having(self, *conditions: ConditionLike) -> Self:
        """Keep groups matching all ``conditions``: ``having(func.count() > 2)``."""
        if not conditions:
            return self._clone()
        return self._clone(_having=(*self._having, and_(*conditions)))

    def distinct(self, *on: ColumnRef[Any]) -> Self:
        """``SELECT DISTINCT``; with columns, Postgres' ``DISTINCT ON (...)``: the first
        row (by ``order_by``) of each distinct value."""
        return self._clone(_distinct=True, _distinct_on=on)

    # -- IR -------------------------------------------------------------------------------

    def _build(
        self, params: list[Any], outer: IRContext | None = None, ctes: _Ctes | None = None
    ) -> tuple[dict[str, Any], IRContext]:
        ir, ctx = self._qs._query_ir("select", params, outer, ctes)
        ir["columns"] = [
            {"t": "model"} if isinstance(item, type) else {"t": "expr", "expr": item._ir(ctx), "name": name}
            for item, name in zip(self._items, self._names)
        ]
        if self._group:
            ir["group_by"] = [g._ir(ctx) for g in self._group]
        if self._having:
            ir["having"] = [h._ir(ctx) for h in self._having]
        if self._distinct_on:
            ir["distinct_on"] = [c._ir(ctx) for c in self._distinct_on]
        elif self._distinct:
            ir["distinct"] = True
        ctx.add_windows(ir)
        return ir, ctx

    def _ir(self, params: list[Any]) -> dict[str, Any]:
        ir, ctx = self._build(params)
        return ctx.finish(ir)

    def _subquery_ir(self, ctx: IRContext, what: str) -> dict[str, Any]:
        if what != "exists()" and (len(self._items) != 1 or isinstance(self._items[0], type)):
            raise QueryError(f"{what} takes a query that selects exactly one column")
        if self._qs._lock is not None:
            raise QueryError("a subquery can't lock rows")
        return self._build(ctx.params, outer=ctx)[0]

    def _cte_ir(self, params: list[Any], ctes: _Ctes) -> dict[str, Any]:
        return self._build(params, ctes=ctes)[0]

    def as_scalar(self: Select[T]) -> ScalarSubquery[T]:
        """This one-column query as a value in another query: ``(SELECT ...)``. It must
        return at most one row (slice it with ``[:1]``); no row is ``NULL``. Correlate it
        with :func:`orm.outer`::

            latest = (Post.objects.filter(Post.author_id == outer(User.id))
                      .order_by(Post.created_at.desc()).select(Post.title)[:1].as_scalar())
            await User.objects.select(User, latest.label("latest"))
        """
        self._one_column("as_scalar")
        return ScalarSubquery(self)

    def cte(
        self,
        name: str,
        *,
        recursive: Callable[[Cte], Any] | None = None,
        distinct: bool = False,
        materialized: bool | None = None,
    ) -> Cte:
        """This query as a CTE (``WITH <name> AS (...)``) whose columns are the selected
        items' names (``cte.c.<name>``; a selected model gives its fields). See
        :mod:`orm.cte`."""
        from .cte import Cte

        return Cte(name, self, recursive=recursive, distinct=distinct, materialized=materialized)

    def sql(self) -> str:
        """The SELECT this runs, with parameters inlined (for debugging)."""
        params: list[Any] = []
        from .db import _with_scope

        ir = self._ir(params)
        sql: str = self._qs._native().sql(*_with_scope(json.dumps(ir), params))
        return sql

    # -- execution ------------------------------------------------------------------------

    async def _rows(self) -> list[Any]:
        qs = self._qs
        params: list[Any] = []
        ir = self._ir(params)
        qs._check_lock()
        rows: list[Any] = await qs._run(ir, params, row_class(self._names))
        return rows

    def __await__(self) -> Generator[Any, None, list[Row[Unpack[Ts]]]]:
        return cached(self, self._rows).__await__()

    async def __aiter__(self) -> AsyncIterator[Row[Unpack[Ts]]]:
        for row in await self:
            yield row

    async def first(self) -> Row[Unpack[Ts]] | None:
        """The first row (by ``order_by``; unordered otherwise), or ``None``."""
        rows = await self[:1]._rows()
        return rows[0] if rows else None

    async def one(self) -> Row[Unpack[Ts]]:
        """The only row; raises ``DoesNotExist`` / ``MultipleObjectsReturned`` otherwise."""
        rows = await self.limit(2)._rows()
        if not rows:
            raise DoesNotExist("the query returned no row")
        if len(rows) > 1:
            raise MultipleObjectsReturned("the query returned more than one row")
        return rows[0]  # type: ignore[no-any-return]

    async def scalar(self: Select[T]) -> T | None:
        """The single column of the first row, or ``None`` without rows:
        ``await Post.objects.select(func.max(Post.views)).scalar()``."""
        self._one_column("scalar")
        rows = await self[:1]._rows()
        return rows[0][0] if rows else None

    async def scalars(self: Select[T]) -> list[T]:
        """The single column of every row: ``await qs.select(Post.id).scalars()``."""
        self._one_column("scalars")
        return [r[0] for r in await self._rows()]

    def _one_column(self, what: str) -> None:
        if len(self._items) != 1:
            raise QueryError(f"{what}() needs a query selecting one column, this one selects {len(self._items)}")

    def __repr__(self) -> str:
        parts = [f"select({', '.join(self._names)})"]
        if self._group:
            parts.append(f"group_by{self._group!r}")
        if self._having:
            parts.append(f"having{self._having!r}")
        return f"<Select {self._qs!r} {'.'.join(parts)}>"
