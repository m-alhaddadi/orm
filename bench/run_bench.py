"""Phase 0 feasibility benchmark.

Compares reading and writing 1 / 50 / 1000 posts through:

  django-async       Django async ORM (model instances)
  django-async-dict  Django async ORM, .values() dicts (reads only)
  sqla-asyncpg       SQLAlchemy 2.0 AsyncSession on asyncpg
  sqla-psycopg       SQLAlchemy 2.0 AsyncSession on psycopg 3 (same driver as Django)
  ormcore-obj        PyO3 + SeaORM, returns #[pyclass] objects
  ormcore-dict       PyO3 + SeaORM, returns dicts (reads only)
  ormcore-sync       PyO3 + SeaORM sync API (block_on, GIL released), #[pyclass] objects
  rust-only          SeaORM timed inside Rust: no Python objects, no event-loop hops
  django-sync        Django sync ORM, for reference

Every timed read also touches every field of every returned row, so lazily converted
attributes can't look artificially cheap.

Usage:  python bench/run_bench.py [--transport unix|tcp] [--quick] [--only NAME,...]
"""

import argparse
import asyncio
import gc
import json
import os
import platform
import statistics
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "django_bench"))
sys.path.insert(0, str(HERE))
os.environ.setdefault("DJANGO_SETTINGS_MODULE", "benchsite.settings")

import django  # noqa: E402
import ormcore  # noqa: E402
import sqlalchemy  # noqa: E402
from sqlalchemy import select  # noqa: E402
from sqlalchemy.ext.asyncio import AsyncSession, create_async_engine  # noqa: E402
from sqlalchemy.orm import joinedload  # noqa: E402

from sa_models import Post as SaPost  # noqa: E402

# Django models are imported in setup_django(), after PGHOST is chosen.
DjPost = dj_connection = None

PG_USER = os.environ.get("PGUSER", "postgres")
PG_PASSWORD = os.environ.get("PGPASSWORD", "postgres")
PG_DB = os.environ.get("PGDATABASE", "ormbench")
PG_TCP_HOST = os.environ.get("PG_TCP_HOST", "localhost")
PG_SOCKET_DIR = os.environ.get("PG_SOCKET_DIR", "/var/run/postgresql")
PG_PORT = os.environ.get("PGPORT", "5432")


def urls(transport):
    """Connection URLs per driver. Every contender uses the same transport."""
    auth = f"{PG_USER}:{PG_PASSWORD}"
    if transport == "tcp":
        base = f"{auth}@{PG_TCP_HOST}:{PG_PORT}/{PG_DB}"
        return dict(
            django_host=PG_TCP_HOST,
            sqlx=f"postgres://{base}",
            asyncpg=f"postgresql+asyncpg://{base}",
            psycopg=f"postgresql+psycopg://{base}",
        )
    q = f"?host={PG_SOCKET_DIR}&port={PG_PORT}"
    return dict(
        django_host=PG_SOCKET_DIR,
        sqlx=f"postgres://{auth}@{PG_SOCKET_DIR.replace('/', '%2F')}:{PG_PORT}/{PG_DB}",
        asyncpg=f"postgresql+asyncpg://{auth}@/{PG_DB}{q}",
        psycopg=f"postgresql+psycopg://{auth}@/{PG_DB}{q}",
    )


def setup_django(host):
    global DjPost, dj_connection
    os.environ["PGHOST"] = host
    django.setup()
    from blog.models import Post
    from django.db import connection

    DjPost, dj_connection = Post, connection


SEED_MAX_ID = 1000

SIZES = (1, 50, 1000)
OPS = ("read", "read_join", "write_bulk", "write_loop")
ITERS = {
    "read": {1: 1000, 50: 300, 1000: 50},
    "read_join": {1: 1000, 50: 300, 1000: 50},
    "write_bulk": {1: 500, 50: 200, 1000: 30},
    "write_loop": {1: 500, 50: 20, 1000: 3},
}


