"""Explicit to-one loads, sharing the owner's existing relation cache."""
from __future__ import annotations

import asyncio
from typing import Any

from .errors import IntegrityError, NotLoaded
from .fields import BelongsTo, HasOne


_MISSING = object()


def peek_key(owner: Any, name: str) -> Any:
    data = owner.__dict__
    return data[name] if name in data else data.get("_orm_internal", {}).get(name, _MISSING)


def key_value(owner: Any, name: str) -> Any:
    value = peek_key(owner, name)
    if value is _MISSING:
        raise NotLoaded(f"{owner._meta.name}.{name} is not loaded")
    return value


class ReferenceBelongsTo(BelongsTo[Any, Any]):
    filtered: bool = False
    optional: bool = False

    def __get__(self, obj: Any, owner: Any) -> Any:
        if obj is None:
            return super().__get__(obj, owner)
        data = obj.__dict__
        key = key_value(obj, self.via)
        if self.name in data:
            value = data[self.name]
            keys = data.get("_reference_keys", {})
            absent_is_valid = key is None or self.filtered or (self.optional and self.name not in keys) or keys.get(self.name, _MISSING) == key
            if value is None and absent_is_valid:
                return None
            if value is not None:
                target_key = peek_key(value, self.to)
                if target_key == key or (target_key is _MISSING and keys.get(self.name, _MISSING) == key):
                    return value
        elif key is None:
            return None
        raise NotLoaded(f"{owner.__name__}.{self.name} is not loaded; use {loader_name(self.name)}() or select_related({owner.__name__}.{self.name})")


def loader_name(name: str) -> str:
    return f"load_{name}"


def validate(model: Any, fields: Any, relations: Any, behavior: Any) -> None:
    methods = {m["name"] for m in behavior.get("methods", ()) if m["model"] == model.__name__}
    for name, relation in relations.items():
        if not isinstance(relation, (BelongsTo, HasOne)):
            continue
        method = loader_name(name)
        existing = getattr(model, method, None)
        if method in fields or method in relations or method in methods or (hasattr(model, method) and getattr(existing, "_reference_relation", None) != name):
            raise TypeError(f"{model.__name__}.{method}: reference loader collides with an existing member")


def install(model: Any) -> None:
    references = []
    for name, relation in model._meta.relations.items():
        if not isinstance(relation, (BelongsTo, HasOne)):
            continue
        if isinstance(relation, BelongsTo) and not isinstance(relation, ReferenceBelongsTo):
            specialized = ReferenceBelongsTo.__new__(ReferenceBelongsTo)
            specialized.__dict__.update(relation.__dict__)
            relation = specialized
            model._meta.relations[name] = relation
            setattr(model, name, relation)
        filtered = any(policy.get("model") == relation.target_name and policy.get("filter") is not None for policy in model._meta.registry._behavior.get("query_defaults", ()))
        if isinstance(relation, ReferenceBelongsTo):
            relation.filtered = filtered
            relation.optional = model._meta.fields[relation.via].nullable
        references.append((name, relation))
        method = loader_name(name)
        existing = getattr(model, method, None)
        if hasattr(model, method) and getattr(existing, "_reference_relation", None) != name:
            raise TypeError(f"{model.__name__}.{method}: reference loader collides with an existing member")
        setattr(model, method, make_loader(name, filtered=filtered))
    if not references:
        return
    # Capture only relation source keys; ordinary rows keep the baseline method.
    dependencies = tuple((name, r.via if isinstance(r, BelongsTo) else r.from_) for name, r in references)
    sources = tuple(source for _, source in dependencies)
    reverse_names = tuple(name for name, r in references if isinstance(r, HasOne))
    replace = model._replace_from
    if getattr(replace, "_reference_adapter", False):
        replace = replace._reference_base
    def replace_from(self: Any, fresh: Any) -> None:
        before = {source: peek_key(self, source) for source in sources}
        replace(self, fresh)
        for name, source in dependencies:
            if before[source] != peek_key(self, source):
                invalidate_reference(self, name)
    replace_from._reference_adapter = True  # type: ignore[attr-defined]
    replace_from._reference_base = replace  # type: ignore[attr-defined]
    setattr(model, "_replace_from", replace_from)
    baseline_refresh = model.refresh
    if getattr(baseline_refresh, "_reference_adapter", False):
        baseline_refresh = baseline_refresh._reference_base
    async def refresh(self: Any, *fields: Any) -> None:
        before = {source: peek_key(self, source) for source in sources}
        await baseline_refresh(self, *fields)
        for name, source in dependencies:
            if name in reverse_names or before[source] != peek_key(self, source):
                invalidate_reference(self, name)
    refresh._reference_adapter = True  # type: ignore[attr-defined]
    refresh._reference_base = baseline_refresh  # type: ignore[attr-defined]
    setattr(model, "refresh", refresh)


def make_loader(name: str, *, filtered: bool = False) -> Any:
    async def load(self: Any, *, reload: bool = False) -> Any:
        return await load_reference(self, name, reload=reload, filtered=filtered)
    load.__name__ = loader_name(name)
    load._reference_relation = name  # type: ignore[attr-defined]
    return load


async def load_reference(owner: Any, name: str, *, reload: bool = False, filtered: bool = False) -> Any:
    from .db import resolve

    relation = owner._meta.relations[name]
    source = relation.via if isinstance(relation, BelongsTo) else relation.from_
    key = key_value(owner, source)
    data = owner.__dict__
    if not reload:
        try:
            return getattr(owner, name)
        except NotLoaded:
            pass
    db = resolve(data.get("_db"))
    context = (key, db, db._tx())
    async def fetch() -> Any:
        target = relation.target
        to = relation.to if isinstance(relation, BelongsTo) else relation.via
        value = None if key is None else await target.objects.using(db).filter(getattr(target, to) == key).first()
        if value is None and isinstance(relation, BelongsTo) and not owner._meta.fields[source].nullable and not filtered:
            raise IntegrityError(f"{owner._meta.name}.{name}: required target {relation.target_name} is missing")
        return value
    def publish(value: Any) -> None:
        if key_value(owner, source) == key:
            data[name] = value
            data.setdefault("_reference_keys", {})[name] = key
    return await coalesced_load(owner, name, context, fetch, publish)


async def coalesced_load(owner: Any, name: str, context: tuple[Any, ...], fetch: Any, publish: Any) -> Any:
    """Internal seam for ordinary/generic references: context includes keys, DB and TX.

    The caller validates dependencies and owns its cache policy. Invalidation removes
    the name's token/pending entry. A cancelled waiter leaves shared work running.
    """
    data = owner.__dict__
    pending = data.setdefault("_reference_pending", {})
    entry = pending.get(name)
    if entry is not None and entry[0] == context and not entry[1].done():
        return await asyncio.shield(entry[1])
    tokens = data.setdefault("_reference_tokens", {})
    token = object()
    tokens[name] = token
    async def run() -> Any:
        try:
            value = await fetch()
            if tokens.get(name) is token:
                publish(value)
            return value
        finally:
            if pending.get(name, (None, None))[1] is asyncio.current_task():
                pending.pop(name, None)
    task = asyncio.create_task(run())
    task.add_done_callback(lambda done: None if done.cancelled() else done.exception())
    pending[name] = (context, task)
    return await asyncio.shield(task)


def invalidate_reference(owner: Any, name: str) -> None:
    """Invalidate a selected ordinary/generic reference without cancelling its waiters."""
    data = owner.__dict__
    data.pop(name, None)
    for storage in ("_reference_keys", "_reference_tokens", "_reference_pending"):
        data.get(storage, {}).pop(name, None)
