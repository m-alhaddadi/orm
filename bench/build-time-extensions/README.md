# Extension refactor performance comparison

This compares the original checkout at `3a710cab6169752b07834021cacc384aa803a368`
with the interim contracts extraction and eager, atomic definition refactor.
The complete extension framework is still being implemented. These measurements
cover the core build, with behavioral extensions disabled. They cannot establish
performance of future enabled extension implementations.

Both sides use their own Python/TypeScript package and native release library.
Rust uses the same dependency versions, fat LTO, and one codegen unit. Two separate
native runtimes and pools coexist in each process. Each independent process runs
24 paired batches in alternating AB/BA order. CPU cases receive 20,000 warmup calls per side;
setup/database cases receive 30. Every result is retained to prevent unused query
objects from being optimized away.
Three processes run for each runtime/database combination. Builds and tests finish
before any retained timing runs.

The matrix covers definition to a ready-to-query state, frontend query construction,
fixed-document native SQL planning, fresh public query construction plus SQL,
awaited reads of 1/50/1,000 instances, a 50-row projection, updates, 50-row bulk
updates, and inserts of 1/50 rows through the bulk insert API. Read/write timers
include frontend work, native calls, database work, and result construction.
Definition includes the old version's lazy preparation so both sides end in the
same ready state. It is reported separately from warm operations.

PostgreSQL runs use a temporary isolated cluster on localhost port 55441, with
ordinary durability. Each contender owns 1,000 seed rows in the same table;
SQLite contenders have separate in-memory databases. Writes repeat identical
values and check affected counts; insert batches check returned values and delete
benchmark-owned rows outside the timer. Before timing, fixed-document SQL is
compared. Read checks cover counts, ordering, and decoded JSON. Each batch starts
with explicit language garbage collection outside the timer; automatic collection
remains enabled inside it.

Raw JSON, source/binary fingerprints, and allocation counts live in `results/`.
The report gives medians of process medians and deterministic, stratified bootstrap
intervals over paired batch percentage changes. The user accepts warm costs strictly below 1%. A case passes when its
paired 95% upper bound is below 1%; a lower bound at or above 1% establishes a
regression, and an interval crossing 1% remains inconclusive. These exploratory intervals have
no multiple-comparison correction. A local latency benchmark cannot prove
production equivalence, throughput, or cold-start behavior.

The allocation probe counts Rust allocations and requested bytes in warmed parsing
and planning. It is run separately, with allocation instrumentation; its timings
are excluded. It covers select, projection, and returning delete. It does not
measure language allocations, database-driver allocations, peak RSS, or enabled
extensions.

`build.py --out <empty-temporary-directory>` archives the baseline, overlays current
tracked/new source into a separate candidate snapshot, and builds both packages.
It requires installed Rust/TypeScript dependencies. `run.py --before <snapshot>
--after <snapshot> --url <isolated-postgres-url>` runs the sequential matrix;
`report.py` writes the summary. All scripts use the repository root as working
directory. Use an isolated writable database: benchmark table creation and writes
are part of the experiment.

## Current gate

The interim gate has **not passed**. Under the accepted less-than-1% rule,
20 of 44 warm cases have upper confidence bounds below the limit; 24 remain
inconclusive. None has a lower bound establishing a cost of 1% or more.
These results predate the complete implementation and must not be used as
final performance evidence. Fresh disabled and enabled controls are pending.

The first matrix is retained in `results/initial/`. A focused old-versus-old
control produced about a 42% apparent construction gain when results were
discarded. Retaining results removed that artifact. The refined matrix uses
retained results, sufficient CPU warmup, and fewer setup repetitions to limit
metadata accumulation before measuring warm operations. Diagnostic raw data
are retained as `results/cpu-*.json`; they are excluded from the final matrix.
Neither the apparent gains in construction nor the timing-only differences
establish a production optimization.

The native probe found identical allocations/requested bytes per operation:

| Parsing and planning | Allocations | Requested bytes |
|---|---:|---:|
| Select | 28 | 2,009 |
| Projection | 28 | 3,251 |
| Returning delete | 18 | 1,151 |

Native Python artifact size increased from 10,493,424 to 10,607,792 bytes (+1.1%);
Node increased from 9,122,224 to 9,228,512 bytes (+1.2%). Build/setup/artifact
costs are distinct from warm execution costs. Initial incremental release builds
took 112 seconds for the baseline and 100 seconds for the candidate; those
cache-dependent measurements do not establish a build-time improvement.

`report.py --check` fails when any warm upper confidence bound reaches 1%. Framework completion still
requires third-party static wiring and enabled-extension versus equivalent
built-in controls, including the specialized Rust and inheritance workloads
required by the implementation plan.
