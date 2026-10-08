"""Lazy, immutable, awaitable query sets."""

from __future__ import annotations

import asyncio
import json
from collections.abc import AsyncIterator, Generator, Iterable, Mapping
from typing import TYPE_CHECKING, Any, Generic, Literal, TypeVar, Unpack, overload

from ._cache import cached
from .errors import QueryError, TransactionRequired
from .expr import (
    IR,
    ColumnRef,
    Condition,
    ConditionLike,
    Expression,
    IRContext,
    Ordering,
    Param,
    RelationPath,
    SlotParams,
    _Slot,
    _Ctes,
    and_,
    as_condition,
    not_,
)
from . import pagination
from .fields import BelongsTo, HasMany, ManyToMany
from .pagination import Page
from .protection import allowed_writes
from . import debug
from .write import CopyInsert, Delete, InsertMany, InsertOne, Update, UpdateMany, assignments, lookup_values, prepare_rows

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from typing_extensions import Self

    from .cte import Cte
    from .db import Database
    from .model import Model
    from .select import Select

M = TypeVar("M", bound="Model")
T1 = TypeVar("T1")
T2 = TypeVar("T2")
T3 = TypeVar("T3")
T4 = TypeVar("T4")
T5 = TypeVar("T5")
T6 = TypeVar("T6")

__all__ = ["QuerySet", "RelatedSet", "ManyRelatedSet", "Prefetch", "Prepared"]

# Ids per query in in_bulk(), well below Postgres' 65535 parameters.
IN_BULK_CHUNK = 10_000
# get_or_insert: rounds of read, then insert, before a concurrent delete wins.
GET_OR_INSERT_ROUNDS = 3


class Prefetch(Generic[M]):
    """A ``prefetch_related`` entry with its own query::

        User.objects.prefetch_related(
            Prefetch(User.posts, Post.objects.filter(Post.published).order_by(Post.views.desc())[:3]),
        )

    ``queryset`` filters, orders and slices the related rows (a slice applies per parent:
    "the 3 most viewed posts of each user"); its own ``prefetch_related`` /
    ``select_related`` apply to them. The rows fill ``user.posts`` (so
    ``user.posts.cached`` and ``await user.posts`` give only them), or the plain list
    attribute ``to_attr`` (``user.top_posts``) when given.
    """

    __slots__ = ("path", "queryset", "to_attr")

    def __init__(self, path: RelationPath[M], queryset: QuerySet[M] | None = None, *, to_attr: str | None = None) -> None:
        if not isinstance(path, RelationPath):
            raise TypeError(f"Prefetch() takes a relation such as User.posts, got {path!r}")
        if queryset is not None and queryset.model is not path._target:
            raise TypeError(f"Prefetch({path!r}) needs a query set of {path._target.__name__}")
        if to_attr is not None and (not to_attr.isidentifier() or to_attr.startswith("_")):
            raise ValueError(f"to_attr {to_attr!r} must be an identifier not starting with '_'")
        self.path = path
        self.queryset = queryset
        self.to_attr = to_attr

    def __repr__(self) -> str:
        return f"Prefetch({self.path!r}, {self.queryset!r}, to_attr={self.to_attr!r})"


class _Node:
    """One relation in the tree of prefetches."""

    __slots__ = ("relation", "attr", "qs", "children")

    def __init__(self, relation: str, attr: str, qs: QuerySet[Any] | None) -> None:
        self.relation = relation
        self.attr = attr
        self.qs = qs
        self.children: list[tuple[tuple[str, ...], Prefetch[Any]]] = []


def _prefetch_tree(model: type[Model], items: Iterable[tuple[tuple[str, ...], Prefetch[Any]]]) -> dict[str, _Node]:
    """Nodes by attribute: paths sharing a prefix share its node, so
    ``User.posts.comments`` nests under ``User.posts``."""
    nodes: dict[str, _Node] = {}
    for hops, p in items:
        hop = hops[0]
        if hop not in model._meta.relations:
            raise ValueError(f"{model.__name__} has no relation {hop!r}")
        attr = (p.to_attr or hop) if len(hops) == 1 else hop
        node = nodes.get(attr)
        if node is None:
            node = nodes[attr] = _Node(hop, attr, None)
        elif node.relation != hop:
            raise ValueError(f"prefetch_related stores {model.__name__}.{node.relation} and .{hop} both in {attr!r}")
        if len(hops) == 1:
            if p.queryset is not None:
                if node.qs is not None and node.qs is not p.queryset:
                    raise ValueError(f"{p.path!r} is prefetched twice with different query sets; use to_attr")
                node.qs = p.queryset
        else:
            node.children.append((hops[1:], p))
    return nodes


def _prefetch_ir(model: type[Model], items: Iterable[tuple[tuple[str, ...], Prefetch[Any]]], params: list[Any]) -> list[IR]:
    out = []
    for node in _prefetch_tree(model, items).values():
        target = model._meta.relations[node.relation].target
        qs: QuerySet[Any] = node.qs if node.qs is not None else target.objects
        if qs._lock is not None:
            raise QueryError("a prefetch query set can't lock rows")
        children = [(p.path._path, p) for p in qs._prefetch] + node.children
        ir, ctx = qs._query_ir("select", params, prefetch=False)
        if children:
            ir["prefetch"] = _prefetch_ir(target, children, params)
        ctx.finish(ir)
        del ir["op"]
        ir["relation"] = node.relation
        if node.attr != node.relation:
            ir["attr"] = node.attr
        out.append(ir)
    return out


_SUBCLASS_SLOTS: dict[type, tuple[str, ...]] = {}


def _copy_subclass_state(source: QuerySet[Any], target: QuerySet[Any]) -> None:
    """Copy what a QuerySet subclass adds: its own slots and any instance ``__dict__``."""
    cls = type(source)
    names = _SUBCLASS_SLOTS.get(cls)
    if names is None:
        found: list[str] = []
        for klass in cls.__mro__[: cls.__mro__.index(QuerySet)]:
            slots = klass.__dict__.get("__slots__", ())
            found.extend((slots,) if isinstance(slots, str) else slots)
        names = _SUBCLASS_SLOTS[cls] = tuple(n for n in found if n not in ("__dict__", "__weakref__"))
    for name in names:
        try:
            setattr(target, name, getattr(source, name))
        except AttributeError:
            pass
    state = getattr(source, "__dict__", None)
    if state:
        target.__dict__.update(state)


