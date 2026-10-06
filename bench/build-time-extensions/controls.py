"""Build extension and independently handwritten execution controls, before timing."""
import argparse
import ast
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
import tomllib
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('--out', type=Path, required=True)
parser.add_argument('--resume',action='store_true',help='resume an interrupted build in the same output')
args = parser.parse_args()
root = Path(__file__).resolve().parents[2]
out = args.out.resolve()
out.mkdir(parents=True, exist_ok=True)
if any(out.iterdir()) and not args.resume:
    raise SystemExit('choose an empty controls output directory')
script = ast.parse((root / 'bench/build-time-extensions/python.py').read_text())
source = next(ast.literal_eval(n.value) for n in script.body if isinstance(n, ast.Assign) and any(isinstance(t, ast.Name) and t.id == 'SCHEMA' for t in n.targets))
fixture = root / 'extension-build/tests/fixtures/native'
extension_manifest = tomllib.loads((fixture / 'Cargo.toml').read_text())['package']['metadata']['orm-extension']
target = root / 'target/extension-proof'
env = dict(os.environ, CARGO_TARGET_DIR=str(target), PYO3_PYTHON=sys.executable)
metadata = {'control': 'independently handwritten native execution using the same Rust functions and host planning/result primitives', 'profile': {'release': True,'lto':'fat','codegen_units':1}, 'builds':{}}
for backend, provider in [('sqlite','sqlite'),('postgres','postgresql')]:
    if args.resume and all((out/backend/side/'allocation-probe').exists() for side in ('extension','builtin')):
        for side in ('extension','builtin'):
            workspace = out/backend/side
            metadata['builds'][f'{backend}/{side}'] = {'incremental_build_seconds':None,'binaries':{name:{'bytes':path.stat().st_size,'sha256':hashlib.sha256(path.read_bytes()).hexdigest()} for name,path in [('python',workspace/'python/orm/_native.so'),('node',workspace/'js/orm.node')]},'execution_sha256':hashlib.sha256((workspace/'engine-composition.rs').read_bytes()).hexdigest()}
        (out/'build.json').write_text(json.dumps(metadata,indent=2))
        continue
    schema = out / f'{backend}.prisma'
    schema.write_text(source.replace('BACKEND', provider))
    extension = out / backend / 'extension'
    config = out / f'{backend}.json'
    config.write_text(json.dumps({'host':str(root),'output':str(extension),'offline':True,
        'modules':[{'alias':'native_rules','source':str(fixture/'src/lib.rs'),'manifest':extension_manifest}],
        'specialization':{'schema':str(schema),'models':[{'model':'BenchItem',
            'fields':[{'field':'label','transforms':['example.trim'],'validators':['example.username']}],
            'records':[{'export':'example.record','dependencies':['label']}],
            'computed':[{'field':'display','dependency':'label','export':'example.display'}]}]}}))
    start = time.perf_counter()
    if not (args.resume and extension.exists()):
        subprocess.run(['cargo','run','--offline','--locked','-q','-p','orm-extension-build','--',str(config)],cwd=root,env=env,check=True)
        elapsed = time.perf_counter() - start
    else:
        elapsed = None
        # An interrupted frontend can leave its workspace before the native
        # libraries were packaged. Rebuild that profile rather than treating
        # directory existence as a completed build.
        if not (extension/'python/orm/_native.so').exists() or not (extension/'js/orm.node').exists():
            buildenv = dict(env, ORM_CORE_COMPOSITION=str(extension/'composition.rs'),
                ORM_ENGINE_COMPOSITION=str(extension/'engine-composition.rs'),
                ORM_PYTHON_METHODS=str(extension/'python-methods.rs'),
                ORM_NODE_METHODS=str(extension/'node-methods.rs'))
            subprocess.run(['cargo','build','--release','--offline','--locked','--manifest-path',str(extension/'Cargo.toml'),'-p','orm-python','-p','orm-node'],env=buildenv,check=True)
            elapsed = time.perf_counter() - start
    def package(workspace, elapsed):
        (workspace/'python').mkdir(exist_ok=True)
        shutil.copytree(root/'python/orm',workspace/'python/orm',ignore=shutil.ignore_patterns('*.so','__pycache__'),dirs_exist_ok=True)
        suffix = 'dylib' if sys.platform == 'darwin' else 'so'
        py = workspace/'python/orm/_native.so'
        if elapsed is not None:
            shutil.copy2(target/f'release/lib_native.{suffix}',py)
        shutil.copytree(root/'js/dist',workspace/'js/dist',dirs_exist_ok=True)
        (workspace/'js/package.json').write_bytes((root/'js/package.json').read_bytes())
        if not (workspace/'js/node_modules').exists():
            (workspace/'js/node_modules').symlink_to(root/'js/node_modules',target_is_directory=True)
        node = workspace/'js/orm.node'
        if elapsed is not None:
            shutil.copy2(target/f'release/liborm_node.{suffix}',node)
        return {'incremental_build_seconds':elapsed,'binaries':{name:{'bytes':p.stat().st_size,'sha256':hashlib.sha256(p.read_bytes()).hexdigest()} for name,p in [('python',py),('node',node)]}}
    metadata['builds'][f'{backend}/extension'] = package(extension,elapsed)
    builtin = out/backend/'builtin'
    shutil.copytree(extension,builtin,dirs_exist_ok=True,ignore=lambda directory,names: [name for name in names if name == 'node_modules' or (Path(directory) == extension and name in ('python','js'))])
    handwritten = (root/'bench/build-time-extensions/builtin.rs').read_bytes()
    if not (builtin/'engine-composition.rs').exists() or (builtin/'engine-composition.rs').read_bytes() != handwritten:
        (builtin/'engine-composition.rs').write_bytes(handwritten)
    buildenv = dict(env,ORM_CORE_COMPOSITION=str(extension/'composition.rs'),ORM_ENGINE_COMPOSITION=str(builtin/'engine-composition.rs'),ORM_PYTHON_METHODS=str(extension/'python-methods.rs'),ORM_NODE_METHODS=str(extension/'node-methods.rs'))
    start = time.perf_counter()
    subprocess.run(['cargo','build','--release','--offline','--locked','--manifest-path',str(builtin/'Cargo.toml'),'-p','orm-python','-p','orm-node'],env=buildenv,check=True)
    metadata['builds'][f'{backend}/builtin'] = package(builtin,time.perf_counter()-start)
    metadata['builds'][f'{backend}/extension']['generated_execution_sha256'] = hashlib.sha256((extension/'engine-composition.rs').read_bytes()).hexdigest()
    metadata['builds'][f'{backend}/builtin']['handwritten_execution_sha256'] = hashlib.sha256((builtin/'engine-composition.rs').read_bytes()).hexdigest()
    for workspace in (extension, builtin):
        engine = workspace / 'engine'
        (engine/'examples').mkdir(exist_ok=True)
        shutil.copy2(root/'bench/build-time-extensions/native-enabled.rs', engine/'examples/extension_probe.rs')
        shutil.copy2(root/'extension-build/tests/public/ownership-schema.json', engine/'examples/ownership-schema.json')
        manifest = engine/'Cargo.toml'
        text = manifest.read_text().replace('\nallocation-probe = []\n','\n')
        manifest.write_text(text.replace('[features]\n','[features]\nallocation-probe = []\n'))
        probeenv = dict(env,ORM_CORE_COMPOSITION=str(extension/'composition.rs'),ORM_ENGINE_COMPOSITION=str(workspace/'engine-composition.rs'))
        for instrumented in (False, True):
            features = 'composition,allocation-probe' if instrumented else 'composition'
            subprocess.run(['cargo','build','--release','--offline','--locked','--manifest-path',str(workspace/'Cargo.toml'),'-p','orm-engine','--features',features,'--example','extension_probe'],env=probeenv,check=True)
            shutil.copy2(target/'release/examples/extension_probe',workspace/('allocation-probe' if instrumented else 'native-probe'))
    (out/'build.json').write_text(json.dumps(metadata,indent=2))
print(f'Prepared enabled controls in {out}; timing has not run.')
