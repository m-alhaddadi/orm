"""Fixed cost of one async round trip with no database work, per bridge."""

import asyncio
import statistics
import time

import ormcore
from asgiref.sync import sync_to_async

N = 20_000


def bench_sync(name, fn):
    for _ in range(1000):
        fn()
    s = []
    for _ in range(N):
        t0 = time.perf_counter_ns()
        fn()
        s.append(time.perf_counter_ns() - t0)
    print(f"| {name} | {statistics.median(s) / 1e3:.1f} |")


async def bench(name, make):
    for _ in range(1000):
        await make()
    s = []
    for _ in range(N):
        t0 = time.perf_counter_ns()
        await make()
        s.append(time.perf_counter_ns() - t0)
    print(f"| {name} | {statistics.median(s) / 1e3:.1f} |")


async def main():
    c = await ormcore.connect("postgres://postgres:postgres@%2Fvar%2Frun%2Fpostgresql/ormbench")

    async def coro():
        return None

    print("| bridge | median µs per call |\n|---|---:|")
    await bench("plain Python coroutine", coro)
    await bench("PyO3 `future_into_py` (Tokio thread → asyncio)", c.noop)
    await bench("Django `sync_to_async` (thread_sensitive)", sync_to_async(lambda: None))
    bench_sync("PyO3 sync `block_on` (GIL released, no asyncio)", c.noop_sync)


asyncio.run(main())
