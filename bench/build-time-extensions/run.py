"""Run builds before this script: measurements must not overlap compilation/tests."""
import argparse
import subprocess
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument('--before', required=True)
ap.add_argument('--after', required=True)
ap.add_argument('--url', default='postgres://postgres@127.0.0.1:55441/orm_benchmark')
ap.add_argument('--out', type=Path, default=Path('bench/build-time-extensions/results'))
ap.add_argument('--repetitions', type=int, default=3)
ap.add_argument('--batches', type=int, default=24)
ap.add_argument('--iterations', type=int, default=100)
ap.add_argument('--native-profile', action='store_true', help='before/after name the controls root; compare builtin vs extension per backend')
args = ap.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
for run in range(1, args.repetitions + 1):
    for backend, url in [('postgres', args.url), ('sqlite', 'sqlite://:memory:')]:
        for runtime in (['python', 'node'] if run % 2 else ['node', 'python']):
            prefix = ['.venv/bin/python', 'bench/build-time-extensions/python.py'] if runtime == 'python' else ['node', '--expose-gc', 'bench/build-time-extensions/node.mjs']
            before = str(Path(args.before) / backend / 'builtin') if args.native_profile else args.before
            after = str(Path(args.after) / backend / 'extension') if args.native_profile else args.after
            subprocess.run([*prefix, *(['--native-profile'] if args.native_profile else []), '--before', before, '--after', after, '--url', url,
                '--out', str(args.out / f'{runtime}-{backend}-{run}.json'), '--batches', str(args.batches), '--iterations', str(args.iterations)], check=True)
