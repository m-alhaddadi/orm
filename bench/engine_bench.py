"""End-to-end timings of the `orm` package on the blog models, for comparing engines.

    maturin develop --release
    python bench/engine_bench.py [--url URL] [--out results.json]

Every case runs through the public Python API (query set -> IR -> native engine ->
Postgres -> instances), so it measures what users get. Median of `--rounds` timed
batches after a warm-up.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import statistics
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "examples"))

from blog.models import Comment, Post, User  # noqa: E402

import orm  # noqa: E402


async def seed() -> None:
    db = orm.get_database()
    await db.drop_tables()
    await db.create_tables()
    users = await User.objects.insert_many([{"email": f"u{i}@x.io", "name": f"User {i}"} for i in range(10)]).returning()
    await Post.objects.insert_many(
        [{"author": users[i % 10], "title": f"post {i}", "body": "x" * 200, "views": i} for i in range(1000)]
    )


async def timed(fn, rounds: int, inner: int) -> float:
    for _ in range(max(inner // 4, 3)):
        await fn()
    samples = []
    for _ in range(rounds):
        t = time.perf_counter()
        for _ in range(inner):
            await fn()
        samples.append((time.perf_counter() - t) / inner)
    return statistics.median(samples) * 1e6


async def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="postgres://postgres:postgres@localhost/orm_test")
    ap.add_argument("--rounds", type=int, default=15)
    ap.add_argument("--out")
    args = ap.parse_args()

    await orm.connect(args.url, max_connections=4)
    await seed()
    counter = iter(range(10**9))

    async def insert_one():
        await User.objects.insert(email=f"b{next(counter)}@x.io", name="B")

    async def insert_50():
        n = next(counter)
        await User.objects.insert_many([{"email": f"m{n}-{i}@x.io", "name": "M"} for i in range(50)])

    async def tx_2_writes():
        async with orm.get_database().transaction():
            await Post.objects.filter(Post.id == 1).update(views=Post.views + 1)
            await Post.objects.filter(Post.id == 2).update(views=Post.views + 1)

    get_by_pk = Post.objects.filter(Post.id == orm.param("id")).prepare()
    read_page = Post.objects.order_by(Post.id).limit(orm.param("n")).prepare()

    cases = {
        "get by pk": (lambda: Post.objects.get(Post.id == 500), 200),
        "get by pk, prepared": (lambda: get_by_pk.get(id=500), 200),
        "read 50": (lambda: Post.objects.order_by(Post.id)[:50], 100),
        "read 50, prepared": (lambda: read_page(n=50), 100),
        "read 1000": (lambda: Post.objects.all(), 20),
        "read 1000 + select_related": (lambda: Post.objects.load(Post.author), 20),
        "10 users + prefetch 1000 posts": (lambda: User.objects.load(User.posts), 20),
        "count with EXISTS filter": (lambda: User.objects.filter(User.posts.views > 990).count(), 200),
        "insert 1": (insert_one, 200),
        "insert_many 50": (insert_50, 50),
        "update 100 rows": (lambda: Post.objects.filter(Post.id <= 100).update(views=Post.views + 1), 100),
        "transaction, 2 updates": (tx_2_writes, 100),
        "10 concurrent gets": (lambda: asyncio.gather(*(Post.objects.get(Post.id == i) for i in range(1, 11))), 50),
    }
    _ = Comment  # registered for the schema
    results = {}
    for name, (fn, inner) in cases.items():
        results[name] = round(await timed(fn, args.rounds, inner), 1)
        print(f"{name:34} {results[name]:9.1f} µs")
    if args.out:
        Path(args.out).write_text(json.dumps(results, indent=2) + "\n")
    await orm.get_database().drop_tables()


if __name__ == "__main__":
    asyncio.run(main())
