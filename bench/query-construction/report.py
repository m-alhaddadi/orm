"""Report exploratory paired intervals, with one independent process per repetition."""
import argparse
import json
import random
import statistics
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument('--directory', type=Path, default=Path('bench/query-construction/results'))
ap.add_argument('--out', type=Path, default=Path('bench/query-construction/results/summary.md'))
args = ap.parse_args()
rng = random.Random(84130)
lines = ['The direct operation decoder was rejected for an error compatibility change, regardless of timing.', '',
         '| Runtime / backend / workload | Before µs | After µs | Change | Paired 95% interval | Three run changes |',
         '|---|---:|---:|---:|---:|---|']
regressions = []
gains = set()
for runtime in ['python', 'node']:
    for backend in ['postgres', 'sqlite']:
        runs = [json.loads((args.directory / f'{runtime}-{backend}-{i}.json').read_text()) for i in range(1, 4)]
        assert all(r['cases'].keys() == runs[0]['cases'].keys() for r in runs)
        for name in runs[0]['cases']:
            cases = [r['cases'][name] for r in runs]
            before = [statistics.median(c['baseline']) for c in cases]
            after = [statistics.median(c['candidate']) for c in cases]
            pairs = [[100 * (a / b - 1) for b, a in zip(c['baseline'], c['candidate'], strict=True)] for c in cases]
            estimates = sorted(statistics.median(statistics.median(rng.choices(p, k=len(p))) for p in pairs) for _ in range(3000))
            lo, hi = estimates[75], estimates[2924]
            b, a = statistics.median(before), statistics.median(after)
            changes = ', '.join(f'{100*(a/b-1):+.1f}%' for b, a in zip(before, after, strict=True))
            label = f'{runtime}/{backend}/{name}'
            if lo > 0:
                regressions.append(label)
            if hi < 0:
                gains.add(runtime)
            lines.append(f'| {label} | {b:.2f} | {a:.2f} | {100*(a/b-1):+.1f}% | [{lo:+.1f}%, {hi:+.1f}%] | {changes} |')
lines += ['', f'Detected timing regressions in this matrix: {regressions}.',
          f'Runtimes with at least one detected timing gain: {sorted(gains)}.',
          'These exploratory intervals are not corrected for multiple comparisons; they do not establish production equivalence.', '']
args.out.write_text('\n'.join(lines))
print('\n'.join(lines))
