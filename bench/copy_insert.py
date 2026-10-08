"""Bulk load: insert_many(rows, copy=True) against batched insert_many(rows).

    ORM_TEST_DATABASE_URL=postgres://... python bench/copy_insert.py [rows] [repeats]

Uses the blog example's `posts` table. It drops and creates the blog tables, so point
it at a scratch database. Prints the best time of each method in seconds.
"""

import asyncio
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "examples"))

from blog.models import Post, User  # noqa: E402

import orm  # noqa: E402

URL = os.environ.get("ORM_TEST_DATABASE_URL", "postgres://postgres:postgres@localhost/orm_test")


async def best(db: orm.Database, load) -> float:
    times = []
    for _ in range(REPEATS):
        await db.execute("TRUNCATE posts RESTART IDENTITY CASCADE")
        start = time.perf_counter()
        await load()
        times.append(time.perf_counter() - start)
    return min(times)


async def main() -> None:
    db = await orm.connect(URL, max_connections=2)
    await db.drop_tables()
    await db.create_tables()
    try:
        alice = await User.objects.insert(email="a@x.io", name="A")
        rows = [{"author_id": alice.id, "title": f"title {i}", "body": "body " * 10, "views": i} for i in range(ROWS)]
        insert = await best(db, lambda: Post.objects.insert_many(rows))
        copy = await best(db, lambda: Post.objects.insert_many(rows, copy=True))
        print(f"rows={ROWS} repeats={REPEATS}")
        print(f"insert_many           {insert:.3f}s  ({ROWS / insert:,.0f} rows/s)")
        print(f"insert_many(copy=True) {copy:.3f}s  ({ROWS / copy:,.0f} rows/s)  x{insert / copy:.1f}")
    finally:
        await db.drop_tables()
        await db.close()


ROWS = int(sys.argv[1]) if len(sys.argv) > 1 else 200_000
REPEATS = int(sys.argv[2]) if len(sys.argv) > 2 else 3

if __name__ == "__main__":
    asyncio.run(main())
