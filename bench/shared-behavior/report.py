"""Summarize run medians and paired batch variability (stdlib only)."""
import argparse
import json
import random
import statistics
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument('--directory', type=Path, default=Path(__file__).with_name('results'))
ap.add_argument('--out', type=Path)
ap.add_argument('--check', action='store_true', help='fail on a detectable regression or absent gain in either language')
args = ap.parse_args()
rng = random.Random(74913)
lines = ['| Runtime / workload | Before µs | After µs | Latency change | Paired 95% interval | Run changes |',
         '|---|---:|---:|---:|---:|---|']
regressions = []
gains = set()
for language in ('python', 'node', 'python-pool4', 'node-pool4'):
    runs = [json.loads(p.read_text()) for p in sorted(args.directory.glob(f'{language}-[123].json'))]
    if len(runs) != 3:
        raise SystemExit(f'expected three runs for {language}')
    for name in runs[0]['cases']:
        cases = [r['cases'][name] for r in runs]
        before = [statistics.median(c['baseline']) for c in cases]
        after = [statistics.median(c['candidate']) for c in cases]
        pairs = [[(a/b-1)*100 for b,a in zip(c['baseline'],c['candidate'],strict=True)] for c in cases]
        estimates = sorted(statistics.median(statistics.median(rng.choices(p,k=len(p))) for p in pairs) for _ in range(3000))
        lo, hi = estimates[75], estimates[2924]
        if lo > 0:
            regressions.append(f'{language}: {name} [{lo:.2f}%, {hi:.2f}%]')
        if hi < 0:
            gains.add(language.split('-')[0])
        b, a = statistics.median(before), statistics.median(after)
        changes = ', '.join(f'{(a/b-1)*100:+.1f}%' for b,a in zip(before,after,strict=True))
        lines.append(f'| {language}: {name.lower()} | {b:.2f} | {a:.2f} | {(a/b-1)*100:+.1f}% | [{lo:+.1f}%, {hi:+.1f}%] | {changes} |')
text = '\n'.join(lines)+'\n'
if args.out:
    args.out.write_text(text)
else:
    print(text)

if args.check:
    if regressions or gains != {'python', 'node'}:
        raise SystemExit('benchmark gate failed: ' + repr(regressions) + '; gains=' + repr(gains))
    print('Benchmark gate passed: gain in both languages; no detected regression.')
