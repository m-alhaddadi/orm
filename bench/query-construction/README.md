# Query construction investigation

No production optimization was retained. The measured Rust decoder changes an
existing error, so it fails compatibility regardless of timing. Production source
and installed extensions were restored. Existing user changes, including
`docs/performance.md` and code generation changes, were preserved.

## What is already in Rust

Python `_Ctes` and TypeScript `Ctes` collect CTE bodies once per object, hoist
dependencies to the outer statement in dependency order, track recursion, and
collect positional parameters. `IRContext` registers named windows by identity
within each query scope. These are frontend expression-to-IR operations.

`core/src/ir.rs` defines shared wire types. `engine/src/plan.rs` derives CTE column
types, builds recursive UNION/UNION ALL, validates column scope, plans expressions
and joins, and validates/declares windows and frames. Both bindings invoke this
planner and `engine/src/exec.rs`; parameter conversion follows its types. Moving
SQL responsibilities into Rust would duplicate work already done there.

The inspection also covered write normalization, parameter interfaces, native
conversion in both bindings, Python's native instance builder, TypeScript's
result builder, PostgreSQL buffering/preparation, and schema metadata.

## Rejected candidate: direct operation decoding

The internally tagged `Operation` enum buffers a complete document before decoding
its variant. The prototype directly decodes the body when `op` is first, removing
that outer buffer. Nested expression decoding, planning and frontends stay intact.
Other layouts and decoding failures use the original decoder for its errors.
No cache, alternate SQL or skipped result conversion is involved.

The fallback is insufficient. Given:

```json
{"op":"select","model":"Book","unknown":1e999}
```

The original rejects it with `invalid query IR: number out of range at line 1
column 45`. The prototype succeeds: struct decoding ignores the field without
converting the number. Ignored fields in nested query structs have the same risk.
The included differential Rust test fails on this case. A successful fast-path
parse cannot recover the original error with a fallback. Revalidation or another
recursive decoder needs a new compatibility and end-to-end experiment.
The prototype is kept only as [an unapplied experimental patch](direct-operation.patch).

## Benchmark method

- Three independent processes per runtime/backend, 24 paired batches per case,
  alternating AB/BA order; 30 warm-up calls per variant. Each batch has 100
  operations, or 20 for the 1,000-row read. Runtime order reverses on repetition
  two. Builds and tests never overlap timing.
- Two release extensions load in the same process with separate module names,
  four-connection pools and Rust runtimes. PostgreSQL variants read identical
  benchmark-owned data. SQLite uses separately seeded in-memory databases.
- Every timed operation constructs a fresh public query and awaits its complete
  result. Timers include frontend construction, IR traversal, JSON serialization,
  FFI/native argument extraction, parsing, SQL planning/building, typed database
  parameter conversion, checkout/handoff, execution, native decoding/allocation,
  language objects and result-cache overhead. Cached results are never reused.
- Cases: reads of 1/50/1,000 models; a 50-row projection; 4/16-level nested CTEs;
  a diamond sharing one dependency; an eight-row bounded recursive CTE; and
  3/16/64 sums sharing one ordered, framed window. CTE/window cases return eight
  rows. Each author has 1,000 books with text, enum, nullable JSON and integers.
- Before timing, all projection/window values are compared; model text, integer,
  enum and JSON fields are compared while seed identities are normalized.
  Expected row counts are checked. These comparisons do not replace broader
  correctness tests; no candidate passed all correctness requirements.
- SQLite named windows are excluded: the baseline smoke test fails with
  `no such window: w1`. PostgreSQL handles the same query. This existing behavior
  is unchanged. Failed smoke timings are excluded.
- [All results](results/summary.md) and [raw JSON](results/) are retained.
  Timings are medians of three run medians. Paired intervals use 3,000 deterministic
  bootstrap resamples of batch percentage changes, stratified by process, taking
  the median of run medians. This statistic differs from the ratio of separately
  aggregated medians. Exploratory intervals have no multiple-comparison correction.

## Other candidates left out

**CTE collection migration:** not implemented. Bodies are already emitted once.
Identity checks, dependency traversal, parameter order, recursive callbacks and
errors are observable. A native collector still needs expression traversal/scope
or language callbacks; returning IR adds conversion and boundary costs. This
experiment measures a shared Rust cost and provides no migration performance claim.

**Window registration migration:** not implemented. Reuse of one definition
usually checks one identity entry. Rust currently permits one named window per
query and already plans its SQL. No evidence justifies another representation/FFI.

