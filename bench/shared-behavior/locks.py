"""Paired public API benchmark: baseline and candidate in alternating ABBA blocks."""
import argparse
import asyncio
import json
import os
import platform
import statistics
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'examples'))
from blog.models import User
from baseline import lock as baseline
import orm

async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--out', required=True)
    ap.add_argument('--rounds', type=int, default=24)
    ap.add_argument('--inner', type=int, default=200)
    ap.add_argument('--connections', type=int, default=1)
    ap.add_argument('--only', help='comma-separated case names')
    args = ap.parse_args()
    db = await orm.connect(os.environ['ORM_TEST_DATABASE_URL'], max_connections=args.connections)
    other = await orm.connect(db.url, default=False, max_connections=1)
    await db.create_tables()
    user = await User.objects.using(db).insert(email=f'lockbench-{time.time_ns()}@x.io', name='Bench')
    candidate = orm.Database.lock
    result = {'runtime': sys.version, 'platform': platform.platform(), 'rounds': args.rounds, 'inner': args.inner, 'connections': args.connections, 'cases': {}}
    keys = {'integer': 42, 'short': 'import', 'unicode': 'ورود 🔒', 'long': 'x' * 4096}
    async def measure(name, fn):
        if args.only and name not in args.only.split(','):
            return
        count = min(args.inner, 40) if name.startswith('long/') else args.inner
        samples = {'baseline': [], 'candidate': [], 'inner': count}
        for method in (baseline, candidate):
            orm.Database.lock = method
            for _ in range(30):
                await fn()
        for block in range(args.rounds):
            order = [('baseline', baseline), ('candidate', candidate)]
            if block % 2:
                order.reverse()
            for label, method in order:
                orm.Database.lock = method
                start = time.perf_counter_ns()
                for _ in range(count):
                    await fn()
                samples[label].append((time.perf_counter_ns() - start) / count / 1000)
        result['cases'][name] = samples
        b, c = (statistics.median(samples[k]) for k in ('baseline', 'candidate'))
        print(f'{name:28} {b:9.2f} -> {c:9.2f} us ({(c/b-1)*100:+.1f}%)', flush=True)
    async with db.transaction():
        for name, key in keys.items():
            for nowait in (False, True):
                for exclusive in (True, False):
                    async def fn(key=key, nowait=nowait, exclusive=exclusive):
                        assert await db.lock(key, nowait=nowait, exclusive=exclusive) is True
                    await measure(f'{name}/{nowait}/{exclusive}', fn)
        await db.lock('held')
        async with other.transaction():
            async def denied():
                assert await other.lock('held', nowait=True) is False
            await measure('contended', denied)
    async def transaction():
        async with db.transaction():
            assert await db.lock('import') is True
    await measure('transaction+short', transaction)
    async def concurrent():
        async def one(i):
            async with db.transaction():
                assert await db.lock(i, nowait=True) is True
        await asyncio.gather(*(one(i) for i in range(4)))
    await measure('concurrent transactions', concurrent)
    async def read_write():
        async with db.transaction():
            assert await db.lock('import') is True
            row = await User.objects.using(db).get(User.id == user.id)
            assert row.email == user.email
            assert await User.objects.using(db).filter(User.id == user.id).update(name='Bench') == 1
    await measure('transaction+read+write', read_write)
    await User.objects.using(db).filter(User.id == user.id).delete()
    orm.Database.lock = candidate
    result['server'] = await db._fetch_text('SELECT version()')
    Path(args.out).write_text(json.dumps(result, indent=2) + '\n')
    await other.close()
    await db.close()

asyncio.run(main())
