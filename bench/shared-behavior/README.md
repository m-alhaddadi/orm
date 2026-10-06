# Shared advisory locking: public API benchmark

The retained change moves advisory-key hashing, lock SQL construction, and boolean
result handling into Rust. Python and TypeScript retain their public signatures,
validation, error messages and precedence, string encoding, transaction context,
and integer-range rules. BLAKE2b uses an **eight-byte digest**, interpreted as a
signed big-endian integer. No key cache is
introduced. Migration locking, query planning, and write paths are unchanged.

The PostgreSQL transaction executor consumes the existing simple-query response
and returns one boolean. It avoids the nested vectors, owned text cells, and
Python/JS result rows previously built by `fetch_text`/`fetchText`. Both bindings
use the same Rust key and SQL helpers. Language-specific argument conversion stays
in each binding.

## Method

- Three independent processes per runtime/configuration; 24 paired batches per
  case, with AB/BA order alternating between batches. Both implementations receive
  30 warm-up calls per case. Every batch contains 200 operations, except 4 KiB
  names, which use 40 to bound runtime.
- `baseline.py` and `baseline.mjs` retain the original public lock implementations
  from the starting checkout. The TypeScript control uses the original unchanged
  `js/src/blake2b.ts`. Both contenders run in the **same release binary**, with the
  original text-query path intact. The only switch is `Database.lock`; no mock
  database, disabled conversion, cached hash, or CPU-only timer is used.
- Timers cover the whole awaited public operation: validation, UTF-8 encoding,
  buffer allocation/copying, Rust hashing or the original language hash, decimal
  key formatting/parsing, SQL allocation, FFI entry, Tokio/event-loop scheduling,
  PostgreSQL execution, result allocation/conversion, and result checking.
- Lock-only cases reuse an open transaction and repeatedly acquire the same key.
  Separate cases acquire and release a lock in a complete transaction; contend
  against another connection; and run four concurrent complete transactions.
  `transaction+read+write` also constructs and awaits public ORM `get` and `update`
  calls, including their JSON IR serialization, native parameters and model
  materialization. Every write checks the affected count and uses the same value.
- Case names use `<key kind>/<nowait>/<exclusive>`. Integer keys are `42`
  (including JS `bigint`); short names are `import`; Unicode names are `ورود 🔒`;
  long names contain 4,096 ASCII bytes. Each flag combination is measured.
- Pools contain one connection for the main matrix. Supplemental runs use four
  pooled connections, including four concurrent transactions on separate keys.
  Separate pools model actual denied locks. No timings run alongside builds/tests.
- Tables are created if absent. Each process inserts and deletes only its own
  benchmark user; it does not drop tables. Use an isolated test database.
- The summary reports medians of the three run medians, their changes, and all
  three individual run changes. Paired intervals use 3,000 deterministic bootstrap
  resamples of paired batch percentage changes, stratified by run, taking the
  median of run medians. The paired interval estimates that paired statistic;
  it is distinct from the ratio of the separately aggregated before/after medians. They describe this experiment's variability, not a
  population-wide guarantee. A positive lower bound flags a detected regression;
  a negative upper bound establishes a detected gain. The check requires a gain
  in each language and no detected regression in the measured matrix.

## Reproduce (Nushell, from the repository root)

Use an isolated PostgreSQL server with `postgres:postgres`, the example schema's
extensions, and a writable `orm_test` database. Our existing isolated server ran
on port 55439. Substitute your URL when invoking the runner.

```nu
$env.UV_CACHE_DIR = '/tmp/orm-uv-cache'
^uv pip install --python .venv/bin/python -e .
with-env {VIRTUAL_ENV: ($env.PWD | path join .venv)} { cd packaging/python/tooling; ^../../../.venv/bin/maturin develop --release }
^node js/scripts/build-native.mjs --release
^npm --prefix js run build
^nu bench/shared-behavior/run.nu --url 'postgres://postgres:postgres@127.0.0.1:55439/orm_test' --out /tmp/orm-lock-results
```

Wait for each build to finish successfully before running the next command.
`run.nu` runs both language matrices and the four-connection cases, checks live
cross-language contention and rollback release, then produces a summary and
applies the benchmark gate. To summarize the retained measurements:

```nu
^.venv/bin/python bench/shared-behavior/report.py --check
```

Correctness checks (the native bindings above must already be built):

```nu
$env.ORM_TEST_DATABASE_URL = 'postgres://postgres:postgres@127.0.0.1:55439/orm_test'
^.venv/bin/python -m pytest -q
^cargo test -q
^cargo clippy -q -p orm-core -p orm-engine -p orm-python -p orm-node
^.venv/bin/mypy --strict python/orm
^.venv/bin/pyright python/orm
^npm --prefix js test
^npm --prefix js run test:bun
^npm --prefix js run typecheck
^.venv/bin/python bench/shared-behavior/compat.py
```

