"""The result cache of query sets: awaiting the same query set twice runs it once.

Each query set (and ``select()``) keeps the task of its first ``await``. Later awaits,
concurrent ones included, share it and get a copy of its rows, like Django's result
cache. Builders return new query sets with empty caches, so ``qs.filter(...)`` or
``qs.all()`` query again; a failed or cancelled run isn't kept, so awaiting again
retries. ``Model.objects`` lives as long as the model, so it never caches.
"""

from __future__ import annotations

import asyncio
from collections.abc import Callable, Coroutine
from typing import Any, TypeVar

from . import debug

T = TypeVar("T")


async def cached(owner: Any, fetch: Callable[[], Coroutine[Any, Any, list[T]]], *, enabled: bool = True) -> list[T]:
    """The rows of ``owner``'s first run (``owner._result`` holds the task)."""
    if not enabled:
        return await fetch()
    loop = asyncio.get_running_loop()
    task: asyncio.Task[list[T]] | None = owner._result
    if task is None or task.get_loop() is not loop or (task.done() and (task.cancelled() or task.exception() is not None)):
        task = debug.spawn(loop, fetch())
        owner._result = task
    return list(await asyncio.shield(task))