def warmup_for(iters):
    return max(3, iters // 10)


def make_rows(n):
    now = datetime.now(timezone.utc)
    return [
        dict(
            author_id=i % 50 + 1,
            title=f"Bench post {i}",
            body="Lorem ipsum dolor sit amet, consectetur adipiscing elit. " * 4,
            views=i,
            published=i % 2 == 0,
            created_at=now,
        )
        for i in range(n)
    ]


# --- field touching -------------------------------------------------------------


def touch_obj(rows, join=False):
    s = 0
    for p in rows:
        s += p.id + p.author_id + p.views + len(p.title) + len(p.body) + p.published
        p.created_at
        if join:
            a = p.author
            s += a.id + len(a.name) + len(a.email)
            a.created_at
    return s


def touch_dict(rows, join=False):
    s = 0
    for p in rows:
        s += p["id"] + p["author_id"] + p["views"] + len(p["title"]) + len(p["body"])
        s += p["published"]
        p["created_at"]
        if join:
            a = p["author"]
            s += a["id"] + len(a["name"]) + len(a["email"])
            a["created_at"]
    return s


def touch_dj_values_join(rows):
    s = 0
    for p in rows:
        s += p["id"] + p["author_id"] + p["views"] + len(p["title"]) + len(p["body"])
        s += p["published"] + len(p["author__name"]) + len(p["author__email"])
        p["created_at"]
        p["author__created_at"]
    return s


DJ_VALUES_JOIN = (
    "id", "author_id", "title", "body", "views", "published", "created_at",
    "author__id", "author__name", "author__email", "author__created_at",
)


# --- contenders -----------------------------------------------------------------
# Each contender exposes async callables  op(n, rows) -> None  for the ops it supports.


class Ormcore:
    def __init__(self, client, mode):
        self.c, self.mode = client, mode
        self.touch = touch_obj if mode == "obj" else touch_dict

    async def read(self, n, _):
        self.touch(await self.c.fetch_posts(n, self.mode))

    async def read_join(self, n, _):
        self.touch(await self.c.fetch_posts_with_author(n, self.mode), join=True)

    async def write_bulk(self, n, rows):
        await self.c.insert_posts(rows)

    async def write_loop(self, n, rows):
        for r in rows:
            await self.c.insert_post(r)


class DjangoAsync:
    async def read(self, n, _):
        touch_obj([p async for p in DjPost.objects.order_by("id")[:n]])

    async def read_join(self, n, _):
        qs = DjPost.objects.select_related("author").order_by("id")[:n]
        touch_obj([p async for p in qs], join=True)

    async def write_bulk(self, n, rows):
        await DjPost.objects.abulk_create([DjPost(**r) for r in rows])

    async def write_loop(self, n, rows):
        for r in rows:
            await DjPost.objects.acreate(**r)


class DjangoAsyncDict:
    async def read(self, n, _):
        touch_dict([p async for p in DjPost.objects.order_by("id").values()[:n]])

    async def read_join(self, n, _):
        qs = DjPost.objects.order_by("id").values(*DJ_VALUES_JOIN)[:n]
        touch_dj_values_join([p async for p in qs])


class SqlaAsync:
    def __init__(self, engine):
        self.engine = engine

    def session(self):
        return AsyncSession(self.engine, expire_on_commit=False)

    async def read(self, n, _):
        async with self.session() as s:
            touch_obj((await s.scalars(select(SaPost).order_by(SaPost.id).limit(n))).all())

    async def read_join(self, n, _):
        stmt = select(SaPost).options(joinedload(SaPost.author)).order_by(SaPost.id).limit(n)
        async with self.session() as s:
            touch_obj((await s.scalars(stmt)).all(), join=True)

    async def write_bulk(self, n, rows):
        async with self.session() as s:
            s.add_all([SaPost(**r) for r in rows])
            await s.commit()

    async def write_loop(self, n, rows):
        for r in rows:
            async with self.session() as s:
                s.add(SaPost(**r))
                await s.commit()


class OrmcoreSync:
    def __init__(self, client):
        self.c = client

    def read(self, n, _):
        touch_obj(self.c.fetch_posts_sync(n, "obj"))

    def read_join(self, n, _):
        touch_obj(self.c.fetch_posts_with_author_sync(n, "obj"), join=True)

    def write_bulk(self, n, rows):
        self.c.insert_posts_sync(rows)

    def write_loop(self, n, rows):
        for r in rows:
            self.c.insert_post_sync(r)


class DjangoSync:
    """Sync reference; run from a worker thread so Django's async-safety check is happy."""

    def read(self, n, _):
        touch_obj(list(DjPost.objects.order_by("id")[:n]))

    def read_join(self, n, _):
        touch_obj(list(DjPost.objects.select_related("author").order_by("id")[:n]), join=True)

    def write_bulk(self, n, rows):
        DjPost.objects.bulk_create([DjPost(**r) for r in rows])

    def write_loop(self, n, rows):
        for r in rows:
            DjPost.objects.create(**r)


# --- harness --------------------------------------------------------------------


def summarize(samples_ns, n):
    s = sorted(samples_ns)
    med = statistics.median(s)
    return dict(
        n=n,
        iters=len(s),
        median_us=med / 1e3,
        p95_us=s[min(len(s) - 1, int(len(s) * 0.95))] / 1e3,
        mean_us=statistics.fmean(s) / 1e3,
        us_per_row=med / 1e3 / n,
    )


async def time_async(fn, op, n, admin, quick):
    iters = ITERS[op][n] if not quick else max(3, ITERS[op][n] // 10)
    out = []
    for i in range(warmup_for(iters) + iters):
        rows = make_rows(n) if op.startswith("write") else None
        t0 = time.perf_counter_ns()
        await fn(n, rows)
        dt = time.perf_counter_ns() - t0
        if op.startswith("write"):
            await admin.delete_posts_above(SEED_MAX_ID)
        if i >= warmup_for(iters):
            out.append(dt)
    return out


def time_sync(fn, op, n, admin_sync, quick):
    iters = ITERS[op][n] if not quick else max(3, ITERS[op][n] // 10)
    out = []
    for i in range(warmup_for(iters) + iters):
        rows = make_rows(n) if op.startswith("write") else None
        t0 = time.perf_counter_ns()
        fn(n, rows)
        dt = time.perf_counter_ns() - t0
        if op.startswith("write"):
            admin_sync()
        if i >= warmup_for(iters):
            out.append(dt)
    return out


async def vacuum(admin):
    await admin.delete_posts_above(SEED_MAX_ID)
    await admin.execute("VACUUM ANALYZE blog_post")


async def main_async(args, u):
    admin = await ormcore.connect(u["sqlx"])
    rust_client = await ormcore.connect(u["sqlx"])
    sqla_asyncpg = create_async_engine(u["asyncpg"], pool_size=1)
    sqla_psycopg = create_async_engine(u["psycopg"], pool_size=1)

    contenders = {
        "django-async": DjangoAsync(),
        "django-async-dict": DjangoAsyncDict(),
        "sqla-asyncpg": SqlaAsync(sqla_asyncpg),
        "sqla-psycopg": SqlaAsync(sqla_psycopg),
        "ormcore-obj": Ormcore(rust_client, "obj"),
        "ormcore-dict": Ormcore(rust_client, "dict"),
        "ormcore-sync": OrmcoreSync(rust_client),
        "rust-only": None,
        "django-sync": DjangoSync(),
    }
    if args.only:
        contenders = {k: v for k, v in contenders.items() if k in args.only}

    def admin_sync():
        with dj_connection.cursor() as cur:
            cur.execute("DELETE FROM blog_post WHERE id > %s", [SEED_MAX_ID])

    results = []
    for name, impl in contenders.items():
        for op in OPS:
            await vacuum(admin)
            for n in SIZES:
                if name == "rust-only":
                    iters = ITERS[op][n] if not args.quick else max(3, ITERS[op][n] // 10)
                    samples = await rust_client.rust_bench(
                        op, n, iters, warmup_for(iters), SEED_MAX_ID
                    )
                elif name == "django-sync":
                    samples = await asyncio.to_thread(
                        time_sync, getattr(impl, op), op, n, admin_sync, args.quick
                    )
                elif name == "ormcore-sync":
                    samples = time_sync(
                        getattr(impl, op), op, n,
                        lambda: admin.delete_posts_above_sync(SEED_MAX_ID), args.quick,
                    )
                elif hasattr(impl, op):
                    samples = await time_async(getattr(impl, op), op, n, admin, args.quick)
                else:
                    continue
                r = dict(contender=name, op=op, **summarize(samples, n))
                results.append(r)
                print(
                    f"{name:18} {op:11} n={n:<5} median={r['median_us']:10.1f}us "
                    f"p95={r['p95_us']:10.1f}us  {r['us_per_row']:8.2f}us/row",
                    flush=True,
                )
            gc.collect()

    await vacuum(admin)
    await sqla_asyncpg.dispose()
    await sqla_psycopg.dispose()
    return results


def pg_version():
    with dj_connection.cursor() as cur:
        cur.execute("SHOW server_version")
        return cur.fetchone()[0]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true", help="10%% of the iterations")
    ap.add_argument("--only", type=lambda s: set(s.split(",")), default=None)
    ap.add_argument("--transport", choices=("unix", "tcp"), default="unix")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    out = Path(args.out or HERE / f"results-python-{args.transport}.json")
    u = urls(args.transport)
    setup_django(u["django_host"])

    env = dict(
        python=platform.python_version(),
        django=django.get_version(),
        sqlalchemy=sqlalchemy.__version__,
        postgres=pg_version(),
        cpus=os.cpu_count(),
        platform=platform.platform(),
        transport=args.transport,
    )
    dj_connection.close()  # don't hold the main thread's connection open

    results = asyncio.run(main_async(args, u))
    out.write_text(json.dumps(dict(env=env, results=results), indent=2))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
