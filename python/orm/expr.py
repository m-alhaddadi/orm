"""Expression tree for filters, ordering and updates.

``User.email`` evaluates to a :class:`ColumnRef`; comparing it builds a
:class:`Condition`. ``User.posts`` evaluates to a :class:`RelationPath`, whose
attributes are the related model's columns and relations, so
``User.posts.created_at < yesterday`` is a condition on a column reached through a
relation. Nothing here talks to the database: expressions compile to the IR
dictionaries consumed by the native engine (see ``core/src/ir.rs``).
"""

from __future__ import annotations

from collections.abc import Callable, Iterable
from datetime import datetime
from decimal import Decimal
from typing import TYPE_CHECKING, Any, Generic, TypeVar, Union, Unpack, overload

from .errors import QueryError

if TYPE_CHECKING:
    from .cte import Cte
    from .fields import Field
    from .model import Model
    from .query import QuerySet
    from .select import Select

T = TypeVar("T")
E = TypeVar("E")
M = TypeVar("M", bound="Model")

IR = dict[str, Any]

__all__ = [
    "Expression",
    "ColumnRef",
    "RelationPath",
    "Condition",
    "Ordering",
    "Literal",
    "Param",
    "param",
    "Excluded",
    "excluded",
    "Func",
    "Labeled",
    "Window",
    "WindowDef",
    "window",
    "ScalarSubquery",
    "func",
    "outer",
    "exists",
    "and_",
    "or_",
    "not_",
]


class _Ctes:
    """The CTEs one statement uses, in dependency order, compiled once each."""

    __slots__ = ("items", "building")

    def __init__(self) -> None:
        self.items: list[tuple[Cte, IR]] = []
        self.building: list[Cte] = []

    def use(self, cte: Cte, params: list[Any]) -> None:
        for c, _ in self.items:
            if c is cte:
                return
            if c.name == cte.name:
                raise ValueError(f"two different CTEs are named {cte.name!r} in one query")
        if any(c is cte for c in self.building):
            return  # the recursive part of `cte` reading `cte`
        self.building.append(cte)
        try:
            ir = cte._body_ir(params, self)
        finally:
            self.building.pop()
        self.items.append((cte, ir))


class IRContext:
    """Collects literal parameters and CTEs while compiling, and checks that every
    column belongs to the query's root model (or CTE). ``outer`` is the context of the
    enclosing query, for subqueries."""

    __slots__ = ("root", "params", "outer", "ctes", "windows", "_owner")

    def __init__(
        self, root: type[Model] | Cte, params: list[Any], outer: IRContext | None = None, ctes: _Ctes | None = None
    ) -> None:
        self.root = root
        self.params = params
        self.outer = outer
        self._owner = outer is None and ctes is None
        self.ctes: _Ctes = ctes if ctes is not None else (outer.ctes if outer is not None else _Ctes())
        # Named windows of this query (not of enclosing ones), with their IR.
        self.windows: list[tuple[WindowDef, IR]] = []

    def param(self, value: Any) -> IR:
        self.params.append(value)
        return {"t": "param", "i": len(self.params) - 1}

    def use_cte(self, cte: Cte) -> None:
        self.ctes.use(cte, self.params)

    def window_name(self, w: WindowDef) -> str:
        """The name of ``w`` in this query's ``WINDOW`` clause (declared on first use)."""
        for i, (known, _) in enumerate(self.windows):
            if known is w:
                return f"w{i + 1}"
        name = f"w{len(self.windows) + 1}"
        self.windows.append((w, {"name": name, **w._spec_ir(self)}))
        return name

    def add_windows(self, ir: IR) -> None:
        if self.windows:
            ir["windows"] = [w for _, w in self.windows]

    def finish(self, ir: IR) -> IR:
        """Declares the CTEs the statement uses (``WITH``), if this context compiles the
        statement itself rather than a part of it."""
        if self._owner and self.ctes.items:
            ir["with"] = [c for _, c in self.ctes.items]
        return ir


def _bool_misuse(self: object) -> bool:
    raise TypeError(
        "ORM expressions have no truth value; combine conditions with & | ~ "
        "instead of and / or / not, and compare with None using == / !=."
    )


class Node:
    __slots__ = ()

    def _ir(self, ctx: IRContext) -> IR:
        raise NotImplementedError

    __bool__ = _bool_misuse


def _wrap(value: Any) -> Node:
    return value if isinstance(value, Node) else Literal(value)


