"""Paired uninstrumented native timings and separate allocation counts."""
import argparse
import json
import subprocess
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument('--controls', type=Path, required=True)
ap.add_argument('--out', type=Path, required=True)
ap.add_argument('--pairs', type=int, default=40)
ap.add_argument('--iterations', type=int, default=300000)
ap.add_argument('--disabled',action='store_true')
args = ap.parse_args()
results = {}
for backend, provider in [('sqlite', 'sqlite'), ('postgres', 'postgresql')]:
    roots = [args.controls/side for side in ('before','after')] if args.disabled else [args.controls/backend/side for side in ('builtin','extension')]
    samples = [{}, {}]
    for pair in range(args.pairs):
        for side in ([0,1] if pair%2 == 0 else [1,0]):
            run = json.loads(subprocess.check_output([str(roots[side]/'native-probe'),provider,str(args.iterations),'1'],text=True))
            for name, data in run.items():
                samples[side].setdefault(name,[]).append(data['ns'][0])
        print(f'{backend}: {pair+1}/{args.pairs}',flush=True)
    counts = [json.loads(subprocess.check_output([str(root/'allocation-probe'),provider,'10000'],text=True)) for root in roots]
    results[backend] = {'cases':{name:{'baseline':samples[0][name],'candidate':samples[1][name]} for name in samples[0]},'allocations':{'builtin':counts[0],'extension':counts[1]}}
args.out.parent.mkdir(parents=True,exist_ok=True)
args.out.write_text(json.dumps(results,indent=2))
