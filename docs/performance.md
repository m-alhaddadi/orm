# Performance optimization opportunities

Status: measured opportunities, not scheduled or implemented in the production
engine. Evidence recorded on 2026-10-03. Start with result materialization for
large reads; the measurements below do not establish priorities for every
workload or language binding.

## Evidence and scope

The [benchmark report](../bench/RESULTS.md#diesel-investigation-and-pool-setting-correction--2026-10-03)
contains the source investigation, corrected ORM comparisons, and raw results.
The [diagnostic harness](../bench/rust-compare/src/bin/diagnose.rs) compares
identical owned Rust models on PostgreSQL 18.6, with one pooled connection per
variant, release builds, and three independent runs. It checks every returned
field and rotates contender order. Timings are medians of run medians.

| Opportunity | Measured evidence | Priority |
|---|---|---|
| Materialize results from the incoming row stream | Typed buffered versus typed streamed reads of 1,000 rows: **296.52 → 247.84 µs**, 16.4% lower latency | First |
| Choose a reusable decoder from the query output shape | CPU-only decoding and allocation of 1,000 already fetched rows: runtime cells **56.11 µs**, typed decoding **48.18 µs** | Second |
| Skip query planning for fixed queries | Current planner path **305.36 µs**, prebuilt SQL through our driver **298.88 µs** | Defer until another workload justifies it |

These experiments isolate different costs; their savings must not be added
together as a forecast. The streaming prototype also skips our planner and
runtime cell interface. Its absolute latency is not a production-engine result.
The controlled buffered/streamed pair uses the same SQL, pool, prepared-statement
cache, typed decoder, and final result type. Small reads showed little benefit
from streaming.

## 1. Stream into final results

Today, [the PostgreSQL driver](../engine/src/db/postgres.rs) calls
`Client::query`, collects a `Vec<Row>`, and returns a boxed `RowSet`. Consumers
then allocate and fill their final result collection. Diesel's async adapter
uses `query_raw`, converts arriving rows into typed models, and collects those
models directly. Our diagnostic prototype demonstrates the same approach with
tokio-postgres.

Prototype an internal query consumer or materialization API that can build final
results while consuming the incoming stream. Retain the buffered path for
existing consumers and operations that need a row set, including relation-key
collection and prefetching. Public query results can still be complete lists;
this opportunity does not require a public streaming API.

The implementation must preserve ordering, null handling, errors, transactions,
and cancellation behavior. Python and Node objects must be constructed in the
runtime context their bindings require; the Rust-struct prototype does not
measure those constraints. Measure binding materialization and thread hand-off
costs before claiming the same improvement for Python or JavaScript.

## 2. Reuse decoders for an output shape

`RowSet::cell` dispatches on `ValueType` for each field of each row. Investigate
choosing column decoders once from a validated output shape and reusing that
recipe across rows. Include selected expressions, joined models, nullable
relations, enum mappings, arrays, decimals, and extension types in the design.

For a future native Rust frontend, generated model types can enable specialized
row decoding. That is a distinct opportunity from compiling schema metadata
merely to reduce startup time. Shared Python/Node packages should continue to
support dynamic schemas without requiring a native build for each schema.

## 3. Keep planning and cache changes evidence-driven

Prepared database statements are already cached by our driver. Diesel also
caches prepared statements using typed query identities where available; the
inspected diesel-async version still renders SQL before its cache lookup.
Its read performance does not demonstrate that all SQL was compiled at build
time.

The current simple-read diagnostic attributes only about 6.5 µs to bypassing
planning. Keep the existing decision against a general IR-keyed plan cache
unless complex-query profiling establishes enough benefit to justify binding
recipes, cache memory, invalidation, and correctness work. Building typed Rust
operations directly can avoid JSON parsing independently of plan caching.

## Benchmark and acceptance requirements

- Match pool checkout policies, transport, TLS, connection count, database
  settings, result shape, and decoded fields. Earlier SeaORM comparisons used
  default per-checkout health pings while our engine and Diesel used fast
  recycling; use the corrected results when assessing implementation costs.
- Run release benchmarks sequentially after compilation finishes. Preserve raw
  measurements, rotate execution order, and report variability. The retained
  diagnostic calibration run overlapped compilation and is excluded.
- Assert complete result equality and write-state correctness. Include small
  reads as well as large reads to detect regressions.
- Before shipping an engine change, exercise joins, projections, nested
  prefetching, transactions, cancellation, conversion errors, and supported
  column types. Verify the existing buffered API and SQLite behavior.
- Measure the public Python and Node APIs and concurrent workloads. The current
  diagnostic covers sequential local PostgreSQL reads of 1, 50, and 1,000 rows;
  it does not establish production throughput or memory savings. Measure peak
  memory separately if the implementation claims a memory improvement.

Full comparison methodology and reproduction details live in
[bench/rust-compare/README.md](../bench/rust-compare/README.md).
