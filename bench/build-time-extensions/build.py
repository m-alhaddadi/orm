"""Build isolated old/new snapshots without changing installed packages or source."""
import argparse
import hashlib
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import time
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument('--before-ref', default='3a710cab6169752b07834021cacc384aa803a368')
ap.add_argument('--out', type=Path, required=True)
args = ap.parse_args()
root = Path(__file__).resolve().parents[2]
output = args.out.resolve()
output.mkdir(parents=True, exist_ok=True)
if any(output.iterdir()):
    raise SystemExit('output directory must be empty; choose a new directory')
reference = subprocess.check_output(['git', 'rev-parse', args.before_ref], cwd=root, text=True).strip()
archive = subprocess.check_output(['git', 'archive', reference], cwd=root)
for side in ['before', 'after']:
    dest = output / side
    dest.mkdir()
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        tar.extractall(dest, filter='data')
# Overlay the actual working tree, including new source files; preserve ignored inputs.
paths = subprocess.check_output(['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard'], cwd=root).split(b'\0')
for raw in paths:
    if not raw:
        continue
    path = Path(os.fsdecode(raw))
    src, dest = root / path, output / 'after' / path
    if src.is_file():
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dest)
    elif dest.exists():
        dest.unlink()
metadata = {'baseline_commit': reference, 'profile': {'release': True, 'lto': 'fat', 'codegen_units': 1}, 'builds': {}}
for side in ['before', 'after']:
    dest = output / side
    (dest / 'js/node_modules').symlink_to(root / 'js/node_modules', target_is_directory=True)
    env = dict(os.environ, CARGO_TARGET_DIR=str(root / 'target'), PYO3_PYTHON=sys.executable)
    start = time.perf_counter()
    subprocess.run(['cargo', 'build', '--offline', '--locked', '--release', '--manifest-path', str(dest / 'Cargo.toml'), '-p', 'orm-python', '-p', 'orm-node'], env=env, check=True)
    elapsed = time.perf_counter() - start
    suffix = '.dylib' if os.uname().sysname == 'Darwin' else '.so'
    py = dest / 'python/orm/_native.so'
    node = dest / 'js/orm.node'
    shutil.copy2(root / f'target/release/lib_native{suffix}', py)
    shutil.copy2(root / f'target/release/liborm_node{suffix}', node)
    subprocess.run([str(root / 'js/node_modules/.bin/tsc'), '-p', str(dest / 'js/tsconfig.json')], check=True)
    metadata['builds'][side] = {'incremental_build_seconds': elapsed, 'binaries': {}}
    for runtime, path in [('python', py), ('node', node)]:
        metadata['builds'][side]['binaries'][runtime] = {'bytes': path.stat().st_size, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
(output / 'build.json').write_text(json.dumps(metadata, indent=2))
print(f'Prepared {output}/before and {output}/after; no benchmark has run yet.')