class Expression(Node, Generic[T]):
    """A SQL value expression whose Python type is ``T``."""

    __slots__ = ()

    # Comparisons -----------------------------------------------------------------------
    # `__eq__` / `__ne__` return conditions, SQLAlchemy style.

    def __eq__(self, other: T | Expression[T] | None) -> Condition:  # type: ignore[override]
        if other is None:
            return IsNull(self, neg=False)
        return Comparison("eq", self, _wrap(other))

    def __ne__(self, other: T | Expression[T] | None) -> Condition:  # type: ignore[override]
        if other is None:
            return IsNull(self, neg=True)
        return Comparison("ne", self, _wrap(other))

    def __lt__(self, other: T | Expression[T]) -> Condition:
        return Comparison("lt", self, _wrap(other))

    def __le__(self, other: T | Expression[T]) -> Condition:
        return Comparison("le", self, _wrap(other))

    def __gt__(self, other: T | Expression[T]) -> Condition:
        return Comparison("gt", self, _wrap(other))

    def __ge__(self, other: T | Expression[T]) -> Condition:
        return Comparison("ge", self, _wrap(other))

    __hash__ = object.__hash__

    def in_(self, values: Iterable[T] | Select[T]) -> Condition:
        """``IN (values)``, or ``IN (SELECT ...)`` for a one-column ``select()`` query."""
        from .select import Select

        if isinstance(values, Select):
            return InSelect(self, values, neg=False)
        if isinstance(values, Param):
            raise TypeError("in_() can't take a param(); the number of values is part of the query")
        values = list(values)
        if not values:
            return Const(False)
        return In(self, [_wrap(v) for v in values], neg=False)

    def not_in(self, values: Iterable[T] | Select[T]) -> Condition:
        from .select import Select

        if isinstance(values, Select):
            return InSelect(self, values, neg=True)
        if isinstance(values, Param):
            raise TypeError("not_in() can't take a param(); the number of values is part of the query")
        values = list(values)
        if not values:
            return Const(True)
        return In(self, [_wrap(v) for v in values], neg=True)

    def is_null(self) -> Condition:
        return IsNull(self, neg=False)

    def is_not_null(self) -> Condition:
        return IsNull(self, neg=True)

    def between(self, low: T | Expression[T], high: T | Expression[T]) -> Condition:
        return (self >= low) & (self <= high)

    # String matching ---------------------------------------------------------------------

    def like(self: Expression[str] | Expression[str | None], pattern: str) -> Condition:
        return Like(self, pattern, ci=False)

    def ilike(self: Expression[str] | Expression[str | None], pattern: str) -> Condition:
        return Like(self, pattern, ci=True)

    def contains(self: Expression[str] | Expression[str | None], text: str) -> Condition:
        return Like(self, _pattern(text, _contains), ci=False)

    def icontains(self: Expression[str] | Expression[str | None], text: str) -> Condition:
        return Like(self, _pattern(text, _contains), ci=True)

    def startswith(self: Expression[str] | Expression[str | None], text: str) -> Condition:
        return Like(self, _pattern(text, _startswith), ci=False)

    def endswith(self: Expression[str] | Expression[str | None], text: str) -> Condition:
        return Like(self, _pattern(text, _endswith), ci=False)

    # Arrays ---------------------------------------------------------------------------------

    def has(self: Expression[list[E]] | Expression[list[E] | None], value: E) -> Condition:
        """Array columns: ``value`` is one of the elements (``col @> ARRAY[value]``)."""
        if isinstance(value, Param):
            return Comparison("contains", self, _Bound(value, _one_element))
        return Comparison("contains", self, Literal([value]))

    def has_all(self: Expression[list[E]] | Expression[list[E] | None], values: Iterable[E]) -> Condition:
        """Array columns: every one of ``values`` is an element (``col @> values``)."""
        return Comparison("contains", self, Literal(list(values)))

    def has_any(self: Expression[list[E]] | Expression[list[E] | None], values: Iterable[E]) -> Condition:
        """Array columns: at least one of ``values`` is an element (``col && values``)."""
        return Comparison("overlaps", self, Literal(list(values)))

    def contained_by(self: Expression[list[E]] | Expression[list[E] | None], values: Iterable[E]) -> Condition:
        """Array columns: every element is one of ``values`` (``col <@ values``)."""
        return Comparison("contained_by", self, Literal(list(values)))

    # Arithmetic ----------------------------------------------------------------------------

    def __add__(self, other: T | Expression[T]) -> Expression[T]:
        return Arith("add", self, _wrap(other))

    def __sub__(self, other: T | Expression[T]) -> Expression[T]:
        return Arith("sub", self, _wrap(other))

    def __mul__(self, other: T | Expression[T]) -> Expression[T]:
        return Arith("mul", self, _wrap(other))

    def __truediv__(self, other: T | Expression[T]) -> Expression[T]:
        return Arith("div", self, _wrap(other))

    def __radd__(self, other: T) -> Expression[T]:
        return Arith("add", _wrap(other), self)

    def __rsub__(self, other: T) -> Expression[T]:
        return Arith("sub", _wrap(other), self)

    def __rmul__(self, other: T) -> Expression[T]:
        return Arith("mul", _wrap(other), self)

    # Boolean columns used directly as conditions: `filter(Post.published & ...)`.

    def __and__(self: Expression[bool], other: Condition | Expression[bool]) -> Condition:
        return as_condition(self) & other

    def __or__(self: Expression[bool], other: Condition | Expression[bool]) -> Condition:
        return as_condition(self) | other

    def __invert__(self: Expression[bool]) -> Condition:
        return ~as_condition(self)

    # Naming ----------------------------------------------------------------------------------

    def label(self, name: str) -> Labeled[T]:
        """The name of this expression in ``select()`` rows: ``row.<name>``."""
        return Labeled(self, name)

    # Ordering --------------------------------------------------------------------------------

    def asc(self) -> Ordering:
        return Ordering(self, desc=False)

    def desc(self) -> Ordering:
        return Ordering(self, desc=True)