class QuerySet(Generic[M]):
    """A query over one model. Every method returns a new query set; nothing runs until
    the query set is awaited (``await User.objects.filter(...)`` gives a list) or a
    terminal coroutine (``first``, ``get``, ``count``, ``update``, ...) is awaited.

    Awaiting the same query set again gives the same rows without querying again (the
    result cache, see ``_cache.py``); ``await qs.all()`` queries afresh.

    Filters are expressions over the model's columns and relation paths::

        await User.objects.filter(User.posts.created_at < yesterday)

    Conditions inside one ``filter()`` call that go through the same to-many relation
    must hold for the same related row; separate ``filter()`` calls are independent.
    ``exclude(c)`` keeps rows for which ``c`` does not hold, e.g.
    ``exclude(User.posts.published == False)`` keeps users with no unpublished post.
    """

    __slots__ = ("_model_helpers", "_without_defaults", "_without_related", "_model_fields", "_model", "_filters", "_order", "_limit", "_offset", "_related", "_prefetch", "_lock", "_db", "_from", "_joins", "_result")

    def __init__(self, model: type[M]) -> None:
        self._model_helpers: tuple[str, ...] = ()
        self._without_defaults = False
        self._without_related = False
        self._model_fields: tuple[str, ...] | None = None
        self._model = model
        self._filters: tuple[Condition, ...] = ()
        self._order: tuple[Ordering, ...] = ()
        self._limit: int | Param | None = None
        self._offset: int | Param | None = None
        self._related: tuple[tuple[str, ...], ...] = ()
        self._prefetch: tuple[Prefetch[Any], ...] = ()
        self._lock: dict[str, bool] | None = None
        self._db: Database | None = None
        self._from: Cte | None = None
        self._joins: tuple[tuple[Cte, Condition, bool], ...] = ()
        self._result: asyncio.Task[list[M]] | None = None

    @property
    def model(self) -> type[M]:
        return self._model

    def _clone(self, **changes: Any) -> Self:
        # Every builder step clones; explicit slot copies keep that step cheap.
        cls = type(self)
        new = cls.__new__(cls)
        new._model_helpers = self._model_helpers
        new._without_defaults = self._without_defaults
        new._without_related = self._without_related
        new._model_fields = self._model_fields
        new._model = self._model
        new._filters = self._filters
        new._order = self._order
        new._limit = self._limit
        new._offset = self._offset
        new._related = self._related
        new._prefetch = self._prefetch
        new._lock = self._lock
        new._db = self._db
        new._from = self._from
        new._joins = self._joins
        new._result = None
        if cls is not QuerySet:
            _copy_subclass_state(self, new)
        for k, v in changes.items():
            setattr(new, k, v)
        return new

    # -- building ------------------------------------------------------------------------

    def all(self) -> Self:
        return self._clone()

    def without_defaults(self) -> Self:
        """Bypass schema filter, selection and loading defaults; retain caller filters."""
        return self._clone(_without_defaults=True)

    def without_related(self) -> Self:
        """Clear default and explicitly requested eager reference loading."""
        return self._clone(_without_related=True, _related=())

    def only(self, *fields: ColumnRef[Any]) -> Self:
        """Return partial model instances. No arguments restores all public fields."""
        names = []
        for field in fields:
            if not isinstance(field, ColumnRef) or field._root is not self._model or field._path:
                raise TypeError("only() takes root model columns")
            names.append(field._field.name)
        if len(set(names)) != len(names):
            raise TypeError("duplicate model field")
        return self._clone(_model_fields=tuple(names) if fields else self._model._meta.field_names)

    def filter(self, *conditions: ConditionLike) -> Self:
        """Keep rows matching all ``conditions``."""
        if not conditions:
            return self._clone()
        return self._clone(_filters=(*self._filters, and_(*conditions)))

    def exclude(self, *conditions: ConditionLike) -> Self:
        """Drop rows matching all ``conditions``."""
        if not conditions:
            return self._clone()
        return self._clone(_filters=(*self._filters, not_(and_(*conditions))))

    def order_by(self, *items: Expression[Any] | Ordering) -> Self:
        """Replace the ordering. ``order_by(Post.created_at.desc(), Post.id)``."""
        order = tuple(i if isinstance(i, Ordering) else Ordering(i, desc=False) for i in items)
        return self._clone(_order=order)

    def limit(self, n: int | None) -> Self:
        """At most ``n`` rows; ``n`` may be a :func:`orm.param` in a prepared query."""
        return self._clone(_limit=n)

    def offset(self, n: int | None) -> Self:
        """Skip ``n`` rows; ``n`` may be a :func:`orm.param` in a prepared query."""
        return self._clone(_offset=n if isinstance(n, Param) else n or None)

    def __getitem__(self, s: slice) -> Self:
        """``qs[10:20]`` is ``qs.offset(10).limit(10)``."""
        if isinstance(self._limit, Param) or isinstance(self._offset, Param):
            raise QueryError("a query set limited by param() can't be sliced; use limit() and offset()")
        if not isinstance(s, slice):
            raise TypeError(
                "query sets can only be sliced; use `await qs.offset(i).first()` for one row"
            )
        if s.step is not None or (s.start or 0) < 0 or (s.stop is not None and s.stop < 0):
            raise ValueError("only non-negative slices without a step are supported")
        start = s.start or 0
        offset = (self._offset or 0) + start
        limit = self._limit
        if s.stop is not None:
            n = max(s.stop - start, 0)
            limit = n if limit is None else max(min(n, limit - start), 0)
        elif limit is not None:
            limit = max(limit - start, 0)
        return self._clone(_offset=offset or None, _limit=limit)

    def select_related(self, *paths: RelationPath[Any]) -> Self:
        """Load to-one relations in the same query with LEFT JOINs.

        ``Comment.objects.select_related(Comment.post.author)`` fills
        ``comment.post`` and ``comment.post.author``.
        """
        related = list(self._related)
        for p in paths:
            self._check_path(p)
            for i in range(1, len(p._path) + 1):
                if p._path[:i] not in related:
                    related.append(p._path[:i])
        return self._clone(_related=tuple(related))

    def prefetch_related(self, *relations: RelationPath[Any] | Prefetch[Any]) -> Self:
        """Load relations with one extra ``IN (...)`` query each, in the same call::

            User.objects.prefetch_related(User.posts)              # user.posts.cached
            User.objects.prefetch_related(User.posts.comments)     # and each post's comments
            Comment.objects.prefetch_related(Comment.post)         # to-one: comment.post
            User.objects.prefetch_related(Prefetch(User.posts, Post.objects.filter(...)))

        A path loads every relation along it. :class:`Prefetch` gives the related rows
        a query of their own (filters, order, a slice per parent, nested loading).
        """
        prefetch = list(self._prefetch)
        for p in relations:
            item = p if isinstance(p, Prefetch) else Prefetch(p)
            self._check_path(item.path)
            prefetch.append(item)
        _prefetch_tree(self._model, ((p.path._path, p) for p in prefetch))  # validates
        return self._clone(_prefetch=tuple(prefetch))

    def lock(self, exclusive: bool = True, *, nowait: bool = False, skip_locked: bool = False) -> Self:
        """Lock the rows this query reads until the transaction ends.

        ``exclusive=True`` is ``FOR UPDATE`` (others can't lock, update or delete the
        rows), ``False`` is ``FOR SHARE`` (others can read-lock them too, but not change
        them). Only this model's rows are locked, not rows joined by ``select_related``.
        Rows locked by another transaction are waited for, unless ``nowait`` (raise
        :class:`~orm.LockNotAvailable`) or ``skip_locked`` (leave them out, e.g. for job
        queues). Must run inside ``db.transaction()``.
        """
        if nowait and skip_locked:
            raise ValueError("lock() takes nowait or skip_locked, not both")
        return self._clone(_lock={"exclusive": exclusive, "nowait": nowait, "skip_locked": skip_locked})

    def using(self, db: Database | Literal["primary"] | None) -> Self:
        """Run on ``db``; ``"primary"`` sends reads of this query set to the primary of
        its database (the default one, resolved now), not to a replica."""
        if isinstance(db, str):
            if db != "primary":
                raise ValueError(f'using() takes a Database or "primary", got {db!r}')
            from .db import resolve

            db = resolve(self._db).primary
        return self._clone(_db=db)

    # -- CTEs -------------------------------------------------------------------------------

    def cte(
        self,
        name: str,
        *,
        recursive: Callable[[Cte], QuerySet[Any]] | None = None,
        distinct: bool = False,
        materialized: bool | None = None,
    ) -> Cte:
        """This query as a CTE (``WITH <name> AS (...)``) with the model's columns: read
        it with ``Model.objects.from_(cte)``. ``recursive`` builds the recursive part
        from the CTE (``WITH RECURSIVE``; ``UNION ALL``, or ``UNION`` with ``distinct``);
        ``materialized`` forces ``[NOT] MATERIALIZED``. See :mod:`orm.cte`."""
        from .cte import Cte

        return Cte(name, self, recursive=recursive, distinct=distinct, materialized=materialized)

    def join(self, cte: Cte, on: ConditionLike, *, outer: bool = False) -> Self:
        """``JOIN <cte> ON <on>`` (``LEFT JOIN`` with ``outer``): the CTE's columns
        (``cte.c.<name>``) come along with each row, for filters, ordering and
        ``select()``::

            totals = Post.objects.select(Post.author_id, func.sum(Post.views).label("views")) \\
                .group_by(Post.author_id).cte("totals")
            await User.objects.join(totals, totals.c.author_id == User.id).select(User, totals.c.views)

        A row matching several CTE rows comes back once per match, as in SQL.
        """
        from .cte import Cte

        if not isinstance(cte, Cte):
            raise TypeError(f"join() takes a CTE, got {cte!r}")
        if any(c is cte for c, _, _ in self._joins) or cte is self._from:
            raise ValueError(f"{cte.name} is already read by this query")
        return self._clone(_joins=(*self._joins, (cte, as_condition(on), outer)))

    def from_(self, cte: Cte) -> Self:
        """Read the rows from ``cte`` instead of the model's table: a subquery in
        ``FROM``. The CTE must have the model's columns (``Post.objects....cte(...)`` or
        a ``select(Post, ...)``); its other columns are ``cte.c.<name>``::

            await Post.objects.from_(ranked).filter(ranked.c.rank <= 3)
        """
        from .cte import Cte

        if not isinstance(cte, Cte):
            raise TypeError(f"from_() takes a CTE, got {cte!r}")
        if cte._model is not self._model:
            raise TypeError(f"from_({cte.name}) needs a CTE with the columns of {self._model.__name__}")
        return self._clone(_from=cte)

    # -- select(...) ----------------------------------------------------------------------
    # Each column is an expression (its value type) or the model itself (an instance).

    @overload
    def select(self, a: Expression[T1] | type[T1], /) -> Select[T1]: ...
    @overload
    def select(self, a: Expression[T1] | type[T1], b: Expression[T2] | type[T2], /) -> Select[T1, T2]: ...
    @overload
    def select(
        self, a: Expression[T1] | type[T1], b: Expression[T2] | type[T2], c: Expression[T3] | type[T3], /
    ) -> Select[T1, T2, T3]: ...
    @overload
    def select(
        self,
        a: Expression[T1] | type[T1],
        b: Expression[T2] | type[T2],
        c: Expression[T3] | type[T3],
        d: Expression[T4] | type[T4],
        /,
    ) -> Select[T1, T2, T3, T4]: ...
    @overload
    def select(
        self,
        a: Expression[T1] | type[T1],
        b: Expression[T2] | type[T2],
        c: Expression[T3] | type[T3],
        d: Expression[T4] | type[T4],
        e: Expression[T5] | type[T5],
        /,
    ) -> Select[T1, T2, T3, T4, T5]: ...
    @overload
    def select(
        self,
        a: Expression[T1] | type[T1],
        b: Expression[T2] | type[T2],
        c: Expression[T3] | type[T3],
        d: Expression[T4] | type[T4],
        e: Expression[T5] | type[T5],
        f: Expression[T6] | type[T6],
        /,
    ) -> Select[T1, T2, T3, T4, T5, T6]: ...
    @overload
    def select(self, *items: Expression[Any] | type[Model]) -> Select[Unpack[tuple[Any, ...]]]: ...
    def select(self, *items: Any) -> Any:
        """Rows of the given columns and aggregates instead of instances: see
        :mod:`orm.select`. ``await qs.select(Post.id, Post.title)`` gives ``Row``s,
        ``.scalars()`` / ``.scalar()`` one column's values."""
        from .select import Select

        return Select(self, items)

    # -- batches --------------------------------------------------------------------------

    async def batches(self, size: int = 1000) -> AsyncIterator[list[M]]:
        """The rows in lists of ``size``, walking the primary key (``WHERE pk > last
        ORDER BY pk LIMIT size``), so memory stays flat and each batch is an index
        range scan. ``select_related``, ``prefetch_related`` and ``lock()`` apply per
        batch; a custom ``order_by`` or slicing is rejected.

        ::

            async for batch in Post.objects.filter(Post.published).batches(500):
                await index(batch)
        """
        if size < 1:
            raise ValueError("batch size must be at least 1")
        if self._order:
            raise QueryError("batches() walk the primary key in order; drop order_by()")
        if self._limit is not None or self._offset is not None:
            raise QueryError("batches() can't be used on a sliced query set")
        pk = self._model._meta.pk_ref()
        last: Any = None
        seen: set[str] = set()
        while True:
            page = self if last is None else self.filter(pk > last)
            # No `yield` inside: the loop scope must not reach the consumer's queries.
            with debug.internal_loop(seen):
                objs = await page.order_by(pk)[:size]._fetch()
            if objs:
                yield objs
            if len(objs) < size:
                return
            last = objs[-1].pk

    async def iterate(self, batch_size: int = 1000) -> AsyncIterator[M]:
        """Every row, fetched ``batch_size`` at a time (see :meth:`batches`)::

            async for post in Post.objects.iterate():
                ...
        """
        async for batch in self.batches(batch_size):
            for obj in batch:
                yield obj

    def _check_path(self, p: RelationPath[Any]) -> None:
        if not isinstance(p, RelationPath):
            raise TypeError(f"expected a relation such as {self._model.__name__}.<relation>, got {p!r}")
        if p._root is not self._model:
            raise ValueError(f"{p!r} does not start at {self._model.__name__}")

    # -- IR ------------------------------------------------------------------------------

    def _context(self, params: list[Any], outer: IRContext | None, ctes: _Ctes | None) -> IRContext:
        ctx = IRContext(self._model, params, outer, ctes)
        if self._from is not None:
            ctx.use_cte(self._from)
        return ctx

    def _root_name(self) -> str:
        name: str = self._model._meta.name
        return name

    def _query_ir(
        self,
        op: str,
        params: list[Any],
        outer: IRContext | None = None,
        ctes: _Ctes | None = None,
        *,
        prefetch: bool = True,
    ) -> tuple[dict[str, Any], IRContext]:
        """The query's IR and the context it compiled in (``finish()`` it to declare its
        CTEs). In a subquery (``outer``) or a CTE (``ctes``), related loading is left out."""
        ctx = self._context(params, outer, ctes)
        ir: dict[str, Any] = {"op": op, "model": self._root_name()}
        if self._without_defaults:
            ir["without_defaults"] = True
        if self._without_related:
            ir["without_related"] = True
        if self._model_fields is not None:
            ir["model_fields"] = list(self._model_fields)
        if self._model_helpers:
            ir["model_helpers"] = list(self._model_helpers)
        if self._from is not None:
            ir["from"] = self._from.name
        if self._joins:
            joins = []
            for cte, on, is_outer in self._joins:
                ctx.use_cte(cte)
                joins.append({"cte": cte.name, "on": on._ir(ctx), "outer": is_outer})
            ir["joins"] = joins
        if self._filters:
            ir["filters"] = [f._ir(ctx) for f in self._filters]
        if self._order:
            ir["order"] = [o._ir(ctx) for o in self._order]
        if self._limit is not None:
            ir["limit"] = self._limit._ir(ctx) if isinstance(self._limit, Param) else ctx.param(self._limit)
        if self._offset is not None:
            ir["offset"] = self._offset._ir(ctx) if isinstance(self._offset, Param) else ctx.param(self._offset)
        top = outer is None and ctes is None
        if self._related and op == "select" and top:
            ir["select_related"] = [list(p) for p in self._related]
        if self._prefetch and op == "select" and top and prefetch:
            ir["prefetch"] = _prefetch_ir(self._model, ((p.path._path, p) for p in self._prefetch), params)
        if self._lock is not None:
            if op != "select":
                raise QueryError(f"{op}() can't lock rows; lock() applies to reading rows")
            ir["lock"] = self._lock
        ctx.add_windows(ir)
        return ir, ctx

    def _select_ir(self, op: str, params: list[Any]) -> dict[str, Any]:
        ir, ctx = self._query_ir(op, params)
        return ctx.finish(ir)

    def _subquery_ir(self, ctx: IRContext, what: str) -> dict[str, Any]:
        if self._lock is not None:
            raise QueryError("a subquery can't lock rows")
        return self._query_ir("select", ctx.params, ctx)[0]

    def _cte_ir(self, params: list[Any], ctes: _Ctes) -> dict[str, Any]:
        return self._query_ir("select", params, ctes=ctes)[0]

    def _mutation_ir(self, op: str, params: list[Any], values: Mapping[str, Any] | None = None) -> dict[str, Any]:
        if self._limit is not None or self._offset is not None:
            raise QueryError(f"{op}() is not supported on a sliced query set")
        if self._lock is not None:
            raise QueryError(f"{op}() locks the rows it changes; drop lock()")
        if self._from is not None or self._joins:
            raise QueryError(f"{op}() writes the model's table; it can't run on a query set with from_() or join()")
        ctx = IRContext(self._model, params)
        ir: dict[str, Any] = {"op": op, "model": self._model._meta.name, "filters": [f._ir(ctx) for f in self._filters]}
        if self._without_defaults:
            ir["without_defaults"] = True
        if self._model_fields is not None:
            ir["model_fields"] = list(self._model_fields)
        if values is not None:
            ir["set"] = assignments(self._model, values, ctx)
        return ctx.finish(ir)

    def _native(self) -> Any:
        return self._model._meta.registry.native()

    def prepare(self) -> Prepared[M]:
        """This query compiled once, to run many times with :func:`orm.param` values::

            by_author = Post.objects.filter(Post.author_id == orm.param("author")).prepare()
            posts = await by_author(author=3)

        Each call skips building the query (the expression tree, the IR and its JSON) and
        only binds the values. Build it once, e.g. at module level, and reuse it."""
        return Prepared(self)

    def sql(self) -> str:
        """The SELECT this query set runs, with parameters inlined (for debugging)."""
        params: list[Any] = []
        ir = self._select_ir("select", params)
        sql: str = self._native().sql(json.dumps(ir), params)
        return sql

    # -- execution -----------------------------------------------------------------------

    async def _run(self, ir: dict[str, Any], params: list[Any], row_cls: type | None = None) -> Any:
        from .db import resolve

        return await resolve(self._db)._run(ir, params, row_cls, self._db)

    def _default_order(self) -> tuple[Ordering, ...]:
        """The order of this read: ``order_by()``, else the schema default order, else the pk."""
        if self._order:
            return self._order
        meta = self._model._meta
        if meta.default_order and not self._without_defaults and self._from is None:
            return tuple(
                Ordering(ColumnRef(self._model, (), meta.fields[k["field"]]), k.get("desc", False), k.get("nulls"))
                for k in meta.default_order
            )
        return (Ordering(meta.pk_ref(), desc=False),)

    async def _fetch(self) -> list[M]:
        params: list[Any] = []
        ir = self._select_ir("select", params)
        self._check_lock()
        objs: list[M] = await self._run(ir, params)
        return objs

    def _check_lock(self) -> None:
        if self._lock is not None:
            from .db import resolve

            if resolve(self._db)._tx() is None:
                raise TransactionRequired(
                    "lock() outside a transaction would release the locks as soon as the "
                    "query ends; run it inside `async with db.transaction():`"
                )

    def __await__(self) -> Generator[Any, None, list[M]]:
        return cached(self, self._fetch, enabled=self is not self._model.__dict__.get("objects")).__await__()

    async def __aiter__(self) -> AsyncIterator[M]:
        for obj in await self:
            yield obj

    async def paginate(
        self, *, first: int | None = None, after: str | None = None, last: int | None = None, before: str | None = None
    ) -> Page[M]:
        """A page of rows by keyset: ``paginate(first=20, after=cursor)`` reads on from a
        cursor, ``paginate(last=20, before=cursor)`` reads back.

        The order is ``order_by()``, else the schema default order, else the primary key;
        the primary key is added when the order is not unique. Order columns are columns
        of the model itself, and a nullable one needs ``nulls=``. ``select_related``,
        ``prefetch_related``, ``only()`` and query defaults apply to each page.
        """
        if (first is None) == (last is None):
            raise TypeError("paginate() takes first= or last=")
        if (first is not None and before is not None) or (last is not None and after is not None):
            raise TypeError("paginate() takes first= with after=, or last= with before=")
        size = first if first is not None else last
        if not isinstance(size, int) or isinstance(size, bool) or size < 1:
            raise ValueError("a page size is an integer of at least 1")
        if self._limit is not None or self._offset is not None:
            raise QueryError("paginate() can't be used on a sliced query set")
        keys = pagination.keyset(self._model, self._default_order())
        fp = pagination.fingerprint(self._model, keys)
        forward = first is not None
        order = [o if forward else o.reversed() for _, o in keys]
        helpers = (*self._model_helpers, *(f.name for f, _ in keys if f.name not in self._model_helpers))
        qs = self._clone(_model_helpers=helpers)
        cursor = after if forward else before
        if cursor is not None:
            qs = qs.filter(pagination.after(self._model, order, pagination.decode_cursor(self._model, cursor, fp, keys)))
        rows = await qs.order_by(*order)[: size + 1]._fetch()
        more = len(rows) > size
        rows = rows[:size] if forward else rows[:size][::-1]
        return Page(
            rows,
            has_next=more if forward else cursor is not None,
            has_previous=cursor is not None if forward else more,
            next_cursor=pagination.encode_cursor(fp, keys, rows[-1]) if rows else None,
            previous_cursor=pagination.encode_cursor(fp, keys, rows[0]) if rows else None,
        )

    async def first(self) -> M | None:
        """First row by the current ordering (primary key if none), or None."""
        objs = await self._clone(_order=self._default_order())[:1]._fetch()
        return objs[0] if objs else None

    async def last(self) -> M | None:
        """Last row by the current ordering (primary key if none), or None."""
        order = tuple(o.reversed() for o in self._default_order())
        objs = await self._clone(_order=order)[:1]._fetch()
        return objs[0] if objs else None

    async def get(self, *conditions: ConditionLike) -> M:
        """The single row matching ``conditions``; raises ``Model.DoesNotExist`` or
        ``Model.MultipleObjectsReturned`` otherwise."""
        objs = await self.filter(*conditions).limit(2)._fetch()
        if not objs:
            raise self._model.DoesNotExist(f"no {self._model.__name__} matches the query")
        if len(objs) > 1:
            raise self._model.MultipleObjectsReturned(
                f"more than one {self._model.__name__} matches the query"
            )
        return objs[0]

    async def count(self) -> int:
        params: list[Any] = []
        n: int = await self._run(self._select_ir("count", params), params)
        return n

    async def exists(self) -> bool:
        """Whether any row matches. For ``EXISTS`` inside another query use
        :func:`orm.exists`."""
        params: list[Any] = []
        b: bool = await self._run(self._select_ir("exists", params), params)
        return b

    async def in_bulk(self, ids: Iterable[Any] | None = None, *, field: ColumnRef[Any] | None = None) -> dict[Any, M]:
        """The rows whose ``field`` (the primary key by default; otherwise a unique
        field) is one of ``ids``, by that value: ``{1: <Post 1>, 3: <Post 3>}``. Missing
        ids are left out. Without ``ids``, every row. Large id lists run as several
        queries of 10 000 ids."""
        meta = self._model._meta
        col = meta.pk_ref() if field is None else field
        if not isinstance(col, ColumnRef) or col._root is not self._model or col._path:
            raise TypeError(f"in_bulk(field=...) takes a column of {self._model.__name__}, got {col!r}")
        if not (col._field.primary_key or col._field.unique):
            raise ValueError(f"in_bulk(field={col!r}) needs a unique field")
        if self._limit is not None or self._offset is not None:
            raise QueryError("in_bulk() can't be used on a sliced query set")
        name = col._field.name
        if ids is None:
            return {o._field_value(name): o for o in await self._clone(_model_helpers=(*self._model_helpers, name))._fetch()}
        keys = list(dict.fromkeys(ids))
        out: dict[Any, M] = {}
        seen: set[str] = set()
        for i in range(0, len(keys), IN_BULK_CHUNK):
            with debug.internal_loop(seen):
                chunk = await self.filter(col.in_(keys[i : i + IN_BULK_CHUNK]))._clone(_model_helpers=(*self._model_helpers, name))._fetch()
            for o in chunk:
                out[o._field_value(name)] = o
        return out

    def update(self, **values: Any) -> Update[M]:
        """``UPDATE`` every matching row; values may be expressions, e.g.
        ``views=Post.views + 1``. ``await`` gives the number of rows updated;
        ``await qs.update(...).returning()`` gives the updated rows instead."""
        return Update.build(self, values)

    def update_many(self, rows: Iterable[Mapping[str, Any]], *, batch_size: int | None = None) -> UpdateMany[M]:
        """``UPDATE`` each row to its own values: ``rows`` are dicts with the primary key
        and the fields to set (the same fields in every row)::

            await Post.objects.update_many([{"id": 1, "title": "a"}, {"id": 2, "title": "b"}])

        Rows outside this query set's filters are left alone. One statement per
        ``batch_size`` rows (by default as many as fit), all in one transaction.
        ``await`` gives the number of rows updated; ``.returning()`` gives the rows."""
        return UpdateMany(self, rows, batch_size)

    def delete(self) -> Delete[M]:
        """``DELETE`` every matching row. ``await`` gives the number of rows deleted;
        ``await qs.delete().returning()`` gives the deleted rows instead."""
        return Delete.build(self)

    def insert(self, **values: Any) -> InsertOne[M]:
        """``INSERT`` one row; ``await`` gives the new instance with database defaults
        (id, timestamps) filled in. Chain ``.on_conflict(...)`` for an upsert."""
        fields, rows, provided = prepare_rows(self._model, [values])
        return InsertOne(self, fields, rows, provided)

    async def get_or_insert(self, defaults: Mapping[str, Any] | None = None, **lookup: Any) -> tuple[M, bool]:
        """The row matching ``lookup``, or a new row of ``lookup`` and ``defaults``:
        ``(row, created)``::

            user, created = await User.objects.get_or_insert(email="a@b.c", defaults={"name": "A"})

        ``lookup`` names the fields of one unique constraint (the database checks this).
        Safe under concurrency: the insert is ``ON CONFLICT (lookup) DO NOTHING``, and a
        row that a concurrent insert wins is read back."""
        key = lookup_values(self._model, lookup)
        fields = self._model._meta.fields
        cols: list[ColumnRef[Any]] = [ColumnRef(self._model, (), fields[name]) for name in key]
        values = {**(defaults or {}), **lookup}
        for _ in range(GET_OR_INSERT_ROUNDS):
            try:
                return await self.get(*(c == key[c._field.name] for c in cols)), False
            except self._model.DoesNotExist:
                pass
            row = await self.insert(**values).on_conflict(*cols, update=False).returning()
            if row is not None:
                return row, True
        raise QueryError(
            f"get_or_insert: a {self._model.__name__} row matching the lookup was deleted "
            f"concurrently {GET_OR_INSERT_ROUNDS} times"
        )

    async def attach(self, parent_id: Any, values: Mapping[str, Any]) -> M:
        """Attach local child values to an existing parent without altering it."""
        from .composition import attach

        return await attach(self, parent_id, values)

    @overload
    def insert_many(
        self, rows: Iterable[Mapping[str, Any]], *, batch_size: int | None = None, copy: Literal[False] = False
    ) -> InsertMany[M]: ...
    @overload
    def insert_many(self, rows: Iterable[Mapping[str, Any]], *, copy: Literal[True]) -> CopyInsert[M]: ...
    def insert_many(
        self, rows: Iterable[Mapping[str, Any]], *, batch_size: int | None = None, copy: bool = False
    ) -> InsertMany[M] | CopyInsert[M]:
        """``INSERT`` many rows; ``await`` gives the number of rows inserted, and
        ``await qs.insert_many(rows).returning()`` the new instances. One statement per
        ``batch_size`` rows (by default as many as fit in the parameter limit), all in
        one transaction.

        ``copy=True`` loads the rows with Postgres ``COPY`` instead, for large imports:
        ``await`` gives the row count, and ``on_conflict()`` is not available."""
        fields, aligned, provided = prepare_rows(self._model, rows)
        if copy:
            if batch_size is not None:
                raise TypeError("insert_many(copy=True) takes no batch_size: COPY is one statement")
            return CopyInsert(self, fields, aligned)
        return InsertMany(self, fields, aligned, provided, batch_size=batch_size)

    def __repr__(self) -> str:
        parts = [f"filter{f!r}" for f in self._filters]
        if self._order:
            parts.append(f"order_by{self._order!r}")
        if isinstance(self._limit, Param) or isinstance(self._offset, Param):
            parts += [f"{k}({v!r})" for k, v in (("offset", self._offset), ("limit", self._limit)) if v is not None]
        elif self._limit is not None or self._offset is not None:
            parts.append(f"[{self._offset or 0}:{'' if self._limit is None else (self._offset or 0) + self._limit}]")
        return f"<{type(self).__name__} {self._model.__name__}{' ' if parts else ''}{'.'.join(parts)}>"


