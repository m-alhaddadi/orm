# Current Rust engine versus SeaORM

This standalone Cargo workspace compares `orm-engine` with unmodified SeaORM
2.0.4 / SQLx 0.9. Both use PostgreSQL over the same connection URL and a pool
limited to one connection, on one current-thread Tokio runtime.

The executable requires `ORM_BENCH_URL` pointing to a disposable PostgreSQL
database. Run its release binary with this directory as the working directory;
it writes `results.json`. The table `orm_rust_compare_post` must not already
exist. The executable creates it, seeds 1,000 rows, and drops it on success.
After a failed run, use a fresh disposable database.

## What is timed

- SeaORM: query construction, SQL generation, execution, and entity decoding.
- Direct IR: cloning a typed operation template, planning, execution, and
  decoding into the exact same owned Rust model as SeaORM.
- JSON IR: parsing a prepared query JSON string, planning, execution, and the
  same model decoding. JSON serialization is not timed.

All three allocate and drop the same result structures inside the timed loop.
Schema compilation and connections are outside timing. This does not measure
Python or JavaScript bindings, generated Rust entities, or precompiled SQL.

## Cases and checks

Primary-key lookup, ordered reads of 50 and 1,000 rows, filtered count, and
updating 100 rows to a constant. Read rows have two integers, a title, and a
200-byte body. Queries bind values rather than interpolating literals.

The harness asserts full result equality across implementations before timing,
including expected row counts. It warms each contender 30 times, then records
15 batches per case, rotating contender order each round. Results include the
median and every batch average in microseconds per operation. A batch is
20–150 operations, depending on the case. PostgreSQL durability uses defaults.

These are sequential, warm-cache, local database measurements. They include
different underlying drivers (`tokio-postgres` versus SQLx), so they do not
isolate planner cost. They do not cover joins, prefetching, inserts, concurrent
load, remote databases, or mixed production workloads.


## Diesel extension

The current harness also includes Diesel 2.3.13 through diesel-async 0.9.2.
Its `AsyncPgConnection` uses the same Tokio runtime and PostgreSQL URL, with
one connection in a deadpool pool. Fast recycling avoids adding a connection
health query to every checkout. Diesel constructs typed queries on each call
and decodes directly into the same Rust model used by the other contenders.
The model derives both SeaORM's entity mapping and Diesel's `Queryable`.

The expanded comparison uses four contenders and 16 rounds, so each contender
occupies each execution-order position four times. The default output is now
`results-diesel.json`; `ORM_BENCH_OUT` selects another filename. Historical
three-contender output files remain unchanged.

All contenders pass the same complete read-result checks and the same independent
update-state checks. This is a comparison with Diesel's async adapter, rather
than its synchronous libpq connection implementation.


## Source investigation and controlled diagnostics

`src/bin/diagnose.rs` adds raw SQLx with both default and fast checkout settings,
SeaORM with both settings, prebuilt SQL through our driver, buffered and streamed
tokio-postgres typed reads, and Diesel async. It measures 1/50/1,000-row reads
and a separate CPU-only decode test. All result structs and fields are identical.
It requires the same disposable `ORM_BENCH_URL`; its table
`orm_rust_diagnose_post` must not exist, and it is dropped on success.
The default output is `results-diagnose.json`, overridable with `ORM_BENCH_OUT`.

Do not run benchmarks while compilation or another benchmark is running.
The retained calibration run overlapped a build and is excluded from reported
results. Diagnostic rounds rotate all nine variants through eighteen batches.

The original five-case harness accepts `ORM_BENCH_SEA_FAST=1` to disable
SeaORM's default per-checkout ping for a matched-setting experiment. With that
variable omitted, it retains the original default-health-check configuration.
The JSON records this setting. Historical result files remain available.