def _escape_like(text: str) -> str:
    return text.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")


def _contains(text: str) -> str:
    return f"%{_escape_like(text)}%"


def _startswith(text: str) -> str:
    return f"{_escape_like(text)}%"


def _endswith(text: str) -> str:
    return f"%{_escape_like(text)}"


def _one_element(value: Any) -> list[Any]:
    return [value]


def _pattern(text: Any, make: Callable[[str], str]) -> Any:
    """The LIKE pattern for ``text``, or for a :func:`param` once its value is known."""
    return _Bound(text, make) if isinstance(text, Param) else make(text)


class _Bound(Expression[Any]):
    """A :func:`param` whose value goes through ``transform`` when bound."""

    __slots__ = ("param", "transform")

    def __init__(self, param: Param, transform: Callable[[Any], Any]) -> None:
        self.param = param
        self.transform = transform

    def _ir(self, ctx: IRContext) -> IR:
        return self.param._slot(ctx.params, self.transform)


class Literal(Expression[Any]):
    __slots__ = ("value",)

    def __init__(self, value: Any) -> None:
        self.value = value

    def _ir(self, ctx: IRContext) -> IR:
        return ctx.param(self.value)

    def __repr__(self) -> str:
        return repr(self.value)


class _Slot:
    """A placeholder in a prepared query's parameter list, filled in per call:
    ``transform`` turns the call's value into the parameter (a LIKE pattern, ...)."""

    __slots__ = ("name", "transform")

    def __init__(self, name: str, transform: Callable[[Any], Any] | None = None) -> None:
        self.name = name
        self.transform = transform

    def __repr__(self) -> str:
        return f"param({self.name!r})"


class SlotParams(list[Any]):
    """The parameter list of a query compiled by ``prepare()``: the only one that
    accepts :func:`param` placeholders."""

    __slots__ = ()


class Param(Expression[Any]):
    """A value supplied when a prepared query runs; see :func:`param`."""

    __slots__ = ("name",)

    def __init__(self, name: str) -> None:
        if not name.isidentifier():
            raise ValueError(f"param() takes an identifier, got {name!r}")
        self.name = name

    def _slot(self, ctx_params: list[Any], transform: Callable[[Any], Any] | None = None) -> IR:
        if not isinstance(ctx_params, SlotParams):
            raise QueryError(f"{self!r} is a placeholder of a prepared query; call .prepare() on the query set")
        ctx_params.append(_Slot(self.name, transform))
        return {"t": "param", "i": len(ctx_params) - 1}

    def _ir(self, ctx: IRContext) -> IR:
        return self._slot(ctx.params)

    def __repr__(self) -> str:
        return f"param({self.name!r})"


def param(name: str) -> Any:
    """A placeholder for a value given each time a prepared query runs::

        by_author = Post.objects.filter(Post.author_id == param("author")).limit(param("n")).prepare()
        posts = await by_author(author=3, n=10)

    It stands for a value in comparisons, arithmetic, ``like`` / ``contains`` / ...,
    ``has()``, ``limit()`` and ``offset()``. Typed as ``Any`` so it fits wherever a
    value does."""
    return Param(name)


