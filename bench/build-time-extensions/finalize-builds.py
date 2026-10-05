"""Refresh copied host implementation after review fixes, preserving composition."""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

ap=argparse.ArgumentParser()
ap.add_argument('--disabled',type=Path,required=True)
ap.add_argument('--controls',type=Path,required=True)
ap.add_argument('--only',choices=['disabled','controls'])
args=ap.parse_args()
root=Path(__file__).resolve().parents[2]
suffix='dylib' if sys.platform=='darwin' else 'so'

def refresh_sources(workspace):
    changed=[]
    # Manifests and generated composition belong to the resolved artifact.
    for component in ['core','engine','cli','bindings/python','bindings/node']:
        for path in (root/component/'src').rglob('*'):
            if path.is_file():
                dest=workspace/path.relative_to(root)
                dest.parent.mkdir(parents=True,exist_ok=True)
                if not dest.exists() or dest.read_bytes() != path.read_bytes():
                    shutil.copy2(path,dest)
                    changed.append(str(path.relative_to(root)))
    return changed

def metadata(workspace,target,seconds):
    binaries={}
    for name,path,library in [('python',workspace/'python/orm/_native.so','lib_native'),('node',workspace/'js/orm.node','liborm_node')]:
        shutil.copy2(target/f'release/{library}.{suffix}',path)
        binaries[name]={'bytes':path.stat().st_size,'sha256':hashlib.sha256(path.read_bytes()).hexdigest()}
    return {'review_refresh_build_seconds':seconds,'binaries':binaries}

for group in ([args.only] if args.only else ['disabled','controls']):
    base=getattr(args,group).resolve()
    report=json.loads((base/'build.json').read_text())
    keys=['after'] if group=='disabled' else ['sqlite/extension','sqlite/builtin','postgres/extension','postgres/builtin']
    for key in keys:
        workspace=base/key
        changed = refresh_sources(workspace)
        target=root/('target' if group=='disabled' else 'target/extension-proof')
        env=dict(os.environ,CARGO_TARGET_DIR=str(target),PYO3_PYTHON=sys.executable)
        if group=='controls':
            extension=base/key.split('/')[0]/'extension'
            env.update(ORM_CORE_COMPOSITION=str(extension/'composition.rs'),ORM_ENGINE_COMPOSITION=str(workspace/'engine-composition.rs'),ORM_PYTHON_METHODS=str(extension/'python-methods.rs'),ORM_NODE_METHODS=str(extension/'node-methods.rs'))
        if changed:
            start=time.perf_counter()
            subprocess.run(['cargo','build','--release','--offline','--locked','--manifest-path',str(workspace/'Cargo.toml'),'-p','orm-python','-p','orm-node'],env=env,check=True)
            report['builds'][key].update(metadata(workspace,target,time.perf_counter()-start))
        report['builds'][key]['refreshed_sources'] = changed
        if group=='controls' and (changed or (root/'bench/build-time-extensions/native-enabled.rs').read_bytes() != (workspace/'engine/examples/extension_probe.rs').read_bytes()):
            shutil.copy2(root/'bench/build-time-extensions/native-enabled.rs',workspace/'engine/examples/extension_probe.rs')
            for instrumented in (False,True):
                subprocess.run(['cargo','build','--release','--offline','--locked','--manifest-path',str(workspace/'Cargo.toml'),'-p','orm-engine','--features','composition,allocation-probe' if instrumented else 'composition','--example','extension_probe'],env=env,check=True)
                shutil.copy2(target/'release/examples/extension_probe',workspace/('allocation-probe' if instrumented else 'native-probe'))
    if group=='disabled':
        for key in ['before','after']:
            workspace=base/key
            probe=workspace/'native-benchmark'
            shutil.copytree(root/'bench/build-time-extensions/native',probe)
            manifest=probe/'Cargo.toml'
            manifest.write_text(manifest.read_text().replace('../../../core','../core').replace('../../../engine','../engine'))
            env=dict(os.environ,CARGO_TARGET_DIR=str(root/'target'))
            for instrumented in (False,True):
                subprocess.run(['cargo','build','--release','--offline','--manifest-path',str(manifest),*(['--features','allocation-probe'] if instrumented else [])],env=env,check=True)
                shutil.copy2(root/'target/release/orm-extension-allocation-probe',workspace/('allocation-probe' if instrumented else 'native-probe'))
    report['host_sources_sha256']={str(p.relative_to(root)):hashlib.sha256(p.read_bytes()).hexdigest() for component in ['core','engine','bindings/python','bindings/node'] for p in (root/component/'src').rglob('*') if p.is_file()}
    (base/'build.json').write_text(json.dumps(report,indent=2))
