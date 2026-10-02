"""Expression tree for filters, ordering and updates.

``User.email`` evaluates to a :class:`ColumnRef`; comparing it builds a
:class:`Condition`. ``User.posts`` evaluates to a :class:`RelationPath`, whose
attributes are the related model's columns and relations, so
``User.posts.created_at < yesterday`` is a condition on a column reached through a
relation. Nothing here talks to the database: expressions compile to the IR
dictionaries consumed by the native engine (see ``core/src/ir.rs``).
"""

from __future__ import annotations

from collections.abc import Iterable
from typing import TYPE_CHECKING, Any, Generic, TypeVar, Union

if TYPE_CHECKING:
    from .fields import Field
    from .model import Model

T = TypeVar("T")
M = TypeVar("M", bound="Model")

IR = dict[str, Any]

__all__ = [
    "Expression",
    "ColumnRef",
    "RelationPath",
    "Condition",
    "Ordering",
    "Literal",
    "Excluded",
    "excluded",
    "and_",
    "or_",
    "not_",
]


class IRContext:
    """Collects literal parameters while compiling, and checks that every column
    belongs to the query's root model."""

    __slots__ = ("root", "params")

    def __init__(self, root: type[Model], params: list[Any]) -> None:
        self.root = root
        self.params = params

    def param(self, value: Any) -> IR:
        self.params.append(value)
        return {"t": "param", "i": len(self.params) - 1}


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

    def in_(self, values: Iterable[T]) -> Condition:
        values = list(values)
        if not values:
            return Const(False)
        return In(self, [_wrap(v) for v in values], neg=False)

    def not_in(self, values: Iterable[T]) -> Condition:
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
        return Like(self, f"%{_escape_like(text)}%", ci=False)

    def icontains(self: Expression[str] | Expression[str | None], text: str) -> Condition:
        return Like(self, f"%{_escape_like(text)}%", ci=True)

    def startswith(self: Expression[str] | Expression[str | None], text: str) -> Condition:
        return Like(self, f"{_escape_like(text)}%", ci=False)

    def endswith(self: Expression[str] | Expression[str | None], text: str) -> Condition:
        return Like(self, f"%{_escape_like(text)}", ci=False)

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

    # Ordering --------------------------------------------------------------------------------

    def asc(self) -> Ordering:
        return Ordering(self, desc=False)

    def desc(self) -> Ordering:
        return Ordering(self, desc=True)


def _escape_like(text: str) -> str:
    return text.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")


class Literal(Expression[Any]):
    __slots__ = ("value",)

    def __init__(self, value: Any) -> None:
        self.value = value

    def _ir(self, ctx: IRContext) -> IR:
        return ctx.param(self.value)

    def __repr__(self) -> str:
        return repr(self.value)


class ColumnRef(Expression[T]):
    """A column of ``root``'s model, or of a model reached from it through ``path``."""

    __slots__ = ("_root", "_path", "_field")

    def __init__(self, root: type[Model], path: tuple[str, ...], field: Field[Any]) -> None:
        self._root = root
        self._path = path
        self._field = field

    def _ir(self, ctx: IRContext) -> IR:
        if self._root is not ctx.root:
            raise ValueError(
                f"{self!r} belongs to {self._root.__name__}, not to a "
                f"{ctx.root.__name__} query; reach it through a relation of "
                f"{ctx.root.__name__} instead"
            )
        return {"t": "col", "path": list(self._path), "name": self._field.name}

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

    def __init__(self, item: Node, pattern: str, ci: bool, neg: bool = False) -> None:
        self.item = item
        self.pattern = pattern
        self.ci = ci
        self.neg = neg

    def _ir(self, ctx: IRContext) -> IR:
        return {
            "t": "like",
            "item": self.item._ir(ctx),
            "pattern": ctx.param(self.pattern),
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