class _Compiled:
    """One statement of a prepared query: its IR as JSON and its parameters, with the
    :func:`orm.param` slots to fill per call."""

    __slots__ = ("json", "params", "slots", "names")

    def __init__(self, ir: IR, params: SlotParams) -> None:
        self.json = json.dumps(ir)
        self.params = list(params)
        self.slots = [(i, p.name, p.transform) for i, p in enumerate(params) if isinstance(p, _Slot)]
        self.names = frozenset(name for _, name, _ in self.slots)

    def bind(self, values: Mapping[str, Any], known: frozenset[str]) -> list[Any]:
        if missing := self.names - values.keys():
            raise TypeError(f"missing values for {', '.join(sorted(missing))}")
        if unknown := values.keys() - known:
            raise TypeError(f"the prepared query has no param {', '.join(sorted(unknown))}")
        params = self.params.copy()
        for i, name, transform in self.slots:
            v = values[name]
            if v is None:
                raise ValueError(f"param({name!r}) can't be None; compare with None (IS NULL) in the query itself")
            params[i] = v if transform is None else transform(v)
        return params


class Prepared(Generic[M]):
    """A query set compiled by :meth:`QuerySet.prepare`. Await a call to get the rows
    (``await q(author=3)``), or use :meth:`get`, :meth:`first`, :meth:`count` and
    :meth:`exists`; each takes the :func:`orm.param` values as keywords."""

    __slots__ = ("_qs", "_compiled", "_names")

    def __init__(self, qs: QuerySet[M]) -> None:
        self._qs = qs
        self._compiled: dict[str, _Compiled] = {}
        self._names = self._statement("select").names

    def _statement(self, kind: str) -> _Compiled:
        c = self._compiled.get(kind)
        if c is None:
            qs = self._qs
            op = "select"
            if kind == "get":
                qs = qs.limit(2)
            elif kind == "first":
                qs = qs._clone(_order=qs._default_order())
                qs = qs.limit(1) if isinstance(qs._limit, Param) else qs[:1]
            elif kind in ("count", "exists"):
                op = kind
            params = SlotParams()
            c = self._compiled[kind] = _Compiled(qs._select_ir(op, params), params)
        return c

    def _start(self, kind: str, values: Mapping[str, Any]) -> Awaitable[Any]:
        """Starts the statement; the engine's awaitable comes back as is, without a
        coroutine around it."""
        from .db import _with_scope, resolve

        c = self._statement(kind)
        params = c.bind(values, self._names)
        qs = self._qs
        if qs._lock is not None:
            qs._check_lock()
        db = resolve(qs._db)
        if debug._scope.get() is not None:
            debug.record("run:" + c.json, lambda: str(qs._native().statement(c.json, params)))
        op, params = _with_scope(c.json, params)
        return db._reader().run(op, params, db._tx(), None, qs._db, allowed_writes())

    def __call__(self, **values: Any) -> Awaitable[list[M]]:
        """The rows, like awaiting the query set."""
        return self._start("select", values)

    async def get(self, **values: Any) -> M:
        objs: list[M] = await self._start("get", values)
        model = self._qs.model
        if not objs:
            raise model.DoesNotExist(f"no {model.__name__} matches the query")
        if len(objs) > 1:
            raise model.MultipleObjectsReturned(f"more than one {model.__name__} matches the query")
        return objs[0]

    async def first(self, **values: Any) -> M | None:
        objs: list[M] = await self._start("first", values)
        return objs[0] if objs else None

    async def count(self, **values: Any) -> int:
        n: int = await self._start("count", values)
        return n

    async def exists(self, **values: Any) -> bool:
        b: bool = await self._start("exists", values)
        return b

    def sql(self, **values: Any) -> str:
        """The SELECT for these values, parameters inlined (for debugging)."""
        c = self._statement("select")
        sql: str = self._qs._native().sql(c.json, c.bind(values, self._names))
        return sql

    @property
    def params(self) -> frozenset[str]:
        """The names of the values each call takes."""
        return self._names

    def __repr__(self) -> str:
        return f"<Prepared {self._qs!r}>"