class ColumnRef(Expression[T]):
    """A column of ``root``'s model, or of a model reached from it through ``path``."""

    __slots__ = ("_root", "_path", "_field")

    def __init__(self, root: type[Model], path: tuple[str, ...], field: Field[Any]) -> None:
        self._root = root
        self._path = path
        self._field = field

    def _ir(self, ctx: IRContext) -> IR:
        if self._root is not ctx.root:
            root = ctx.root.__name__ if isinstance(ctx.root, type) else f"CTE {ctx.root.name}"
            hint = f"reach it through a relation of {root} instead"
            c = ctx.outer
            while c is not None:
                if c.root is self._root:
                    hint = f"use outer({self!r}) for a column of the enclosing query"
                    break
                c = c.outer
            raise ValueError(f"{self!r} belongs to {self._root.__name__}, not to a {root} query; {hint}")
        return {"t": "col", "path": list(self._path), "name": self._field.name}

    def __neg__(self) -> Ordering:
        """``-Post.created_at`` is ``Post.created_at.desc()``, for ``order_by()``. It is
        never SQL negation; write ``0 - Post.views`` for that."""
        return Ordering(self, desc=True)

    def __repr__(self) -> str:
        return ".".join((self._root.__name__, *self._path, self._field.name))


class Excluded(Expression[T]):
    """``excluded(Post.views)``: the value a conflicting insert proposed for a column,
    usable in ``on_conflict(...).do_update(views=Post.views + excluded(Post.views))``."""

    __slots__ = ("_column",)

    def __init__(self, column: ColumnRef[T]) -> None:
        if not isinstance(column, ColumnRef) or column._path:
            raise TypeError(f"excluded() takes a column of the inserted model, got {column!r}")
        self._column = column

    def _ir(self, ctx: IRContext) -> IR:
        self._column._ir(ctx)  # checks the model
        return {"t": "excluded", "name": self._column._field.name}

    def __repr__(self) -> str:
        return f"excluded({self._column!r})"


class Labeled(Expression[T]):
    __slots__ = ("_inner", "_name")

    def __init__(self, inner: Node, name: str) -> None:
        if not name.isidentifier() or name.startswith("_"):
            raise ValueError(f"label {name!r} must be an identifier not starting with '_'")
        self._inner = inner
        self._name = name

    def _ir(self, ctx: IRContext) -> IR:
        return self._inner._ir(ctx)

    def __repr__(self) -> str:
        return f"{self._inner!r}.label({self._name!r})"


class Func(Expression[T]):
    """A SQL function call; build it with :data:`func`."""

    __slots__ = ("_name", "_args", "_rel", "_distinct")

    def __init__(
        self, name: str, args: tuple[Any, ...] = (), rel: RelationPath[Any] | None = None, distinct: bool = False
    ) -> None:
        self._name = name
        self._args = tuple(_wrap(a) for a in args)
        self._rel = rel
        self._distinct = distinct

    def _ir(self, ctx: IRContext) -> IR:
        ir: IR = {"t": "func", "name": self._name, "args": [a._ir(ctx) for a in self._args]}
        if self._rel is not None:
            if self._rel._root is not ctx.root:
                root = ctx.root.__name__ if isinstance(ctx.root, type) else ctx.root.name
                raise ValueError(f"{self._rel!r} does not start at {root}")
            ir["rel"] = list(self._rel._path)
        if self._distinct:
            ir["distinct"] = True
        return ir

    def __repr__(self) -> str:
        args = [repr(a) for a in self._args] + ([repr(self._rel)] if self._rel is not None else [])
        return f"func.{self._name}({', '.join(args)})"

    def over(
        self,
        window: WindowDef | Expression[Any] | Iterable[Expression[Any]] | None = None,
        /,
        order_by: Expression[Any] | Ordering | Iterable[Expression[Any] | Ordering] | None = None,
        *,
        partition_by: Expression[Any] | Iterable[Expression[Any]] | None = None,
        rows: tuple[int | None, int | None] | None = None,
        range: tuple[int | None, int | None] | None = None,
    ) -> Window[T]:
        """``<function> OVER (...)``: the function computed over a window of rows related
        to each row, without grouping them::

            func.row_number().over(partition_by=Post.author_id, order_by=Post.views.desc())
            func.sum(Post.views).over(order_by=Post.created_at, rows=(None, 0))  # running total

            w = window(partition_by=Post.author_id, order_by=Post.created_at)  # shared
            Post.objects.select(func.sum(Post.views).over(w), func.avg(Post.views).over(w))

        Over a :func:`window`, ``order_by`` and ``rows`` / ``range`` may extend it when it
        has none of its own (``OVER (w ROWS ...)``); its partitioning is fixed. ``rows`` /
        ``range`` set the frame as ``(start, end)``: ``None`` is unbounded, ``0`` the
        current row, ``-n`` n preceding, ``n`` n following. A first positional argument
        that isn't a window is ``partition_by``.
        """
        frame = _frame(rows, range)
        if not isinstance(window, WindowDef):
            if window is not None:
                if partition_by is not None:
                    raise TypeError("over() got partition_by twice")
                partition_by = window
            return Window(self, None, _many(partition_by), _orderings(order_by), frame)
        if partition_by is not None:
            raise ValueError("over(window) can't add partition_by: the window defines it")
        if order_by is not None and window._order:
            raise ValueError("over(window, order_by=...) needs a window without its own order_by")
        if frame is not None and window._frame is not None:
            raise ValueError("over(window, rows/range=...) needs a window without its own frame")
        return Window(self, window, [], _orderings(order_by), frame)


