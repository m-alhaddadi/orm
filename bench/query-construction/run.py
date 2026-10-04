"""Run sequential paired matrices; never overlap timing with builds or tests."""
import argparse
import subprocess
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument('--url', default='postgres://postgres:postgres@127.0.0.1:55439/orm_test')
ap.add_argument('--before-python', default='/tmp/orm-query-before.so')
ap.add_argument('--after-python', default='/tmp/orm-query-after.so')
ap.add_argument('--before-node', default='/tmp/orm-query-before.node')
ap.add_argument('--after-node', default='/tmp/orm-query-after.node')
ap.add_argument('--out', type=Path, default=Path('bench/query-construction/results'))
args = ap.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
for run in range(1, 4):
    for backend, url in [('postgres', args.url), ('sqlite', 'sqlite://:memory:')]:
        for runtime in (['python', 'node'] if run % 2 else ['node', 'python']):
            prefix = ['.venv/bin/python', 'bench/query-construction/queries.py'] if runtime == 'python' else ['node', 'bench/query-construction/dist/bench/query-construction/queries.js']
            before = args.before_python if runtime == 'python' else args.before_node
            after = args.after_python if runtime == 'python' else args.after_node
            subprocess.run([*prefix, '--before', before, '--after', after, '--url', url, '--out', str(args.out / f'{runtime}-{backend}-{run}.json')], check=True)
