"""Native bootstrap uncertainty; allocation runs never enter timing samples."""
import argparse
import json
import random
import statistics
from pathlib import Path

ap=argparse.ArgumentParser()
ap.add_argument('input',type=Path)
ap.add_argument('--check',action='store_true')
args=ap.parse_args()
data=json.loads(args.input.read_text())
rng=random.Random(84130)
lines=['# Focused native comparisons','','| Backend / workload | Control ns | Candidate ns | Paired median change | Paired 95% interval | Allocations / bytes |','|---|---:|---:|---:|---:|---|']
accepted=[]; regressions=[]; inconclusive=[]
for backend,run in data.items():
    for name,case in run['cases'].items():
        b=case['baseline']; a=case['candidate']
        pairs=[100*(y/x-1) for x,y in zip(b,a,strict=True)]
        estimates=sorted(statistics.median(rng.choices(pairs,k=len(pairs))) for _ in range(5000))
        lo,hi=estimates[125],estimates[4874]
        label=f'{backend}/{name}'
        if hi<1: accepted.append(label)
        elif lo>=1: regressions.append(label)
        else: inconclusive.append(label)
        counts=run['allocations']
        before=counts['builtin'][name];after=counts['extension'][name]
        if before!=after: raise SystemExit(f'allocation mismatch {label}: {before} vs {after}')
        lines.append(f'| {label} | {statistics.median(b):.2f} | {statistics.median(a):.2f} | {statistics.median(pairs):+.2f}% | [{lo:+.2f}%, {hi:+.2f}%] | {before["allocations"]:g} / {before["bytes"]:g} |')
lines+=['',f'Upper bound below 1%: {len(accepted)}/{len(accepted)+len(regressions)+len(inconclusive)}. Established regressions: {len(regressions)}. Inconclusive: {len(inconclusive)}.', '','Each pair uses independent uninstrumented release processes in alternating order. Allocation counts use separately built instrumented binaries. Intervals are exploratory, without multiple-comparison correction. Positive values indicate slower execution.']
args.input.with_suffix('.md').write_text('\n'.join(lines)+'\n')
args.input.with_name(args.input.stem+'-gate.json').write_text(json.dumps({'passed':not regressions and not inconclusive,'accepted':accepted,'regressions':regressions,'inconclusive':inconclusive},indent=2))
print('\n'.join(lines))
if args.check and (regressions or inconclusive): raise SystemExit(1)