def _frame(rows: tuple[int | None, int | None] | None, range_: tuple[int | None, int | None] | None) -> IR | None:
    if rows is not None and range_ is not None:
        raise ValueError("over() takes rows or range, not both")
    frame = None
    for kind, bounds in (("rows", rows), ("range", range_)):
        if bounds is not None:
            start, end = bounds
            for b in (start, end):
                if b is not None and (isinstance(b, bool) or not isinstance(b, int)):
                    raise TypeError(f"frame bounds are ints or None, got {b!r}")
            frame = {"kind": kind, "start": start, "end": end}
    return frame


class WindowDef:
    """A window definition shared by several window functions of a query: built by
    :func:`window`, used with ``func.<name>(...).over(w)``. It becomes the query's
    ``WINDOW w1 AS (...)`` clause."""

    __slots__ = ("_partition", "_order", "_frame")

    def __init__(self, partition: list[Expression[Any]], order: list[Ordering], frame: IR | None) -> None:
        self._partition = partition
        self._order = order
        self._frame = frame

    def _spec_ir(self, ctx: IRContext) -> IR:
        ir: IR = {}
        if self._partition:
            ir["partition_by"] = [p._ir(ctx) for p in self._partition]
        if self._order:
            ir["order_by"] = [o._ir(ctx) for o in self._order]
        if self._frame is not None:
            ir["frame"] = self._frame
        return ir

    def __repr__(self) -> str:
        return f"window(partition_by={self._partition!r}, order_by={self._order!r})"


def window(
    partition_by: Expression[Any] | Iterable[Expression[Any]] | None = None,
    order_by: Expression[Any] | Ordering | Iterable[Expression[Any] | Ordering] | None = None,
    *,
    rows: tuple[int | None, int | None] | None = None,
    range: tuple[int | None, int | None] | None = None,
) -> WindowDef:
    """A named window (``WINDOW w AS (PARTITION BY ... ORDER BY ...)``) for several
    window functions to share: ``func.sum(x).over(w)``, ``func.avg(x).over(w)``."""
    return WindowDef(_many(partition_by), _orderings(order_by), _frame(rows, range))


def _many(items: Any) -> list[Any]:
    if items is None:
        return []
    if isinstance(items, (Expression, Ordering)):
        return [items]
    return list(items)


def _orderings(items: Any) -> list[Ordering]:
    return [i if isinstance(i, Ordering) else Ordering(i, desc=False) for i in _many(items)]


class Window(Expression[T]):
    """``func.<name>(...).over(...)``: a window function call."""

    __slots__ = ("_func", "_base", "_partition", "_order", "_frame")

    def __init__(
        self,
        fn: Func[T],
        base: WindowDef | None,
        partition: list[Expression[Any]],
        order: list[Ordering],
        frame: IR | None,
    ) -> None:
        self._func = fn
        self._base = base
        self._partition = partition
        self._order = order
        self._frame = frame

    def _ir(self, ctx: IRContext) -> IR:
        ir: IR = {"t": "window", "func": self._func._ir(ctx)}
        if self._base is not None:
            ir["base"] = ctx.window_name(self._base)
        if self._partition:
            ir["partition_by"] = [p._ir(ctx) for p in self._partition]
        if self._order:
            ir["order_by"] = [o._ir(ctx) for o in self._order]
        if self._frame is not None:
            ir["frame"] = self._frame
        return ir

    def __repr__(self) -> str:
        return f"{self._func!r}.over(partition_by={self._partition!r}, order_by={self._order!r})"


class _Int(Expression[int]):
    """An integer written into the SQL text (window function arguments)."""

    __slots__ = ("value",)

    def __init__(self, value: int) -> None:
        if isinstance(value, bool) or not isinstance(value, int):
            raise TypeError(f"expected an int, got {value!r}")
        self.value = value

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "int", "value": self.value}

    def __repr__(self) -> str:
        return repr(self.value)


class Outer(Expression[T]):
    """``outer(User.id)``: a column of an enclosing query, in a subquery."""

    __slots__ = ("_column",)

    def __init__(self, column: ColumnRef[T]) -> None:
        if not isinstance(column, ColumnRef) or column._path:
            raise TypeError(f"outer() takes a column of a model (no relation path), got {column!r}")
        self._column = column

    def _ir(self, ctx: IRContext) -> IR:
        depth, c = 1, ctx.outer
        while c is not None:
            if c.root is self._column._root:
                return {"t": "outer", "depth": depth, "name": self._column._field.name}
            depth, c = depth + 1, c.outer
        raise ValueError(f"{self!r} is not a column of an enclosing query")

    def __repr__(self) -> str:
        return f"outer({self._column!r})"