**Write validation/alignment:** not implemented. Python supports callable defaults,
mapping iterables and attribute-based relation values. TypeScript skips undefined,
maps names and normalizes number/bigint keys. Errors and their timing intentionally
differ. Migration requires independent public-write benchmarks and compatibility
coverage; duplication alone is insufficient.

**Streaming/materialization:** deferred. `docs/performance.md` contains Rust-only
evidence that excludes language object construction and runtime handoff. Python
builds instances natively; Node emits cells for TypeScript objects. Those costs,
joins, prefetches and conversions need a separate public-API experiment.

**General plan caching:** deferred. Driver statements are cached and prepared query
APIs exist. Parameter recipes, schema identity, invalidation, memory and errors
need evidence. Decoder results do not establish a caching gain.

## Reproduce (Nushell, repository root)

Dependencies must be installed. Use an isolated writable PostgreSQL database;
retained runs used the existing local server on port 55439. The builder checks
patch applicability, builds before/after extensions, records the expected failing
compatibility test, and restores production source/extensions in `finally`.
Do not edit the engine or run other builds during it.

```nu
^.venv/bin/python bench/query-construction/build.py --out /tmp/orm-query-binaries
^.venv/bin/python bench/query-construction/run.py --url 'postgres://postgres:postgres@127.0.0.1:55439/orm_test' --before-python /tmp/orm-query-binaries/before.so --after-python /tmp/orm-query-binaries/after.so --before-node /tmp/orm-query-binaries/before.node --after-node /tmp/orm-query-binaries/after.node --out /tmp/orm-query-results
^.venv/bin/python bench/query-construction/report.py --directory /tmp/orm-query-results --out /tmp/orm-query-results/summary.md
```

Wait for each command to finish successfully before the next. The expected
compatibility failure is recorded in `/tmp/orm-query-binaries/compatibility.txt`;
it is not a passing check. Correctness checks on restored production code:

```nu
$env.ORM_TEST_DATABASE_URL = 'postgres://postgres:postgres@127.0.0.1:55439/orm_test'
^.venv/bin/python -m pytest -q
^cargo test -q
^npm --prefix js test
^npm --prefix js run typecheck
```

## Measurement limits

Warm loopback PostgreSQL 18.6 and in-memory SQLite, arm64 macOS, CPython 3.14.7,
Node 26.9.0 with compiled TypeScript, release Rust/fat LTO. Allocation, conversion
and serialization CPU costs are timed; allocation counts/peak memory are not.
Each native extension has its own runtime; scheduling differences can influence
small changes. Pool size is four but timing is sequential. Concurrency,
transactions, writes, relation joins/prefetch, remote latency, cold startup and
other runtime versions are not performance-covered. Correctness coverage cannot
substitute for measurements. No memory or throughput claim is made.

This matrix cannot establish acceptance of an operation-parser change even if
some cases are faster: compatibility fails, and other affected workload families
remain unmeasured.

## Results

Selected PostgreSQL medians (µs, before → after):

| Workload | Python | TypeScript / Node |
|---|---:|---:|
| Sixteen nested CTEs | 1,194.49 → 1,190.15 (−0.4%, inconclusive) | 303.43 → 297.31 (−2.0%) |
| Shared CTE dependency | 443.79 → 441.96 (−0.4%, inconclusive) | 213.93 → 212.13 (−0.8%) |
| Recursive CTE | 750.83 → 753.52 (+0.4%, inconclusive) | 605.80 → 602.92 (−0.5%, inconclusive) |
| Sixty-four uses of one window | 873.06 → 869.20 (−0.4%) | 524.45 → 518.19 (−1.2%) |
| Read 1,000 models | 966.01 → 964.25 (−0.2%, inconclusive) | 1,184.96 → 1,196.49 (+1.0%) |

Node's 1,000-model read has a positive paired interval of +0.7% to +1.6%, so the
measured matrix fails the no-detected-regression rule. Its individual run changes
are +0.4%, +4.2% and +1.0%. Most PostgreSQL effects are inconclusive. Marginal
negative intervals are exploratory, not reasons to override the failed gates.
No follow-up optimization was justified by these measurements.

Restored implementation: Python 158 passed, one skipped for the unavailable
PostgreSQL vector extension. Node and Bun each passed 131 tests. Rust workspace
tests and clippy passed. Strict mypy,
TypeScript type checks and the benchmark's TypeScript compilation passed.
Pyright reports zero errors and three existing typing_extensions source warnings.
