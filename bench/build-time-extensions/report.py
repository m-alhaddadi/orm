"""Report paired uncertainty; setup costs do not count as warm-query regressions."""
import argparse
import json
import random
import statistics
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument('--directory', type=Path, default=Path('bench/build-time-extensions/results'))
ap.add_argument('--out', type=Path)
ap.add_argument('--repetitions', type=int, default=3)
ap.add_argument('--check', action='store_true', help='require warm upper confidence bounds below accepted cost')
ap.add_argument('--max-cost-percent', type=float, default=1.0, help='exclusive accepted warm cost (default: 1%%)')
args = ap.parse_args()
rng = random.Random(84130)
lines = ['# Build-time extension refactor: old versus current', '',
    '| Runtime / backend / workload | Old µs | Current µs | Change | Paired 95% interval | Run changes |',
    '|---|---:|---:|---:|---:|---|']
regressions, inconclusive, within_margin = [], [], []
margin = args.max_cost_percent
for runtime in ['python', 'node']:
    for backend in ['postgres', 'sqlite']:
        runs = [json.loads((args.directory / f'{runtime}-{backend}-{i}.json').read_text()) for i in range(1, args.repetitions + 1)]
        assert all(r['cases'].keys() == runs[0]['cases'].keys() for r in runs)
        for name in runs[0]['cases']:
            cases = [r['cases'][name] for r in runs]
            before = [statistics.median(c['baseline']) for c in cases]
            after = [statistics.median(c['candidate']) for c in cases]
            pairs = [[100 * (a / b - 1) for b, a in zip(c['baseline'], c['candidate'], strict=True)] for c in cases]
            estimates = sorted(statistics.median(statistics.median(rng.choices(p, k=len(p))) for p in pairs) for _ in range(5000))
            lo, hi = estimates[125], estimates[4874]
            b, a = statistics.median(before), statistics.median(after)
            changes = ', '.join(f'{100*(x/y-1):+.1f}%' for y, x in zip(before, after, strict=True))
            label = f'{runtime}/{backend}/{name}'
            if name != 'definition':
                if lo >= margin: regressions.append({'case': label, 'interval': [lo, hi]})
                if lo < margin <= hi: inconclusive.append(label)
                if hi < margin: within_margin.append(label)
            lines.append(f'| {label} | {b:.2f} | {a:.2f} | {100*(a/b-1):+.1f}% | [{lo:+.1f}%, {hi:+.1f}%] | {changes} |')
lines += ['', 'Positive changes mean slower execution. Definition is setup cost and includes preparation to the same ready-to-query state in both versions.', '',
    f'Warm regressions above accepted cost (lower bound ≥ {margin:g}%): {len(regressions)}.',
    *[f"- {r['case']}: [{r['interval'][0]:+.1f}%, {r['interval'][1]:+.1f}%]" for r in regressions], '',
    f'Warm cases with upper bound below {margin:g}%: {len(within_margin)}/44.',
    f'Inconclusive against the {margin:g}% limit: {len(inconclusive)}.',
    'The user accepts costs strictly below 1%. The gate requires an upper confidence bound below the limit. Intervals are exploratory and have no correction for multiple comparisons.', '']
output = args.out or args.directory / 'summary.md'
output.write_text('\n'.join(lines))
(args.directory / 'gate.json').write_text(json.dumps({'passed': not regressions and not inconclusive, 'max_cost_percent_exclusive': margin, 'regressions': regressions, 'inconclusive': inconclusive, 'accepted': within_margin}, indent=2))
print('\n'.join(lines))

if args.check and (regressions or inconclusive):
    raise SystemExit(1)
