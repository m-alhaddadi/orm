"""Paired old/new *whole package* benchmark, including the language frontend."""
import argparse
import asyncio
import gc
import importlib.util
import json
import platform
import sys
import time
from pathlib import Path

SCHEMA = '''datasource db {
 provider = "BACKEND"
}
model BenchItem {
 id BigInt @id @default(autoincrement())
 label String
 number Int @default(0)
 data Json?
}'''


def package(root, name):
    path = Path(root) / 'python/orm'
    spec = importlib.util.spec_from_file_location(name, path / '__init__.py', submodule_search_locations=[str(path)])
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--before', required=True)
    ap.add_argument('--after', required=True)
    ap.add_argument('--url', required=True)
    ap.add_argument('--out', required=True)
    ap.add_argument('--batches', type=int, default=24)
    ap.add_argument('--iterations', type=int, default=100)
    ap.add_argument('--native-profile', action='store_true')
    args = ap.parse_args()
    separator = 'x' if args.native_profile else '-'
    def seed_label(side, index=''):
        return f'seed{separator}{side}{separator}{index}'
    def write_label(side):
        return f'write{separator}{side}'
    modules = [package(args.before, 'orm_before'), package(args.after, 'orm_after')]
    source = SCHEMA.replace('BACKEND', 'sqlite' if args.url.startswith('sqlite:') else 'postgresql')
    dbs, models, registries = [], [], []
    for module in modules:
        reg = module.Registry()
        model = module.loads(source, registry=reg)['BenchItem']
        registries.append(reg)
        models.append(model)
        db = await module.connect(args.url, registry=reg, default=False, max_connections=4)
        await db.create_tables()
        dbs.append(db)
    seeds = []
    for side in [0, 1]:
        rows = await models[side].objects.using(dbs[side]).insert_many([
            {'label': seed_label(side, i), 'number': i, 'data': {'n': i}} for i in range(1000)
        ]).returning()
        seeds.append(rows)

    def query(side, size):
        m = models[side]
        return m.objects.using(dbs[side]).filter(m.label.startswith(seed_label(side))).order_by(m.number).limit(size)

    native_schemas = [reg.native() for reg in registries]
    op = json.dumps({'op': 'select', 'model': 'BenchItem', 'filters': [], 'limit': 1})
    assert native_schemas[0].sql(op, []) == native_schemas[1].sql(op, [])
    results = {}
    sink = None
    cases = ['definition', 'construct', 'plan-native', 'construct+sql', 'read-1', 'read-50', 'read-1000', 'projection', 'update', 'bulk-update-50', 'insert', 'bulk-insert-50']
    for case in cases:
        sync = case in ['definition', 'construct', 'plan-native', 'construct+sql']
        def synchronous(side):
            if case == 'definition':
                mod = modules[side]
                reg = mod.Registry()
                mod.loads(source, registry=reg)
                return reg.native()  # equivalent readiness, includes old lazy preparation
            if case == 'construct':
                return query(side, 50)
            if case == 'plan-native':
                return native_schemas[side].sql(op, [])
            return query(side, 50).sql()

        async def asynchronous(side):
            m, db = models[side], dbs[side]
            if case.startswith('read-'):
                rows = await query(side, int(case.split('-')[1]))
                assert len(rows) == int(case.split('-')[1]) and rows[-1].number == len(rows) - 1
                assert rows[-1].data == {'n': len(rows) - 1}
                if args.native_profile:
                    assert rows[-1].display == f'Hello, {rows[-1].label}!'
                return rows
            if case == 'projection':
                rows = await query(side, 50).select(m.number, m.data)
                assert len(rows) == 50 and rows[-1] == (49, {'n': 49})
                return rows
            if case == 'update':
                n = await m.objects.using(db).filter(m.id == seeds[side][0].id).update(label=seed_label(side,0))
                assert n == 1
                return n
            if case == 'bulk-update-50':
                n = await m.objects.using(db).update_many([
                    {'id': r.id, 'label': seed_label(side, i)} for i, r in enumerate(seeds[side][:50])
                ])
                assert n == 50
                return n
            rows = await m.objects.using(db).insert_many([
                {'label': write_label(side), 'number': i, 'data': {'n': i}}
                for i in range(50 if case == 'bulk-insert-50' else 1)
            ]).returning()
            assert len(rows) == (50 if case == 'bulk-insert-50' else 1)
            assert rows[-1].data == {'n': len(rows) - 1}
            return rows

        async def cleanup(side):
            if case in ['insert', 'bulk-insert-50']:
                await models[side].objects.using(dbs[side]).filter(models[side].label == write_label(side)).delete()

        for side in [0, 1]:
            for _ in range(20000 if sync and case != "definition" else 30):
                sink = synchronous(side) if sync else await asynchronous(side)
            await cleanup(side)
        samples = [[], []]
        n = max(10, args.iterations // 5) if case == "definition" else args.iterations * 5 if sync else (max(10, args.iterations // 5) if case in ['read-1000', 'bulk-insert-50', 'bulk-update-50'] else args.iterations)
        for batch in range(args.batches):
            for side in ([0, 1] if batch % 2 == 0 else [1, 0]):
                gc.collect()
                start = time.perf_counter_ns()
                for _ in range(n):
                    sink = synchronous(side) if sync else await asynchronous(side)
                samples[side].append((time.perf_counter_ns() - start) / n / 1000)
                await cleanup(side)
        results[case] = {'baseline': samples[0], 'candidate': samples[1], 'iterations': n}
        print(case, flush=True)
    Path(args.out).write_text(json.dumps({'runtime': sys.version, 'platform': platform.platform(), 'url': args.url, 'batches': args.batches, 'warmup_cpu': 20000, 'results_retained': True, 'cases': results}, indent=2))
    assert sink is not None
    for side in [0, 1]:
        m, db = models[side], dbs[side]
        await m.objects.using(db).filter(m.label.startswith(seed_label(side))).delete()
        await db.close()


if __name__ == "__main__":
    asyncio.run(main())
