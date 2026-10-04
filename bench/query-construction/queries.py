"""Paired public Python API benchmark; load two release extensions in one process."""
import argparse
import asyncio
import importlib.util
import json
import platform
import sys
import time
from pathlib import Path

import orm
from orm import func, window

SOURCE = Path('examples/sqlite/schema.prisma').read_text()


def load(path, name):
    spec = importlib.util.spec_from_file_location(name + '._native', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--before', required=True)
    ap.add_argument('--after', required=True)
    ap.add_argument('--out', required=True)
    ap.add_argument('--url', default='sqlite://:memory:')
    ap.add_argument('--batches', type=int, default=24)
    ap.add_argument('--iterations', type=int, default=100)
    args = ap.parse_args()
    source = SOURCE if args.url.startswith('sqlite:') else SOURCE.replace('"sqlite"', '"postgresql"')
    reg = orm.Registry()
    models = orm.loads(source, registry=reg)
    Book, Author = models['Book'], models['Author']
    dbs = []
    schemas = []
    for label, path in [('before', args.before), ('after', args.after)]:
        native = load(path, label)
        schema = native.Schema(json.dumps(reg.ir()), {**reg._models, **reg._enums})
        schemas.append(schema)
        engine = await native.connect(args.url, schema, 4, [])
        dbs.append(orm.Database(engine, args.url))
    await dbs[0].create_tables()
    if args.url.startswith('sqlite:'):
        await dbs[1].create_tables()
    authors = []
    for db in (dbs if args.url.startswith('sqlite:') else dbs[:1]):
        a = await Author.objects.using(db).insert(email=f'bench-{time.time_ns()}@x', name='bench')
        await Book.objects.using(db).insert_many([
            {'author_id': a.id, 'title': f'b{i}', 'pages': i} for i in range(1000)
        ])
        authors.append(a.id)
    if len(authors) == 1:
        authors *= 2

    def query(db, aid, case):
        q = Book.objects.using(db).filter(Book.author_id == aid)
        if case.startswith('read-'):
            return q.order_by(Book.pages).limit(int(case.split('-')[1]))
        if case.startswith('nested-'):
            c = q.filter(Book.pages < 8).cte('base')
            for i in range(int(case.split('-')[1])):
                c = Book.objects.using(db).from_(c).filter(c.c.pages >= 0).cte(f'd{i}')
            return Book.objects.using(db).from_(c).order_by(Book.pages)
        if case == 'shared':
            c = q.filter(Book.pages < 8).cte('base')
            left = Book.objects.using(db).from_(c).filter(c.c.pages >= 0).cte('left_part')
            right = Book.objects.using(db).from_(c).filter(c.c.pages <= 7).cte('right_part')
            return Book.objects.using(db).from_(left).join(right, right.c.id == Book.id).order_by(Book.pages)
        if case == 'recursive':
            c = q.filter(Book.pages == 0).cte('chain', recursive=lambda c: q.filter(Book.pages == c.c.pages + 1, Book.pages < 8))
            return Book.objects.using(db).from_(c).order_by(Book.pages)
        if case.startswith('window-'):
            w = window(order_by=Book.pages, rows=(None, 0))
            return q.filter(Book.pages < 8).select(Book.pages, *[
                func.sum(Book.pages).over(w).label(f'w{i}')
                for i in range(int(case.split('-')[1]))
            ])
        if case == 'projection':
            return q.filter(Book.pages < 50).select(Book.title, Book.pages).order_by(Book.pages)
        raise ValueError(case)

    cases = ['read-1', 'read-50', 'read-1000', 'projection', 'nested-4', 'nested-16', 'shared', 'recursive', 'window-3', 'window-16', 'window-64']
    if args.url.startswith('sqlite:'):
        cases = [c for c in cases if not c.startswith('window-')]
    results = {}
    for case in cases:
        async def operation(side):
            return await query(dbs[side], authors[side], case)
        # Complete equality, including all model fields, with differing seed identities normalized.
        def normalized(rows):
            return [tuple(r) if isinstance(r, tuple) else (r.title, r.pages, r.status, r.metadata) for r in rows]
        assert normalized(await operation(0)) == normalized(await operation(1)), case
        expected = int(case.split('-')[1]) if case.startswith('read-') else (50 if case == 'projection' else 8)
        assert len(await operation(0)) == expected, case
        for side in [0, 1]:
            for _ in range(30):
                await operation(side)
        samples = [[], []]
        n = max(10, args.iterations // 5) if case == 'read-1000' else args.iterations
        for batch in range(args.batches):
            for side in ([0, 1] if batch % 2 == 0 else [1, 0]):
                start = time.perf_counter_ns()
                for _ in range(n):
                    await operation(side)
                samples[side].append((time.perf_counter_ns() - start) / n / 1000)
        results[case] = {'baseline': samples[0], 'candidate': samples[1]}
        print(case, flush=True)
    Path(args.out).write_text(json.dumps({'runtime': sys.version, 'platform': platform.platform(), 'url': args.url, 'batches': args.batches, 'iterations': args.iterations, 'cases': results}, indent=2))
    for db, aid in zip(dbs, authors):
        if args.url.startswith('sqlite:') or db is dbs[0]:
            await Author.objects.using(db).filter(Author.id == aid).delete()
        await db.close()


asyncio.run(main())