class ScalarSubquery(Expression[T]):
    """``qs.select(x).as_scalar()``: a one-column subquery used as a value."""

    __slots__ = ("_select",)

    def __init__(self, select: Select[T]) -> None:
        self._select = select

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "subquery", "select": self._select._subquery_ir(ctx, "as_scalar()")}

    def __repr__(self) -> str:
        return f"({self._select!r}).as_scalar()"


N = TypeVar("N", int, float, Decimal)


class _Functions:
    """``func.count(...)``, ``func.sum(...)``, ...: SQL functions as expressions.

    An aggregate over a relation path is computed per row of the query, in a correlated
    subquery: ``func.count(User.posts)`` is each user's number of posts, and
    ``func.sum(User.posts.views)`` their posts' total views. Over the model's own
    columns, aggregates summarize the rows of each ``group_by()`` group (or all rows).
    """

    __slots__ = ()

    def count(self, what: Expression[Any] | RelationPath[Any] | None = None, *, distinct: bool = False) -> Func[int]:
        """``COUNT(*)`` without argument, ``COUNT(expr)`` (non-NULL values), or the rows
        of a relation: ``func.count(User.posts)``."""
        if isinstance(what, RelationPath):
            return Func("count", rel=what)
        return Func("count", () if what is None else (what,), distinct=distinct)

    @overload
    def sum(self, expr: Expression[N], *, distinct: bool = False) -> Func[N | None]: ...
    @overload
    def sum(self, expr: Expression[N | None], *, distinct: bool = False) -> Func[N | None]: ...
    def sum(self, expr: Expression[Any], *, distinct: bool = False) -> Func[Any]:
        """``SUM``; integer sums come back as ``int`` (cast to bigint)."""
        return Func("sum", (expr,), distinct=distinct)

    @overload
    def avg(  # type: ignore[overload-overlap]  # pyright: ignore[reportOverlappingOverload]
        self, expr: Expression[Decimal] | Expression[Decimal | None], *, distinct: bool = False
    ) -> Func[Decimal | None]: ...
    @overload
    def avg(
        self, expr: Expression[int] | Expression[int | None] | Expression[float] | Expression[float | None], *, distinct: bool = False
    ) -> Func[float | None]: ...
    def avg(self, expr: Expression[Any], *, distinct: bool = False) -> Func[Any]:
        """``AVG``: a ``float``, or a ``Decimal`` for decimal columns (exact)."""
        return Func("avg", (expr,), distinct=distinct)

    def min(self, expr: Expression[T]) -> Func[T | None]:
        return Func("min", (expr,))

    def max(self, expr: Expression[T]) -> Func[T | None]:
        return Func("max", (expr,))

    def lower(self, expr: Expression[str] | Expression[str | None]) -> Func[str]:
        return Func("lower", (expr,))

    def upper(self, expr: Expression[str] | Expression[str | None]) -> Func[str]:
        return Func("upper", (expr,))

    def length(self, expr: Expression[str] | Expression[str | None]) -> Func[int]:
        return Func("length", (expr,))

    def cardinality(self, expr: Expression[list[Any]] | Expression[list[Any] | None]) -> Func[int]:
        """The number of elements of an array."""
        return Func("cardinality", (expr,))

    def abs(self, expr: Expression[T]) -> Func[T]:
        return Func("abs", (expr,))

    def coalesce(self, expr: Expression[T | None], default: T | Expression[T]) -> Func[T]:
        return Func("coalesce", (expr, default))

    def now(self) -> Func[datetime]:
        return Func("now")

    # Window functions: only valid with .over(...).

    def row_number(self) -> Func[int]:
        """1, 2, 3, ... in the window's order."""
        return Func("row_number")

    def rank(self) -> Func[int]:
        """Rank with gaps: ties share a rank, the next rank skips (1, 1, 3)."""
        return Func("rank")

    def dense_rank(self) -> Func[int]:
        """Rank without gaps (1, 1, 2)."""
        return Func("dense_rank")

    def percent_rank(self) -> Func[float]:
        return Func("percent_rank")

    def cume_dist(self) -> Func[float]:
        return Func("cume_dist")

    def ntile(self, buckets: int) -> Func[int]:
        """The bucket (1..buckets) of the row when the window is split evenly."""
        return Func("ntile", (_Int(buckets),))

    def lag(self, expr: Expression[T], offset: int = 1, default: T | None = None) -> Func[T | None]:
        """``expr`` on the row ``offset`` rows before this one, ``default`` if none."""
        args: tuple[Any, ...] = (expr, _Int(offset)) if default is None else (expr, _Int(offset), default)
        return Func("lag", args)

    def lead(self, expr: Expression[T], offset: int = 1, default: T | None = None) -> Func[T | None]:
        """``expr`` on the row ``offset`` rows after this one, ``default`` if none."""
        args: tuple[Any, ...] = (expr, _Int(offset)) if default is None else (expr, _Int(offset), default)
        return Func("lead", args)

    def first_value(self, expr: Expression[T]) -> Func[T]:
        return Func("first_value", (expr,))

    def last_value(self, expr: Expression[T]) -> Func[T]:
        return Func("last_value", (expr,))

    def nth_value(self, expr: Expression[T], n: int) -> Func[T | None]:
        return Func("nth_value", (expr, _Int(n)))