class RelatedSet(QuerySet[M]):
    """``user.posts``: the rows of a to-many relation of one instance.

    Awaiting it returns the rows loaded by ``prefetch_related`` when there are any,
    otherwise it runs the query. ``.cached`` gives the prefetched rows without awaiting.
    """

    __slots__ = ("_relation", "_instance")

    def __init__(self, relation: HasMany[M, Any], instance: Model) -> None:
        super().__init__(relation.target)
        self._relation = relation
        self._instance = instance
        key = instance._field_value(relation.from_)
        via: ColumnRef[Any] = ColumnRef(relation.target, (), relation.target._meta.fields[relation.via])
        self._filters = (as_condition(via == key),)

    @property
    def cached(self) -> list[M]:
        """Rows loaded with ``prefetch_related``; raises ``NotLoaded`` otherwise."""
        from .errors import NotLoaded

        rows = self._instance.__dict__.get(self._relation.name)
        if rows is None:
            raise NotLoaded(
                f"{self._relation.model.__name__}.{self._relation.name} was not prefetched"
            )
        return list(rows)

    def _is_pristine(self) -> bool:
        return len(self._filters) == 1 and not self._order and self._limit is None and self._offset is None

    async def _fetch(self) -> list[M]:
        rows = self._instance.__dict__.get(self._relation.name)
        if rows is not None and self._is_pristine() and not self._prefetch and not self._related:
            return list(rows)
        if debug._scope.get() is not None and self._is_pristine():
            with debug.relation_load(self._relation.model.__name__, self._relation.name, "prefetch_related"):
                return await super()._fetch()
        return await super()._fetch()

    def insert(self, **values: Any) -> InsertOne[M]:
        """Insert a related row pointing at this instance."""
        return super().insert(**values, **self._link())

    @overload
    def insert_many(
        self, rows: Iterable[Mapping[str, Any]], *, batch_size: int | None = None, copy: Literal[False] = False
    ) -> InsertMany[M]: ...
    @overload
    def insert_many(self, rows: Iterable[Mapping[str, Any]], *, copy: Literal[True]) -> CopyInsert[M]: ...
    def insert_many(
        self, rows: Iterable[Mapping[str, Any]], *, batch_size: int | None = None, copy: bool = False
    ) -> InsertMany[M] | CopyInsert[M]:
        """Insert related rows pointing at this instance."""
        link = self._link()
        linked = ({**r, **link} for r in rows)
        if copy:
            if batch_size is not None:
                raise TypeError("insert_many(copy=True) takes no batch_size: COPY is one statement")
            return super().insert_many(linked, copy=True)
        return super().insert_many(linked, batch_size=batch_size)

    def _link(self) -> dict[str, Any]:
        return {self._relation.via: self._instance._field_value(self._relation.from_)}


