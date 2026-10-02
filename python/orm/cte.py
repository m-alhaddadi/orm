"""Common table expressions: ``WITH <name> AS (<query>)``.

A CTE is a named query the statement computes first and then reads like a table::

    ranked = Post.objects.select(
        Post, func.row_number().over(partition_by=Post.author_id, order_by=Post.views.desc()).label("rank")
    ).cte("ranked")

    # Instances, read from the CTE instead of the table (a subquery in FROM):
    top3 = await Post.objects.from_(ranked).filter(ranked.c.rank <= 3)
    # Rows of any CTE:
    await ranked.select(ranked.c.author_id, func.max(ranked.c.views).label("best")).group_by(ranked.c.author_id)

A query that reads a CTE declares it: it doesn't need declaring anywhere else, and every
CTE it (or its subqueries) reads ends up in one ``WITH`` clause. ``recursive=`` adds a
part that reads the CTE's own rows (``WITH RECURSIVE``)::

    chain = User.objects.filter(User.id == 1).cte(
        "chain", recursive=lambda chain: User.objects.filter(User.id == chain.c.id + 1)
    )
    await User.objects.from_(chain)
"""

from __future__ import annotations

from collections.abc import Callable, Iterable
from typing import TYPE_CHECKING, Any, Unpack

from .errors import QueryError
from .expr import IR, Expression, IRContext, _Ctes
from .query import QuerySet

if TYPE_CHECKING:
    from .model import Model
    from .select import Select

    AnySelect = Select[Unpack[tuple[Any, ...]]]

__all__ = ["Cte", "CteColumn"]


class CteColumn(Expression[Any]):
    """``cte.c.<name>``: a column of a CTE."""

    __slots__ = ("_cte", "_name")

    def __init__(self, cte: Cte, name: str) -> None:
        self._cte = cte
        self._name = name

    def _ir(self, ctx: IRContext) -> IR:
        ctx.use_cte(self._cte)
        return {"t": "cte_col", "cte": self._cte.name, "name": self._name}

    def __repr__(self) -> str:
        return f"{self._cte.name}.c.{self._name}"


class _Columns:
    __slots__ = ("_cte",)

    def __init__(self, cte: Cte) -> None:
        self._cte = cte

    def __getattr__(self, name: str) -> CteColumn:
        if name.startswith("_"):
            raise AttributeError(name)
        return self[name]

    def __getitem__(self, name: str) -> CteColumn:
        if name not in self._cte.columns:
            raise AttributeError(f"CTE {self._cte.name} has no column {name!r}; it has {', '.join(self._cte.columns)}")
        return CteColumn(self._cte, name)

    def __dir__(self) -> Iterable[str]:
        return list(self._cte.columns)


class Cte:
    """A named query (``WITH``). Build it with ``qs.cte(name)`` or
    ``qs.select(...).cte(name)``; read it with ``Model.objects.from_(cte)`` (when it
    has the model's columns) or ``cte.select(...)``. ``cte.c.<name>`` are its columns.
    """

    __slots__ = ("name", "columns", "c", "_query", "_recursive", "_distinct", "_materialized", "_model")

    def __init__(
        self,
        name: str,
        query: QuerySet[Any] | AnySelect,
        *,
        recursive: Callable[[Cte], QuerySet[Any] | AnySelect] | None = None,
        distinct: bool = False,
        materialized: bool | None = None,
    ) -> None:
        if not name.isidentifier():
            raise ValueError(f"CTE name {name!r} must be an identifier")
        self.name = name
        self._query = query
        # The model whose fields the CTE has (all of them), if any.
        self._model: type[Model] | None
        if isinstance(query, QuerySet):
            if query._prefetch or query._related or query._lock is not None:
                raise QueryError("a CTE's query can't prefetch, select_related or lock")
            self._model = query.model
            self.columns: tuple[str, ...] = query.model._meta.field_names
        else:
            self._model = query._qs.model if any(isinstance(i, type) for i in query._items) else None
            cols: list[str] = []
            for item, n in zip(query._items, query._names):
                cols.extend(query._qs.model._meta.field_names if isinstance(item, type) else (n,))
            dupes = sorted({n for n in cols if cols.count(n) > 1})
            if dupes:
                raise ValueError(f"CTE {name} has several columns named {', '.join(dupes)}; label them")
            self.columns = tuple(cols)
        self.c = _Columns(self)
        self._distinct = distinct
        self._materialized = materialized
        self._recursive = recursive(self) if recursive is not None else None

    def _body_ir(self, params: list[Any], ctes: _Ctes) -> IR:
        ir: IR = {"name": self.name, "query": self._query._cte_ir(params, ctes)}
        if self._recursive is not None:
            ir["recursive"] = self._recursive._cte_ir(params, ctes)
            if self._distinct:
                ir["distinct"] = True
        if self._materialized is not None:
            ir["materialized"] = self._materialized
        return ir

    def select(self, *items: Expression[Any]) -> AnySelect:
        """Rows of this CTE's columns (and expressions over them): ``cte.select(cte.c.a,
        func.count())``. Filter, group, order and slice it like any ``select()``."""
        from .select import Select

        for item in items:
            if not isinstance(item, Expression):
                raise TypeError(f"{self.name}.select() takes expressions over {self.name}.c, got {item!r}")
        return Select(_CteQuerySet(self), items)

    def __repr__(self) -> str:
        return f"<Cte {self.name} ({', '.join(self.columns)})>"


class _CteQuerySet(QuerySet[Any]):
    """The rows of a CTE without a model, for ``cte.select(...)``."""

    __slots__ = ("_cte",)

    def __init__(self, cte: Cte) -> None:
        base = cte._query.model if isinstance(cte._query, QuerySet) else cte._query._qs.model
        super().__init__(base)
        self._cte = cte

    def _context(self, params: list[Any], outer: IRContext | None, ctes: _Ctes | None) -> IRContext:
        ctx = IRContext(self._cte, params, outer, ctes)
        ctx.use_cte(self._cte)
        return ctx

    def _root_name(self) -> str:
        return self._cte.name

    def __repr__(self) -> str:
        return f"<{self._cte.name}>"