func = _Functions()


def outer(column: ColumnRef[T]) -> Outer[T]:
    """A column of the enclosing query, inside a subquery (Django's ``OuterRef``)::

        await User.objects.filter(exists(Post.objects.filter(Post.author_id == outer(User.id))))

    It refers to the nearest enclosing query over the column's model.
    """
    return Outer(column)


def exists(query: QuerySet[Any] | Select[Unpack[tuple[Any, ...]]]) -> Condition:
    """``EXISTS (<query>)``, a condition: true when the query has a row. Correlate it
    with :func:`outer`; negate it with ``~``."""
    return Exists(query)


def excluded(column: ColumnRef[T]) -> Excluded[T]:
    """The row an upsert tried to insert: ``EXCLUDED.<column>`` in ``DO UPDATE``."""
    return Excluded(column)


class RelationPath(Generic[M]):
    """``User.posts``: a relation reached from ``root``. Attribute access continues the
    path: columns of the related model give :class:`ColumnRef`, relations give a
    longer :class:`RelationPath`. Used in filters, ``select_related`` and
    ``prefetch_related``.
    """

    __slots__ = ("_root", "_path", "_target")

    def __init__(self, root: type[Model], path: tuple[str, ...], target: type[M]) -> None:
        self._root = root
        self._path = path
        self._target = target

    # Hidden from type checkers: generated stubs declare each model's path class with
    # its real attributes, so unknown names are reported instead of typed as Any.
    if not TYPE_CHECKING:

        def __getattr__(self, name: str) -> Any:
            if name.startswith("_"):
                raise AttributeError(name)
            meta = self._target._meta
            if name in meta.fields:
                return ColumnRef(self._root, self._path, meta.fields[name])
            if name in meta.relations:
                rel = meta.relations[name]
                return RelationPath(self._root, (*self._path, name), rel.target)
            raise AttributeError(f"{self._target.__name__} has no field or relation {name!r}")

    def __dir__(self) -> Iterable[str]:
        meta = self._target._meta
        return [*meta.fields, *meta.relations]

    def __repr__(self) -> str:
        return ".".join((self._root.__name__, *self._path))

    __bool__ = _bool_misuse


class Ordering:
    __slots__ = ("expr", "desc")

    def __init__(self, expr: Expression[Any], desc: bool) -> None:
        self.expr = expr
        self.desc = desc

    def reversed(self) -> Ordering:
        return Ordering(self.expr, not self.desc)

    def _ir(self, ctx: IRContext) -> IR:
        return {"expr": self.expr._ir(ctx), "desc": self.desc}

    def __repr__(self) -> str:
        return f"{self.expr!r}.{'desc' if self.desc else 'asc'}()"


# -- conditions ---------------------------------------------------------------------------


class Condition(Node):
    """A boolean SQL condition. Combine with ``&``, ``|`` and ``~``."""

    __slots__ = ()

    def __and__(self, other: Condition | Expression[bool]) -> Condition:
        return and_(self, other)

    def __rand__(self, other: Expression[bool]) -> Condition:
        return and_(other, self)

    def __or__(self, other: Condition | Expression[bool]) -> Condition:
        return or_(self, other)

    def __ror__(self, other: Expression[bool]) -> Condition:
        return or_(other, self)

    def __invert__(self) -> Condition:
        return not_(self)

    def label(self, name: str) -> Labeled[bool]:
        """The condition as a named boolean column in ``select()``."""
        return Labeled(self, name)


class Comparison(Condition):
    __slots__ = ("op", "left", "right")

    def __init__(self, op: str, left: Node, right: Node) -> None:
        self.op = op
        self.left = left
        self.right = right

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "cmp", "op": self.op, "l": self.left._ir(ctx), "r": self.right._ir(ctx)}

    def __repr__(self) -> str:
        sym = {"eq": "==", "ne": "!=", "lt": "<", "le": "<=", "gt": ">", "ge": ">="}[self.op]
        return f"({self.left!r} {sym} {self.right!r})"


