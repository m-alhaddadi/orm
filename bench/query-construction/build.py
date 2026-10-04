"""Build the starting checkout and rejected prototype, then restore production files."""
import argparse
import os
import shutil
import subprocess
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument('--out', type=Path, default=Path('/tmp/orm-query-binaries'))
args = ap.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
env = dict(os.environ)
env.setdefault('UV_CACHE_DIR', '/tmp/orm-uv-cache')
patch = 'bench/query-construction/direct-operation.patch'
subprocess.run(['git', 'apply', '--check', patch], check=True)


def build():
    subprocess.run(['.venv/bin/maturin', 'develop', '--release'], env=env, check=True)
    subprocess.run(['node', 'js/scripts/build-native.mjs', '--release'], check=True)


build()
native_path = Path(subprocess.check_output(['.venv/bin/python', '-c', 'import orm; print(orm._native.__file__)'], text=True).strip())
before_python = args.out / 'before.so'
before_node = args.out / 'before.node'
shutil.copy2(native_path, before_python)
shutil.copy2('js/orm.node', before_node)
subprocess.run(['git', 'apply', patch], check=True)
try:
    build()
    shutil.copy2(native_path, args.out / 'after.so')
    shutil.copy2('js/orm.node', args.out / 'after.node')
    result = subprocess.run(['cargo', 'test', '-q', '-p', 'orm-engine', 'differential_operation_decoding'], capture_output=True, text=True)
    (args.out / 'compatibility.txt').write_text(result.stdout + result.stderr)
    if result.returncode != 101 or 'number out of range' not in result.stdout:
        raise RuntimeError('expected the documented compatibility failure; inspect compatibility.txt')
finally:
    subprocess.run(['git', 'apply', '-R', patch], check=True)
    shutil.copy2(before_python, native_path)
    shutil.copy2(before_node, 'js/orm.node')

subprocess.run(['js/node_modules/.bin/tsc', '-p', 'bench/query-construction/tsconfig.json'], check=True)
dist = Path('bench/query-construction/dist')
shutil.copy2('bench/query-construction/package.json', dist / 'package.json')
link = dist / 'js/node_modules'
if not link.exists():
    link.symlink_to(Path('js/node_modules').resolve(), target_is_directory=True)
print(f'Built before/after extensions in {args.out}; production source and extensions restored.')
