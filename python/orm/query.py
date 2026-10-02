"""Lazy, immutable, awaitable query sets."""

from __future__ import annotations

import copy
import json
from collections.abc import AsyncIterator, Generator, Iterable, Mapping
from typing import TYPE_CHECKING, Any, Generic, TypeVar

from .errors import QueryError
from .expr import (
    ColumnRef,
    Condition,
    ConditionLike,
    Expression,
    IRContext,
    Ordering,
    RelationPath,
    and_,
    as_condition,
    not_,
)
from .fields import BelongsTo, HasMany
from .write import InsertMany, InsertOne, prepare_rows

if TYPE_CHECKING:
    from typing_extensions import Self

    from .db import Database
    from .model import Model

M = TypeVar("M", bound="Model")

__all__ = ["QuerySet", "RelatedSet"]


class QuerySet(Generic[M]):
    """A query over one model. Every method returns a new query set; nothing runs until
    the query set is awaited (``await User.objects.filter(...)`` gives a list) or a
    terminal coroutine (``first``, ``get``, ``count``, ``update``, ...) is awaited.

    Filters are expressions over the model's columns and relation paths::

        await User.objects.filter(User.posts.created_at < yesterday)

    Conditions inside one ``filter()`` call that go through the same to-many relation
    must hold for the same related row; separate ``filter()`` calls are independent.
    ``exclude(c)`` keeps rows for which ``c`` does not hold, e.g.
    ``exclude(User.posts.published == False)`` keeps users with no unpublished post.
    """

    __slots__ = ("_model", "_filters", "_order", "_limit", "_offset", "_related", "_prefetch", "_db")

    def __init__(self, model: type[M]) -> None:
        self._model = model
        self._filters: tuple[Condition, ...] = ()
        self._order: tuple[Ordering, ...] = ()
        self._limit: int | None = None
        self._offset: int | None = None
        self._related: tuple[tuple[str, ...], ...] = ()
        self._prefetch: tuple[str, ...] = ()
        self._db: Database | None = None

    @property
    def model(self) -> type[M]:
        return self._model

    def _clone(self, **changes: Any) -> Self:
        new = copy.copy(self)
        for k, v in changes.items():
            setattr(new, k, v)
        return new

    # -- building ------------------------------------------------------------------------

    def all(self) -> Self:
        return self._clone()

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
        return self._clone(_limit=n)

    def offset(self, n: int | None) -> Self:
        return self._clone(_offset=n or None)

    def __getitem__(self, s: slice) -> Self:
        """``qs[10:20]`` is ``qs.offset(10).limit(10)``."""
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

    def prefetch_related(self, *relations: RelationPath[Any]) -> Self:
        """Load to-many relations with one extra ``IN (...)`` query each, in the same
        call: ``User.objects.prefetch_related(User.posts)``."""
        prefetch = list(self._prefetch)
        for p in relations:
            self._check_path(p)
            if len(p._path) != 1:
                raise ValueError("prefetch_related supports direct relations only (for now)")
            if not isinstance(self._model._meta.relations[p._path[0]], HasMany):
                raise ValueError(f"{p!r} is to-one; use select_related")
            if p._path[0] not in prefetch:
                prefetch.append(p._path[0])
        return self._clone(_prefetch=tuple(prefetch))

    def using(self, db: Database | None) -> Self:
        return self._clone(_db=db)

    def _check_path(self, p: RelationPath[Any]) -> None:
        if not isinstance(p, RelationPath):
            raise TypeError(f"expected a relation such as {self._model.__name__}.<relation>, got {p!r}")
        if p._root is not self._model:
            raise ValueError(f"{p!r} does not start at {self._model.__name__}")

    # -- IR ------------------------------------------------------------------------------

    def _select_ir(self, op: str, params: list[Any]) -> dict[str, Any]:
        ctx = IRContext(self._model, params)
        ir: dict[str, Any] = {"op": op, "model": self._model._meta.name}
        if self._filters:
            ir["filters"] = [f._ir(ctx) for f in self._filters]
        if self._order:
            ir["order"] = [o._ir(ctx) for o in self._order]
        if self._limit is not None:
            ir["limit"] = self._limit
        if self._offset is not None:
            ir["offset"] = self._offset
        if self._related and op == "select":
            ir["select_related"] = [list(p) for p in self._related]
        if self._prefetch and op == "select":
            ir["prefetch"] = list(self._prefetch)
        return ir

    def _mutation_ir(self, op: str, params: list[Any]) -> dict[str, Any]:
        if self._limit is not None or self._offset is not None:
            raise QueryError(f"{op}() is not supported on a sliced query set")
        ctx = IRContext(self._model, params)
        return {"op": op, "model": self._model._meta.name, "filters": [f._ir(ctx) for f in self._filters]}

    def sql(self) -> str:
        """The SELECT this query set runs, with parameters inlined (for debugging)."""
        from .model import registry

        params: list[Any] = []
        ir = self._select_ir("select", params)
        return registry.native().sql(json.dumps(ir), params)

    # -- execution -----------------------------------------------------------------------

    async def _run(self, ir: dict[str, Any], params: list[Any]) -> Any:
        from .db import resolve

        return await resolve(self._db)._run(ir, params)

    def _default_order(self) -> tuple[Ordering, ...]:
        return self._order or (Ordering(self._model._meta.pk_ref(), desc=False),)

    async def _fetch(self) -> list[M]:
        params: list[Any] = []
        rows, prefetched = await self._run(self._select_ir("select", params), params)
        objs = self._materialize(rows, prefetched)
        if self._db is not None:  # instance writes go back to the same database
            for o in objs:
                o.__dict__["_db"] = self._db
        return objs

    def _materialize(self, rows: list[tuple[Any, ...]], prefetched: dict[str, list[tuple[Any, ...]]]) -> list[M]:
        model = self._model
        if self._related:
            layout = self._join_layout()
            objs = [self._materialize_joined(r, layout) for r in rows]
        else:
            make = model._from_row
            objs = [make(r) for r in rows]
        for name, related_rows in prefetched.items():
            self._attach_prefetched(objs, model._meta.relations[name], related_rows)  # type: ignore[arg-type]
        return objs

    def _join_layout(self) -> list[tuple[tuple[str, ...], type[Model], int, int]]:
        """(path, model, column count, pk position) per select_related path, in the order
        the engine appends their columns to each row."""
        layout = []
        for path in self._related:
            meta = self._model._meta
            for hop in path[:-1]:
                meta = meta.relations[hop].target._meta
            target = meta.relations[path[-1]].target
            tm = target._meta
            layout.append((path, target, len(tm.field_names), tm.field_names.index(tm.pk.name)))
        return layout

    def _materialize_joined(
        self, row: tuple[Any, ...], layout: list[tuple[tuple[str, ...], type[Model], int, int]]
    ) -> M:
        pos = len(self._model._meta.field_names)
        obj = self._model._from_row(row[:pos])
        by_path: dict[tuple[str, ...], Any] = {(): obj}
        for path, target, width, pk_pos in layout:
            chunk = row[pos : pos + width]
            pos += width
            # A LEFT JOIN without a match yields NULLs, including the primary key.
            child = None if chunk[pk_pos] is None else target._from_row(chunk)
            by_path[path] = child
            parent = by_path[path[:-1]]
            if parent is not None:
                parent.__dict__[path[-1]] = child
        return obj

    @staticmethod
    def _attach_prefetched(objs: list[Any], rel: HasMany[Any, Any], rows: list[tuple[Any, ...]]) -> None:
        target = rel.target
        key_pos = target._meta.field_names.index(rel.via)
        # The reverse to-one relation (Post.author for User.posts) gets filled in too.
        back = next(
            (
                r
                for r in target._meta.relations.values()
                if isinstance(r, BelongsTo) and r.via == rel.via and r.target is rel.model
            ),
            None,
        )
        groups: dict[Any, list[Any]] = {}
        make = target._from_row
        for r in rows:
            groups.setdefault(r[key_pos], []).append(make(r))
        for obj in objs:
            children = groups.get(obj.__dict__.get(rel.from_), [])
            if back is not None:
                for c in children:
                    c.__dict__[back.name] = obj
            obj.__dict__[rel.name] = children

    def __await__(self) -> Generator[Any, None, list[M]]:
        return self._fetch().__await__()

    async def __aiter__(self) -> AsyncIterator[M]:
        for obj in await self._fetch():
            yield obj

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
        params: list[Any] = []
        b: bool = await self._run(self._select_ir("exists", params), params)
        return b

    async def update(self, **values: Any) -> int:
        """``UPDATE`` every matching row; values may be expressions, e.g.
        ``views=Post.views + 1``. Returns the number of rows updated."""
        n: int = await self._update(values, returning=False)
        return n

    async def _update(self, values: dict[str, Any], *, returning: bool) -> Any:
        meta = self._model._meta
        params: list[Any] = []
        ir = self._mutation_ir("update", params)
        ctx = IRContext(self._model, params)
        assignments = []
        for name, value in values.items():
            rel = meta.relations.get(name)
            if isinstance(rel, BelongsTo):
                name, value = rel.via, (None if value is None else getattr(value, rel.to))
            if name not in meta.fields:
                raise TypeError(f"{meta.name} has no field {name!r}")
            node = value._ir(ctx) if isinstance(value, Expression) else ctx.param(value)
            assignments.append({"field": name, "value": node})
        if not assignments:
            return [] if returning else 0
        ir["set"] = assignments
        ir["returning"] = returning
        return await self._run(ir, params)

    async def delete(self) -> int:
        """``DELETE`` every matching row. Returns the number of rows deleted."""
        params: list[Any] = []
        n: int = await self._run(self._mutation_ir("delete", params), params)
        return n

    def insert(self, **values: Any) -> InsertOne[M]:
        """``INSERT`` one row; ``await`` gives the new instance with database defaults
        (id, timestamps) filled in. Chain ``.on_conflict(...)`` for an upsert."""
        fields, rows, provided = prepare_rows(self._model, [values])
        return InsertOne(self, fields, rows, provided)

    def insert_many(self, rows: Iterable[Mapping[str, Any]]) -> InsertMany[M]:
        """``INSERT`` many rows with one statement; ``await`` gives the new instances."""
        fields, aligned, provided = prepare_rows(self._model, rows)
        return InsertMany(self, fields, aligned, provided)

    def __repr__(self) -> str:
        parts = [f"filter{f!r}" for f in self._filters]
        if self._order:
            parts.append(f"order_by{self._order!r}")
        if self._limit is not None or self._offset is not None:
            parts.append(f"[{self._offset or 0}:{'' if self._limit is None else (self._offset or 0) + self._limit}]")
        return f"<{type(self).__name__} {self._model.__name__}{' ' if parts else ''}{'.'.join(parts)}>"


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
        key = instance.__dict__.get(relation.from_)
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
        return await super()._fetch()

    def insert(self, **values: Any) -> InsertOne[M]:
        """Insert a related row pointing at this instance."""
        return super().insert(**values, **self._link())

    def insert_many(self, rows: Iterable[Mapping[str, Any]]) -> InsertMany[M]:
        """Insert related rows pointing at this instance."""
        link = self._link()
        return super().insert_many({**r, **link} for r in rows)

    def _link(self) -> dict[str, Any]:
        return {self._relation.via: self._instance.__dict__[self._relation.from_]}
