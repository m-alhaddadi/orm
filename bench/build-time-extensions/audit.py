"""Retain dependency selections, generated execution and disabled layout evidence."""
import argparse
import hashlib
import json
import subprocess
from pathlib import Path
ap=argparse.ArgumentParser()
ap.add_argument('--disabled',type=Path,required=True)
ap.add_argument('--controls',type=Path,required=True)
ap.add_argument('--out',type=Path,required=True)
args=ap.parse_args()
args.out.mkdir(parents=True,exist_ok=True)
records={}
for label,workspace in [('disabled',args.disabled/'after'),('enabled',args.controls/'sqlite/extension'),('builtin',args.controls/'sqlite/builtin')]:
    tree=subprocess.check_output(['cargo','tree','--offline','--locked','--manifest-path',str(workspace/'Cargo.toml'),'-p','orm-python','-e','normal','--prefix','none'],text=True)
    (args.out/f'{label}-dependencies.txt').write_text(tree)
    if label=='disabled':
        assert 'native-rules' not in tree and 'example-' not in tree and 'orm-extension-build' not in tree
    else: assert 'orm-app-native-rules' in tree
    record={'dependency_sha256':hashlib.sha256(tree.encode()).hexdigest()}
    if label!='disabled':
        source=(workspace/'engine-composition.rs').read_text()
        (args.out/f'{label}-execution.rs').write_text(source)
        assert 'native_rules::' in source
        assert 'serde_json' not in source and 'Box<dyn Fn' not in source
        record['artifact']=json.loads((workspace/'artifact.json').read_text())
        record['execution_sha256']=hashlib.sha256(source.encode()).hexdigest()
    records[label]=record
for path in ['core/src/schema.rs','engine/src/plan.rs','engine/src/exec.rs','engine/src/lib.rs']:
    source=(args.disabled/'after'/path).read_text()
    records.setdefault('conditional_sources',{})[path]={'sha256':hashlib.sha256(source.encode()).hexdigest(),'composition_cfg_sites':source.count('#[cfg(feature = "composition")]')}
records['scope']='Dependency trees prove selected runtime dependencies. Source cfg sites and generated direct calls are structural evidence; timing and separate allocation probes provide execution evidence. No claim of identical assembly is made.'
(args.out/'audit.json').write_text(json.dumps(records,indent=2))