class ManyRelatedSet(QuerySet[M]):
    """``post.tags``: the rows a many-to-many relation links to one instance.

    A query set over the related model (filtered through the join model), which also
    reads rows loaded by ``prefetch_related`` (``.cached``, or awaiting it unchanged),
    and changes the links: ``add()``, ``remove()``, ``set()``, ``clear()`` and
    ``insert()`` write rows of the join model.
    """

    __slots__ = ("_relation", "_instance")

    def __init__(self, relation: ManyToMany[M, Any], instance: Model) -> None:
        from .expr import exists, outer

        super().__init__(relation.target)
        self._relation = relation
        self._instance = instance
        join = relation.through
        fields = join._meta.fields
        source: ColumnRef[Any] = ColumnRef(join, (), fields[relation.source])
        target: ColumnRef[Any] = ColumnRef(join, (), fields[relation.target_field])
        to: ColumnRef[Any] = ColumnRef(relation.target, (), relation.target._meta.fields[relation.to])
        self._filters = (exists(join.objects.filter(source == self._key, target == outer(to))),)

    @property
    def _key(self) -> Any:
        return self._instance._field_value(self._relation.from_)

    @property
    def cached(self) -> list[M]:
        """Rows loaded with ``prefetch_related``; raises ``NotLoaded`` otherwise."""
        from .errors import NotLoaded

        rows = self._instance.__dict__.get(self._relation.name)
        if rows is None:
            raise NotLoaded(f"{self._relation.model.__name__}.{self._relation.name} was not prefetched")
        return list(rows)

    async def _fetch(self) -> list[M]:
        rows = self._instance.__dict__.get(self._relation.name)
        pristine = len(self._filters) == 1 and not self._order and self._limit is None and self._offset is None
        if rows is not None and pristine and not self._prefetch and not self._related:
            return list(rows)
        if debug._scope.get() is not None and pristine:
            with debug.relation_load(self._relation.model.__name__, self._relation.name, "prefetch_related"):
                return await super()._fetch()
        return await super()._fetch()

    # -- links ------------------------------------------------------------------------------

    def _links(self) -> QuerySet[Any]:
        """The join rows of this instance."""
        join = self._relation.through
        source: ColumnRef[Any] = ColumnRef(join, (), join._meta.fields[self._relation.source])
        db = self._db if self._db is not None else self._instance.__dict__.get("_db")
        return join.objects.using(db).filter(source == self._key)

    def _target_keys(self, objs: Iterable[Any]) -> list[Any]:
        """Keys of related instances (or the keys themselves)."""
        to = self._relation.to
        target = self._relation.target
        keys = []
        for o in objs:
            if isinstance(o, target):
                keys.append(o._field_value(to))
            elif hasattr(o, "_meta"):
                raise TypeError(f"{self._relation.model.__name__}.{self._relation.name} links {target.__name__}, not {o!r}")
            else:
                keys.append(o)
        return list(dict.fromkeys(keys))

    def _target_col(self) -> ColumnRef[Any]:
        join = self._relation.through
        return ColumnRef(join, (), join._meta.fields[self._relation.target_field])

    def _forget(self) -> None:
        # prefetched rows no longer match the links
        self._instance.__dict__.pop(self._relation.name, None)

    async def add(self, *objs: Any, through_defaults: Mapping[str, Any] | None = None) -> None:
        """Link the given instances (or keys); links that exist are left alone.
        ``through_defaults`` sets other fields of the new join rows
        (``post.tags.add(tag, through_defaults={"position": 1})``)."""
        extra = self._through_defaults(through_defaults)
        keys = self._target_keys(objs)
        if not keys:
            return
        col = self._target_col()
        have = set(await self._links().filter(col.in_(keys)).select(col).scalars())
        link = {self._relation.source: self._key}
        rows = [{**extra, **link, self._relation.target_field: k} for k in keys if k not in have]
        if rows:
            await self._links().insert_many(rows)
        self._forget()

    def _through_defaults(self, values: Mapping[str, Any] | None) -> dict[str, Any]:
        join = self._relation.through
        keys = {self._relation.source, self._relation.target_field}
        for rel in join._meta.relations.values():
            if isinstance(rel, BelongsTo) and rel.via in keys:
                keys.add(rel.name)
        bad = sorted(keys & set(values or ()))
        if bad:
            raise TypeError(f"through_defaults can't set the link's key {', '.join(bad)}")
        return dict(values or {})

    async def remove(self, *objs: Any) -> int:
        """Unlink the given instances (or keys); returns the number of links removed."""
        keys = self._target_keys(objs)
        if not keys:
            return 0
        n = await self._links().filter(self._target_col().in_(keys)).delete()
        self._forget()
        return n

    async def clear(self) -> int:
        """Remove every link of this instance; returns how many there were."""
        n = await self._links().delete()
        self._forget()
        return n

    async def set(self, objs: Iterable[Any], *, through_defaults: Mapping[str, Any] | None = None) -> None:
        """Make the given instances (or keys) exactly the linked ones;
        ``through_defaults`` sets other fields of the new join rows."""
        self._through_defaults(through_defaults)
        keys = self._target_keys(objs)
        col = self._target_col()
        await self._links().filter(col.not_in(keys)).delete()
        await self.add(*keys, through_defaults=through_defaults)

    async def insert(self, **values: Any) -> M:  # type: ignore[override]
        """Insert a related row and link it, in one transaction."""
        from .db import resolve

        db = self._db if self._db is not None else self._instance.__dict__.get("_db")
        async with resolve(db).transaction():
            obj: M = await QuerySet(self._model).using(db).insert(**values)
            await self.add(obj)
        return obj

    async def get_or_insert(self, defaults: Mapping[str, Any] | None = None, **lookup: Any) -> tuple[M, bool]:
        raise TypeError(
            f"{self._relation.model.__name__}.{self._relation.name}.get_or_insert() isn't supported; "
            f"use {self._model.__name__}.objects.get_or_insert(), then add()"
        )

    def insert_many(  # type: ignore[override]
        self, rows: Iterable[Mapping[str, Any]], *, batch_size: int | None = None, copy: bool = False
    ) -> InsertMany[M]:
        raise TypeError(
            f"{self._relation.model.__name__}.{self._relation.name}.insert_many() isn't supported; "
            f"insert the rows, then link them with add()"
        )