## Candidates left out

**Write validation and row alignment: no qualifying end-to-end evidence.** Both
frontends already normalize rows once, validate, select schema-order columns, and
align values before a native insert/update call. Python allows arbitrary mapping
iterables, callable defaults and attribute-based relation values; TypeScript
skips `undefined`, maps runtime names to IR names, and compares numeric/bigint
primary keys under its own rules. Errors also differ deliberately: Python missing
insert fields raise `ValueError`, TypeScript raises `TypeError`. Preparation errors
are synchronous at statement construction, while native scalar conversion occurs
when the statement runs. Simply fusing these into Rust can change error timing;
returning native-aligned rows to the language creates another conversion and
boundary crossing. A deferred native representation needs its own compatibility
and performance experiment. This migration was not implemented or benchmarked,
and is not claimed to be slower. Duplication alone is insufficient evidence.

**Streaming reads and shared materialization: deferred.** The existing
`docs/performance.md` investigation demonstrates a Rust-struct streaming gain,
but explicitly excludes Python/Node object construction and runtime handoff.
Python already constructs model instances directly in its native binding; Node
returns flat native cells and builds language objects in TypeScript. Consolidating
these requires accounting for GIL/N-API constraints, joins, enum/array/decimal
conversion, projections and prefetch grouping. The existing isolated result does
not satisfy the requested public-API acceptance rule.

**General plan caching: deferred.** Prepared queries are already available, and
schema-directed planning, SQL construction and parameter limits live in Rust.
No new paired Python/TypeScript evidence justifies another cache or changing
serialization/conversion. No claims are made about unmeasured candidates.

## Measurement limits

These are warm local-loopback latency measurements on arm64 macOS, CPython
3.14.7, Node 26.9.0 (compiled TypeScript), PostgreSQL 18.6 and release Rust builds
with fat LTO, `fsync=on` and `synchronous_commit=on`. The benchmark's runtime/server strings are retained in each JSON.
Bun receives correctness coverage, not performance coverage. Other Python/Node
versions, remote network latency, production contention rates, multi-process
throughput, cold startup and hashing enormous names are unmeasured. Reentrant
lock-only cases are accompanied by complete transaction and contended cases,
but do not predict time spent waiting for a lock. Four-way concurrency is bounded
by the selected pool size. CPU costs of allocation/conversion are timed; peak
memory and allocation counts are not measured. No memory reduction is claimed.

The original and candidate paths share the same release build and unchanged read,
write, SQLite and raw SQL paths. The experiment isolates the advisory change; it
is not a broad before/after release benchmark. It does not establish performance
improvements in workloads that never take advisory locks.

## Correctness results

Python: 158 passed, one skipped because the PostgreSQL `vector` extension is
unavailable. Node and Bun: 131 passed each. Rust workspace tests,
clippy, strict mypy, and TypeScript type checks passed. Pyright: zero errors and
three existing `typing_extensions` import-source warnings. Added tests cover
empty/Unicode/block-boundary/long names, signed key boundaries, shared versus
exclusive contention, validation precedence and invalid inputs. Existing tests
cover transactions, cancellation, savepoints, rollback, raw SQL and SQLite.
The live compatibility probe holds locks in Python, checks contention in Node,
then checks release after Python rollback. User changes present at the start are
preserved; `docs/performance.md` is unchanged.

## Retained measurements

See [all workloads, intervals and run changes](results/summary.md) and the raw
JSON files in [results/](results/). Calibration measurements are excluded.

Across three repetitions, the benchmark gate passed: both languages have detected
end-to-end gains and none of the measured workloads has a detected regression.
Representative medians (µs, before → after):

| Public workload | Python | TypeScript / Node |
|---|---:|---:|
| Short string, blocking exclusive lock | 89.96 → 87.56 (−2.7%) | 90.18 → 60.45 (−33.0%) |
| 4 KiB string, blocking exclusive lock | 95.98 → 93.30 (−2.8%) | 1,047.88 → 98.78 (−90.6%) |
| Integer, blocking exclusive lock | 89.58 → 87.80 (−2.0%) | 62.04 → 61.28 (−1.2%) |
| Complete transaction with short-string lock | 251.79 → 248.04 (−1.5%) | 195.74 → 166.33 (−15.0%) |
| Transaction with lock, ORM read and ORM write | 555.09 → 552.24 (−0.5%) | 389.69 → 361.33 (−7.3%) |
| Four concurrent integer-lock transactions, pool size 4 | 674.35 → 646.80 (−4.1%) | 279.55 → 278.81 (inconclusive) |

Node's four-connection integer concurrency paired interval is −0.5% to +0.1%,
so that case establishes neither a detectable gain nor a detectable regression.
The string-lock and complete-transaction improvements qualify the combined change;
no separate optimization is claimed for this inconclusive case.
