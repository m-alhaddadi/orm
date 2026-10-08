"""Where one query's time goes: Python query building, IR, Rust planning, the
asyncio <-> Tokio hand-off, the database, building instances; and what loading the
schema costs at startup.

Needs the profiling probes, which are not part of the shipped module:

    git apply bench/profile_probes.patch && maturin develop --release
    python bench/profile_query.py [--url URL] [--uvloop]
    git apply -R bench/profile_probes.patch
"""

from __future__ import annotations

import argparse
import asyncio
import copy
import json
import os
import statistics
import sys
import time
import timeit
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "examples"))

import blog.models as blog  # noqa: E402
from blog.models import Post, User  # noqa: E402

import orm  # noqa: E402
from orm import _native as N  # noqa: E402

if not hasattr(N, "_probe_plan"):
    sys.exit("build with bench/profile_probes.patch applied first (see the docstring)")


async def amed(fn: Any, n: int, rounds: int = 7) -> float:
    for _ in range(max(n // 10, 50)):
        await fn()
    samples = []
    for _ in range(rounds):
        t = time.perf_counter()
        for _ in range(n):
            await fn()
        samples.append((time.perf_counter() - t) / n)
    return statistics.median(samples) * 1e6


def smed(fn: Any, n: int) -> float:
    return min(timeit.repeat(fn, number=n, repeat=5)) / n * 1e6


def schema_load() -> None:
    print("== schema load (once per process) ==")
    base = json.loads(blog._SCHEMA)
    names = [m["name"] for m in base["models"]]

    def scaled(k: int) -> str:
        out: dict[str, Any] = {"models": [], "enums": base.get("enums", [])}
        for i in range(k):
            ren = {n: f"{n}{i}" for n in names}
            for m in base["models"]:
                m = copy.deepcopy(m)
                m["name"], m["table"] = ren[m["name"]], f"{m['table']}_{i}"
                for r in m.get("relations", []):
                    r["target"] = ren[r["target"]]
                    if "through" in r:
                        r["through"]["model"] = ren[r["through"]["model"]]
                out["models"].append(m)
        return json.dumps(out, indent=2)

    for k in (1, 34):
        src = scaled(k)
        t0 = time.perf_counter()
        ir = json.loads(src)
        t1 = time.perf_counter()
        reg = orm.model.Registry()
        orm.define(ir, registry=reg)
        t2 = time.perf_counter()
        js = json.dumps(reg.ir())
        t3 = time.perf_counter()
        N.Schema(js)
        t4 = time.perf_counter()
        print(
            f"  {len(ir['models']):4} models {len(src) / 1024:5.0f} KB: json.loads {1e3 * (t1 - t0):5.2f} ms, "
            f"define() {1e3 * (t2 - t1):5.2f} ms, json.dumps {1e3 * (t3 - t2):5.2f} ms, "
            f"Rust parse + index {1e3 * (t4 - t3):5.2f} ms"
        )


async def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="postgres://postgres:postgres@localhost/orm_test?sslmode=disable")
    ap.add_argument("--uvloop", action="store_true")
    args = ap.parse_args()

    schema_load()
    await orm.connect(args.url, max_connections=4)
    db = orm.get_database()
    eng = db._engine
    await db.drop_tables()
    await db.create_tables()
    users = await User.objects.insert_many([{"email": f"u{i}@x.io", "name": f"U{i}"} for i in range(10)]).returning()
    await Post.objects.insert_many(
        [{"author": users[i % 10], "title": f"p{i}", "body": "x" * 200, "views": i} for i in range(1000)]
    )
    schema = Post._meta.registry.native()

    print("== per query, Python side and Rust planning (µs) ==")
    print(f"  {'case':16} {'qs build':>8} {'IR dict':>8} {'dumps':>6} | {'parse':>6} {'plan':>6} {'build':>6}")
    cases = {
        "get by pk": (lambda: Post.objects.filter(Post.id == 500).limit(2), "select"),
        "read 50": (lambda: Post.objects.order_by(Post.id)[:50], "select"),
        "select_related": (lambda: Post.objects.select_related(Post.author), "select"),
        "EXISTS count": (lambda: User.objects.filter(User.posts.views > 990), "count"),
        "prefetch": (lambda: User.objects.prefetch_related(User.posts), "select"),
    }
    for name, (mk, op) in cases.items():
        qs = mk()
        params: list[Any] = []
        ir = qs._select_ir(op, params)
        js = json.dumps(ir)
        a, b, c = N._probe_plan(schema, js, params, 50000)
        print(
            f"  {name:16} {smed(mk, 20000):8.1f} {smed(lambda: qs._select_ir(op, []), 20000):8.1f} "
            f"{smed(lambda: json.dumps(ir), 20000):6.1f} | {a:6.2f} {b:6.2f} {c:6.2f}"
        )

    print("== asyncio <-> Tokio hand-off (µs) ==")
    loop = asyncio.get_running_loop()
    fd = os.eventfd(0, os.EFD_NONBLOCK | os.EFD_CLOEXEC)
    pending: list[asyncio.Future[Any]] = []

    def on_ready() -> None:
        os.eventfd_read(fd)
        pending.pop().set_result(N._probe_efd_take())

    loop.add_reader(fd, on_ready)

    def efd_noop() -> asyncio.Future[Any]:
        f = loop.create_future()
        pending.append(f)
        N._probe_efd_noop(fd)
        return f

    def efd_select1() -> asyncio.Future[Any]:
        f = loop.create_future()
        pending.append(f)
        N._probe_efd_select(eng, "SELECT 1", fd)
        return f

    print(f"  sync call, no work                 {smed(N._probe_noop_sync, 200000):7.2f}")
    print(f"  no-op, future_into_py (today)      {await amed(N._probe_noop_async, 3000):7.1f}")
    print(f"  no-op, eventfd completion          {await amed(efd_noop, 3000):7.1f}")
    print("== SELECT 1, prepared (µs) ==")
    print(f"  Rust loop, no Python               {N._probe_rust_loop(eng, 'SELECT 1', 3000):7.1f}")
    print(f"  sync block_on from Python          {smed(lambda: N._probe_select1_sync(eng, 'SELECT 1'), 3000):7.1f}")
    print(f"  async, future_into_py (today)      {await amed(lambda: N._probe_select1_async(eng, 'SELECT 1'), 3000):7.1f}")
    print(f"  async, eventfd completion          {await amed(efd_select1, 3000):7.1f}")

    for label, mk, n in [
        ("get by pk", lambda: Post.objects.filter(Post.id == 500).limit(2), 2000),
        ("read 1000", lambda: Post.objects.all(), 100),
    ]:
        sql = mk().sql()
        print(f"== {label} (µs) ==")
        print(f"  Rust fetch, no Python              {N._probe_rust_loop(eng, sql, n):7.1f}")
        print(f"  async fetch, no instances          {await amed(lambda: N._probe_select1_async(eng, sql), n):7.1f}")
        print(f"  ORM, query set prebuilt            {await amed(mk()._fetch, n):7.1f}")
        print(f"  ORM, end to end                    {await amed(lambda: mk()._fetch(), n):7.1f}")

    print("== in the real loop: where get / read 50 spend time (µs) ==")
    pc = time.perf_counter
    loop_cases = [
        ("get by pk", lambda: Post.objects.filter(Post.id == 500).limit(2)),
        ("read 50, 2 filters", lambda: Post.objects.filter(Post.views > 5, Post.title != "x").order_by(Post.id)[:50]),
    ]
    for label, mk in loop_cases:
        build, call, wait = [], [], []
        for i in range(3000):
            t0 = pc()
            q = mk()
            params = []
            js = json.dumps(q._select_ir("select", params))
            t1 = pc()
            fut = eng.run(js, params, None, None, db)
            t2 = pc()
            await fut
            t3 = pc()
            if i >= 500:
                build.append(t1 - t0)
                call.append(t2 - t1)
                wait.append(t3 - t2)
        m = lambda xs: statistics.median(xs) * 1e6  # noqa: E731
        print(f"  {label:20} Python IR + dumps {m(build):6.1f} | run() before await {m(call):6.1f} | await {m(wait):6.1f}")

    print("== plan cache A/B, interleaved (µs) ==")
    for label, mk in loop_cases:
        qs = mk()
        params = []
        js = json.dumps(qs._select_ir("select", params))

        def rust_cache(mk: Any = mk) -> Any:
            q = mk()
            pp: list[Any] = []
            return N._probe_run_cached(eng, json.dumps(q._select_ir("select", pp)), pp, db)

        variants = {
            "today, end to end": lambda mk=mk: mk()._fetch(),
            "+ Rust plan cache": rust_cache,
            "prepared query (IR prebuilt)": lambda js=js, params=params: eng.run(js, params, None, None, db),
            "prepared query + Rust plan cache": lambda js=js, params=params: N._probe_run_cached(eng, js, params, db),
        }
        res: dict[str, list[float]] = {k: [] for k in variants}
        for _ in range(300):
            for f in variants.values():
                await f()
        for _ in range(15):
            for k, f in variants.items():
                t = time.perf_counter()
                for _ in range(200):
                    await f()
                res[k].append((time.perf_counter() - t) / 200 * 1e6)
        print(f"  {label}")
        for k, v in res.items():
            print(f"    {k:34} {statistics.median(v):7.1f}")
    await db.drop_tables()


if __name__ == "__main__":
    if "--uvloop" in sys.argv:
        import uvloop

        uvloop.run(main())
    else:
        asyncio.run(main())
