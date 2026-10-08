# Enabled-extension paired gate, 2026-10-08 (quiet-machine rerun of Q8)

Source: `main` at `10663b5920cb3dbad59dfc686b7bb63c635da859`.
Machine: Apple M5, macOS, `.venv` Python 3.14.7 (Homebrew), Node 26.9.0.
PostgreSQL: Docker `orm-review-pg`, port 55432, database `orm_j4`.

**Gate: not passed** (measured): 0 warm regressions, 20 of 44 warm cases accepted (upper bound below 1%), 24 inconclusive.
Jury-4 (`results/jury-4/gate/`, load 11 to 23) had 0, 4 and 40.

## Commands

Repository root as working directory, `CARGO_INCREMENTAL=0`:

```bash
.venv/bin/python bench/build-time-extensions/controls.py --out /tmp/orm-gate-2026-10-08
.venv/bin/python bench/build-time-extensions/run.py --native-profile \
  --before /tmp/orm-gate-2026-10-08 --after /tmp/orm-gate-2026-10-08 \
  --url postgres://postgres:postgres@localhost:55432/orm_j4 \
  --out bench/build-time-extensions/results/gate-2026-10-08
.venv/bin/python bench/build-time-extensions/report.py --native-profile --check \
  --directory bench/build-time-extensions/results/gate-2026-10-08
```

`run.py` defaults: 3 processes per runtime and backend, 24 batches, 100 iterations.
`report.py --check` exited 1 (gate not passed). `controls-build.json` is the `build.json` of `controls.py`.
No script was changed.

## Load (measured, `uptime`, 1/5/15-minute averages)

| Point | Time | Load |
|---|---|---|
| Before `controls.py` | 09:44 | 1.91 1.94 2.50 |
| Before `run.py` | 10:03 | 3.00 3.25 3.03 |
| Before `report.py` | 10:06 | 3.19 3.17 3.03 |
| End | 10:06 | 3.19 3.17 3.03 |

A 60-second sampler recorded a 1-minute load from 1.91 to 4.32 (measured); it was above 3 at 09:47, 09:50, 09:54 to 10:00, 10:02 and 10:06.
During `run.py` (10:03 to 10:06) it was 3.00, 2.57, 2.83 and 4.15.
The main source outside the benchmark was the Docker Desktop VM (about 60% CPU, measured with `ps` at 10:06).
Docker Desktop was not running at 09:40; it was started for PostgreSQL, and it also auto-started the unrelated containers `nameless-db` and `nameless-clickhouse` (16% CPU, 1.8 GiB at 09:43, measured). They were not stopped.

## Why 24 cases are inconclusive

- Most inconclusive cases have a point change near 0 (−2.1% to +1.4%), but their 95% interval is 1.4 to 4.7 points wide (measured; `construct` wider), so the upper bound crosses 1%. At this load, 3 processes of 24 batches do not resolve a 1% limit.
- `node/*/construct` (1.6 µs) has the widest intervals ([−2.3%, +8.7%], [−1.6%, +9.0%]) while all three process changes are +0.9% to +2.8%; `perf-investigation.md` documents a load-order bias of this harness on short CPU cases.
- Some cases have a consistent positive change: `python/sqlite/read-1` +1.7% [+0.3%, +1.7%] (runs +1.7, +1.8, +0.2), `node/postgres/bulk-update-50` +2.7% [−0.1%, +3.0%], `node/sqlite/projection` +3.1% [−0.6%, +2.3%], `node/postgres/construct+sql` +1.4% [−0.5%, +2.7%]. None has a lower bound at or above 1%, so none is an established regression.
- The machine was not quiet (load 2.6 to 4.3 during timing). A rerun with Docker Desktop stopped (SQLite only), or with PostgreSQL outside the Docker VM, removes the largest known noise source.

## Per-case verdicts

All values are measured. Verdict: accepted = upper bound below 1%; inconclusive = interval crosses 1%; regression = lower bound at or above 1%. Definition is setup cost and gets no verdict.