class Arith(Expression[Any]):
    __slots__ = ("op", "left", "right")

    def __init__(self, op: str, left: Node, right: Node) -> None:
        self.op = op
        self.left = left
        self.right = right

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "arith", "op": self.op, "l": self.left._ir(ctx), "r": self.right._ir(ctx)}

    def __repr__(self) -> str:
        sym = {"add": "+", "sub": "-", "mul": "*", "div": "/"}[self.op]
        return f"({self.left!r} {sym} {self.right!r})"


class InSelect(Condition):
    __slots__ = ("item", "select", "neg")

    def __init__(self, item: Expression[Any], select: Any, neg: bool) -> None:
        self.item = item
        self.select = select
        self.neg = neg

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "in_select", "item": self.item._ir(ctx), "select": self.select._subquery_ir(ctx, "in_()"), "neg": self.neg}

    def __repr__(self) -> str:
        return f"{self.item!r} {'NOT IN' if self.neg else 'IN'} ({self.select!r})"


class Exists(Condition):
    __slots__ = ("query",)

    def __init__(self, query: Any) -> None:
        from .query import QuerySet
        from .select import Select

        if not isinstance(query, (QuerySet, Select)):
            raise TypeError(f"exists() takes a query set or a select(), got {query!r}")
        self.query = query

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "exists", "select": self.query._subquery_ir(ctx, "exists()")}

    def __repr__(self) -> str:
        return f"exists({self.query!r})"


class In(Condition):
    __slots__ = ("item", "values", "neg")

    def __init__(self, item: Node, values: list[Node], neg: bool) -> None:
        self.item = item
        self.values = values
        self.neg = neg

    def _ir(self, ctx: IRContext) -> IR:
        return {
            "t": "in",
            "item": self.item._ir(ctx),
            "values": [v._ir(ctx) for v in self.values],
            "neg": self.neg,
        }


class IsNull(Condition):
    __slots__ = ("item", "neg")

    def __init__(self, item: Node, neg: bool) -> None:
        self.item = item
        self.neg = neg

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "is_null", "item": self.item._ir(ctx), "neg": self.neg}


class Like(Condition):
    __slots__ = ("item", "pattern", "ci", "neg")

    def __init__(self, item: Node, pattern: Any, ci: bool, neg: bool = False) -> None:
        self.item = item
        self.pattern = pattern
        self.ci = ci
        self.neg = neg

    def _ir(self, ctx: IRContext) -> IR:
        return {
            "t": "like",
            "item": self.item._ir(ctx),
            "pattern": self.pattern._ir(ctx) if isinstance(self.pattern, Node) else ctx.param(self.pattern),
            "ci": self.ci,
            "neg": self.neg,
        }


class Const(Condition):
    __slots__ = ("value",)

    def __init__(self, value: bool) -> None:
        self.value = value

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "const", "value": self.value}


class BoolOp(Condition):
    __slots__ = ("op", "items")

    def __init__(self, op: str, items: list[Condition]) -> None:
        self.op = op
        self.items = items

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": self.op, "items": [i._ir(ctx) for i in self.items]}

    def __repr__(self) -> str:
        sym = " & " if self.op == "and" else " | "
        return "(" + sym.join(map(repr, self.items)) + ")"


class Not(Condition):
    __slots__ = ("item",)

    def __init__(self, item: Condition) -> None:
        self.item = item

    def _ir(self, ctx: IRContext) -> IR:
        return {"t": "not", "item": self.item._ir(ctx)}

    def __repr__(self) -> str:
        return f"~{self.item!r}"


ConditionLike = Union[Condition, Expression[bool]]


def as_condition(value: ConditionLike) -> Condition:
    if isinstance(value, Condition):
        return value
    if isinstance(value, Expression):
        return Comparison("eq", value, Literal(True))
    raise TypeError(
        f"expected a condition such as `User.email == 'a@b.c'`, got {value!r}"
    )


def _combine(op: str, items: tuple[ConditionLike, ...]) -> Condition:
    flat: list[Condition] = []
    for item in map(as_condition, items):
        if isinstance(item, BoolOp) and item.op == op:
            flat.extend(item.items)
        else:
            flat.append(item)
    if len(flat) == 1:
        return flat[0]
    return BoolOp(op, flat)


def and_(*conditions: ConditionLike) -> Condition:
    """All of ``conditions``. Same as joining them with ``&``."""
    return _combine("and", conditions) if conditions else Const(True)


def or_(*conditions: ConditionLike) -> Condition:
    """Any of ``conditions``. Same as joining them with ``|``."""
    return _combine("or", conditions) if conditions else Const(False)


def not_(condition: ConditionLike) -> Condition:
    return Not(as_condition(condition))