| Runtime / backend / workload | Handwritten µs | Extension µs | Change | Paired 95% interval | Run changes | Verdict |
|---|---:|---:|---:|---:|---|---|
| python/postgres/definition | 365.21 | 366.19 | +0.3% | [-1.3%, +1.1%] | -0.3%, +0.3%, -0.3% | setup (no verdict) |
| python/postgres/construct | 3.87 | 3.87 | +0.1% | [+0.0%, +0.8%] | +0.1%, +0.6%, +0.8% | accepted |
| python/postgres/plan-native | 2.05 | 2.04 | -0.7% | [-1.9%, -0.5%] | -0.7%, -1.5%, -1.3% | accepted |
| python/postgres/construct+sql | 16.17 | 16.18 | +0.1% | [-0.3%, +0.6%] | +0.9%, -0.4%, +0.1% | accepted |
| python/postgres/read-1 | 853.26 | 832.40 | -2.4% | [-2.8%, -0.8%] | -0.9%, -3.1%, -2.4% | accepted |
| python/postgres/read-50 | 884.73 | 836.05 | -5.5% | [-3.6%, -1.5%] | +1.2%, -5.5%, -2.7% | accepted |
| python/postgres/read-1000 | 2820.80 | 2762.66 | -2.1% | [-2.8%, +1.9%] | -2.1%, +5.9%, -2.0% | inconclusive |
| python/postgres/projection | 741.51 | 745.20 | +0.5% | [-0.8%, +0.5%] | -0.3%, +0.5%, -1.1% | accepted |
| python/postgres/update | 462.43 | 460.31 | -0.5% | [-1.1%, +0.6%] | -0.4%, -0.5%, -0.5% | accepted |
| python/postgres/bulk-update-50 | 730.55 | 727.03 | -0.5% | [-0.8%, +1.6%] | +0.3%, +0.1%, -0.5% | inconclusive |
| python/postgres/insert | 447.19 | 444.84 | -0.5% | [-0.5%, +2.2%] | +1.6%, -3.8%, +1.6% | inconclusive |
| python/postgres/bulk-insert-50 | 858.45 | 866.96 | +1.0% | [-1.2%, +2.0%] | +1.1%, +0.1%, +0.4% | inconclusive |
| python/sqlite/definition | 364.96 | 363.85 | -0.3% | [-1.5%, +1.1%] | -0.3%, -0.3%, +0.1% | setup (no verdict) |
| python/sqlite/construct | 3.87 | 3.89 | +0.6% | [-0.1%, +0.8%] | +0.0%, +0.7%, +0.2% | accepted |
| python/sqlite/plan-native | 2.00 | 2.00 | -0.4% | [-2.3%, -0.2%] | -1.1%, +3.1%, -3.6% | accepted |
| python/sqlite/construct+sql | 16.21 | 16.19 | -0.1% | [-0.4%, +0.3%] | -0.1%, +1.5%, -0.1% | accepted |
| python/sqlite/read-1 | 221.41 | 225.19 | +1.7% | [+0.3%, +1.7%] | +1.7%, +1.8%, +0.2% | inconclusive |
| python/sqlite/read-50 | 266.07 | 266.82 | +0.3% | [+0.1%, +0.9%] | +0.6%, +0.4%, +0.3% | accepted |
| python/sqlite/read-1000 | 1354.56 | 1358.10 | +0.3% | [-0.2%, +0.7%] | -0.1%, +1.3%, +0.1% | accepted |
| python/sqlite/projection | 250.76 | 253.44 | +1.1% | [-0.3%, +1.2%] | +0.3%, +1.2%, +0.1% | inconclusive |
| python/sqlite/update | 91.75 | 92.22 | +0.5% | [-0.2%, +1.3%] | -0.4%, +0.6%, -0.3% | inconclusive |
| python/sqlite/bulk-update-50 | 243.86 | 243.83 | -0.0% | [-0.3%, +0.8%] | +0.1%, -0.3%, -0.0% | accepted |
| python/sqlite/insert | 84.67 | 84.52 | -0.2% | [-0.4%, +1.2%] | +0.9%, +0.4%, -0.2% | inconclusive |
| python/sqlite/bulk-insert-50 | 241.49 | 241.91 | +0.2% | [-1.3%, +1.2%] | -0.4%, -1.2%, +0.6% | inconclusive |
| node/postgres/definition | 228.50 | 228.18 | -0.1% | [-1.4%, -0.1%] | -0.4%, -0.7%, -0.1% | setup (no verdict) |
| node/postgres/construct | 1.60 | 1.62 | +1.3% | [-2.3%, +8.7%] | +1.3%, +1.3%, +2.6% | inconclusive |
| node/postgres/plan-native | 2.16 | 2.16 | +0.1% | [-0.5%, +0.7%] | +0.1%, +0.2%, +1.0% | accepted |
| node/postgres/construct+sql | 9.22 | 9.34 | +1.4% | [-0.5%, +2.7%] | +1.4%, +1.2%, +1.8% | inconclusive |
| node/postgres/read-1 | 681.38 | 640.75 | -6.0% | [-4.8%, -2.3%] | -5.7%, -6.2%, -3.9% | accepted |
| node/postgres/read-50 | 737.45 | 709.61 | -3.8% | [-5.1%, -2.5%] | -3.0%, -7.5%, -4.1% | accepted |
| node/postgres/read-1000 | 2233.40 | 2260.71 | +1.2% | [-1.9%, +2.3%] | +4.6%, -2.4%, +2.4% | inconclusive |
| node/postgres/projection | 640.67 | 648.11 | +1.2% | [-0.1%, +1.5%] | +1.4%, +0.9%, +1.2% | inconclusive |
| node/postgres/update | 375.70 | 377.21 | +0.4% | [-0.7%, +0.8%] | -1.4%, +0.4%, +0.2% | accepted |
| node/postgres/bulk-update-50 | 621.08 | 637.79 | +2.7% | [-0.1%, +3.0%] | +3.3%, +1.2%, -0.7% | inconclusive |
| node/postgres/insert | 381.03 | 382.47 | +0.4% | [-0.5%, +1.5%] | +0.8%, +0.4%, -0.2% | inconclusive |
| node/postgres/bulk-insert-50 | 817.68 | 808.57 | -1.1% | [-1.5%, +0.5%] | -1.0%, -0.9%, -1.1% | accepted |
| node/sqlite/definition | 225.30 | 226.00 | +0.3% | [-0.6%, +1.0%] | +0.5%, +0.3%, -0.7% | setup (no verdict) |
| node/sqlite/construct | 1.58 | 1.59 | +1.1% | [-1.6%, +9.0%] | +1.1%, +0.9%, +2.8% | inconclusive |
| node/sqlite/plan-native | 2.18 | 2.14 | -1.9% | [-2.3%, -1.0%] | -1.0%, -2.4%, -0.7% | accepted |
| node/sqlite/construct+sql | 9.37 | 9.33 | -0.4% | [-1.8%, +1.0%] | +0.3%, -0.1%, -0.4% | inconclusive |
| node/sqlite/read-1 | 134.61 | 132.84 | -1.3% | [-2.9%, +1.1%] | -3.1%, -1.3%, +0.5% | inconclusive |
| node/sqlite/read-50 | 192.53 | 193.00 | +0.2% | [-1.2%, +1.6%] | -1.6%, +1.2%, +0.2% | inconclusive |
| node/sqlite/read-1000 | 1401.61 | 1397.98 | -0.3% | [-1.4%, +1.3%] | -1.4%, -0.3%, +0.1% | inconclusive |
| node/sqlite/projection | 158.43 | 163.35 | +3.1% | [-0.6%, +2.3%] | -1.4%, +3.1%, +0.9% | inconclusive |
| node/sqlite/update | 47.75 | 47.80 | +0.1% | [-1.1%, +1.8%] | +1.0%, -1.9%, -0.4% | inconclusive |
| node/sqlite/bulk-update-50 | 197.78 | 198.53 | +0.4% | [-1.1%, +0.5%] | +1.1%, -0.2%, -0.9% | accepted |
| node/sqlite/insert | 49.93 | 50.43 | +1.0% | [-0.7%, +1.9%] | +0.7%, +1.0%, -1.6% | inconclusive |
| node/sqlite/bulk-insert-50 | 237.74 | 239.93 | +0.9% | [-0.3%, +1.2%] | +0.0%, +0.9%, +0.9% | inconclusive |

Positive changes mean slower execution. Definition is setup cost and includes preparation to the same ready-to-query state in both versions.

Warm regressions above accepted cost (lower bound ≥ 1%): 0.

Warm cases with upper bound below 1%: 20/44.
Inconclusive against the 1% limit: 24.
The user accepts costs strictly below 1%. The gate requires an upper confidence bound below the limit. Intervals are exploratory and have no correction for multiple comparisons.
