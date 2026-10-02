# Phase 0 results: performance feasibility

**Question:** Is `Python → PyO3 → SeaORM → Postgres` fast enough to build on, once FFI
and Python object materialization are counted?

**Answer: yes.** For reads and bulk writes, the async PyO3 + SeaORM path (`ormcore-obj`)
was **2.0–4.5× faster than Django's async ORM** and **2.4–4.5× faster than SQLAlchemy
async** at every size tested (1, 50 and 1000 rows), over both the Unix socket and TCP.
The sync variant (`ormcore-sync`) is faster again on small queries. Building the Python
objects costs about **0.6–0.8 µs per row**, compared with **~8–9 µs per row** for Django.

## Headline numbers (median ms per call, lower is better)

Unix socket:

| operation | N | django-sync | django-async | sqla-asyncpg | **ormcore-obj** (async) | **ormcore-sync** | rust-only (floor) |
|---|---:|---:|---:|---:|---:|---:|---:|
| read posts | 1 | 0.44 | 0.78 | 0.88 | **0.32** | **0.19** | 0.13 |
| read posts | 50 | 0.89 | 1.41 | 1.26 | **0.43** | **0.22** | 0.20 |
| read posts | 1000 | 9.25 | 10.44 | 8.66 | **2.32** | **2.12** | 1.51 |
| read + author JOIN | 1000 | 17.57 | 18.77 | 11.56 | **4.17** | **3.80** | 3.06 |
| bulk insert | 50 | 3.72 | 4.18 | 4.61 | **1.54** | **1.20** | 1.09 |
| bulk insert | 1000 | 43.55 | 45.00 | 55.76 | **12.30** | **12.54** | 12.70 |
| insert one by one (N commits) | 1000 | 699 | 1122 | 1594 | **657** | **428** | 401 |

TCP (localhost) gives the same picture: see the full tables below.

## Transport: Unix socket vs TCP

**Every contender, Django included, was measured over both the Unix socket and TCP**
(`--transport unix|tcp`; all drivers use the same transport in a run). Django, SQLAlchemy
and the patched `ormcore` all behave about the same on both, with TCP up to ~0.1–0.3 ms
slower per call.

### sqlx doesn't set `TCP_NODELAY` (fixed by a vendored patch)

Before the patch, SeaORM's 1000-row bulk insert over TCP took **~55 ms**. Postgres's
own logs showed it spending only ~8 ms on the statement, and the same insert over the
Unix socket took ~12 ms.

The cause: sqlx 0.9 (SeaORM's driver) never calls `set_nodelay(true)` on its
`TcpStream`. Large multi-packet requests (here 43 KB of SQL plus 6000 bind parameters)
then stall on Nagle's algorithm combined with delayed ACKs, adding about 40 ms. libpq
(psycopg) and asyncpg both set `TCP_NODELAY`, so Django and SQLAlchemy never had this
problem. Small requests weren't affected.

**Fix:** `bench/ormcore/patches/sqlx-core` holds sqlx-core 0.9.0 plus a one-line
`stream.set_nodelay(true)` change, wired in through `[patch.crates-io]`. With it, the same
insert over TCP takes **12.8–13.2 ms**, matching the Unix socket. See
`bench/ormcore/patches/README.md`. We should send this fix upstream and drop the patch
once sqlx ships it.

| ormcore bulk insert, N=1000, TCP | before patch | after patch |
|---|---:|---:|
| `ormcore-obj` | 54.5 ms | 13.2 ms |
| `rust-only` | 55.9 ms | 13.7 ms |

## Async vs sync

Every `await` that crosses into Rust pays a fixed cost for handing work from Tokio back
to asyncio (Tokio worker wake-up, `call_soon_threadsafe`, event-loop wake-up). Django's
async ORM pays the same kind of cost through `sync_to_async`:

| bridge | median µs per call |
|---|---:|
| plain Python coroutine | 0.2 |
| PyO3 `future_into_py` (Tokio thread → asyncio) | 121.9 |
| Django `sync_to_async` (thread_sensitive) | 128.1 |
| PyO3 sync `block_on` (GIL released, no asyncio) | 0.3 |

(The sync `block_on` row is an already-finished future, so it shows that `block_on`
itself is free. A real query still waits on I/O in both modes; the full benchmark below
shows the real difference.)

What that means per query:

- **Single-row and small queries: sync is ~0.1–0.2 ms faster.** At N=1, a read takes
  0.19 ms sync vs 0.32 ms async; at N=50, 0.22 vs 0.43 ms; a single insert, 0.43 vs 0.57 ms.
- **Large results: about the same.** At 1000 rows, materialization and the database
  dominate (2.12 vs 2.32 ms).
- **Django shows the same effect:** sync 0.44 ms vs async 0.78 ms at N=1.

That's **latency for one query at a time**. Async is about concurrency: while one query
waits on Postgres, the event loop serves other requests. The sequential benchmark doesn't
measure that, so "sync is faster" doesn't mean "sync gives more throughput" for a web
server. Since both are cheap to provide on one Rust core, the recommendation is: **ship
async as the primary API, and offer a sync API too** (scripts, Django sync views,
Celery). The ~120 µs async hop is also worth optimizing later (current-thread runtime,
fewer thread hand-offs).

## What the data says about the design assumptions

| Assumption in PLAN.md | Verdict | Evidence |
|---|---|---|
| FFI overhead is usually small | **True for the call itself; the async hand-off is the real fixed cost** | ~120 µs per `await`, about equal to Django's `sync_to_async`. The sync path avoids it. |
| Object materialization is the expensive part | **True, and it's where Rust wins** | Read 1000: Rust floor 1.51 ms, PyO3 objects 2.12–2.32 ms (≈0.6–0.8 µs/row incl. tz-aware `datetime`), Django 9.25–10.44 ms (≈8–9 µs/row over the floor). |
| Database latency dominates | **True for small and per-commit workloads** | Single-row inserts cost 0.4–1.6 ms each for every contender, dominated by WAL fsync on commit. |
| Batch, don't cross the boundary row by row | **Confirmed** | 1000 rows: bulk insert 12.3 ms vs 657 ms one by one (~50×), mostly per-commit cost. |
| `#[pyclass]` objects vs dicts | **No meaningful difference** | `ormcore-obj` ≈ `ormcore-dict`, because fields are converted to Python once, up front. |

## Notes and caveats

- Hardware: shared cloud VM, 4 vCPU Xeon @ 2.1 GHz, Postgres 16.14 on the same host,
  default durability settings. Absolute numbers are noisy (see p95 in the JSON); the
  ratios were stable across runs and transports.
- Versions: Python 3.11, Django 5.2.17 (psycopg 3.3), SQLAlchemy 2.1.2 (asyncpg / psycopg),
  SeaORM 2.0.4 (sqlx 0.9 + `TCP_NODELAY` patch), PyO3 0.29, pyo3-async-runtimes 0.29.
- SQLAlchemy opens a transaction per session (BEGIN … ROLLBACK/COMMIT round trips).
  That's idiomatic usage, so it was left in.
- `ormcore` uses compile-time SeaORM entities that mirror the Django tables. This matches
  the decision that schema → IR → engine translation happens at compile time.
- Not measured yet: concurrent load (many in-flight queries), free-threaded Python,
  uvloop, Node bindings.

## Full results

Generated by `python bench/report.py bench/results-unix.json bench/results-tcp.json`.
Raw data (median, p95, mean, iterations): `results-unix.json`, `results-tcp.json`.

### Transport: unix — median ms per call (× = speed-up vs `django-async`)

#### Read N posts

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.44 (1.8×) | 0.89 (1.6×) | 9.25 (1.1×) |
| `django-async` | 0.78 | 1.41 | 10.44 |
| `django-async-dict` | 1.08 (0.7×) | 1.29 (1.1×) | 7.16 (1.5×) |
| `sqla-asyncpg` | 0.88 (0.9×) | 1.26 (1.1×) | 8.66 (1.2×) |
| `sqla-psycopg` | 1.02 (0.8×) | 1.42 (1.0×) | 8.06 (1.3×) |
| `ormcore-obj` | 0.32 (2.4×) | 0.43 (3.3×) | 2.32 (4.5×) |
| `ormcore-dict` | 0.32 (2.4×) | 0.51 (2.7×) | 2.30 (4.5×) |
| `ormcore-sync` | 0.19 (4.1×) | 0.22 (6.5×) | 2.12 (4.9×) |
| `rust-only` | 0.13 (5.8×) | 0.20 (7.1×) | 1.51 (6.9×) |

#### Read N posts + author (JOIN)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.91 (1.5×) | 1.71 (1.3×) | 17.57 (1.1×) |
| `django-async` | 1.37 | 2.23 | 18.77 |
| `django-async-dict` | 1.38 (1.0×) | 1.95 (1.1×) | 12.39 (1.5×) |
| `sqla-asyncpg` | 1.15 (1.2×) | 1.86 (1.2×) | 11.56 (1.6×) |
| `sqla-psycopg` | 1.50 (0.9×) | 2.35 (0.9×) | 12.24 (1.5×) |
| `ormcore-obj` | 0.42 (3.3×) | 0.72 (3.1×) | 4.17 (4.5×) |
| `ormcore-dict` | 0.53 (2.6×) | 0.79 (2.8×) | 4.78 (3.9×) |
| `ormcore-sync` | 0.29 (4.7×) | 0.52 (4.3×) | 3.80 (4.9×) |
| `rust-only` | 0.19 (7.1×) | 0.38 (5.9×) | 3.06 (6.1×) |

#### Bulk insert N posts (one statement)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.86 (1.7×) | 3.72 (1.1×) | 43.55 (1.0×) |
| `django-async` | 1.42 | 4.18 | 45.00 |
| `sqla-asyncpg` | 1.60 (0.9×) | 4.61 (0.9×) | 55.76 (0.8×) |
| `sqla-psycopg` | 1.56 (0.9×) | 5.35 (0.8×) | 79.33 (0.6×) |
| `ormcore-obj` | 0.60 (2.4×) | 1.54 (2.7×) | 12.30 (3.7×) |
| `ormcore-dict` | 0.65 (2.2×) | 1.55 (2.7×) | 13.66 (3.3×) |
| `ormcore-sync` | 0.46 (3.1×) | 1.20 (3.5×) | 12.54 (3.6×) |
| `rust-only` | 0.28 (5.1×) | 1.09 (3.8×) | 12.70 (3.5×) |

#### Insert N posts one at a time (N calls, N commits)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.65 (1.9×) | 37.94 (1.5×) | 699 (1.6×) |
| `django-async` | 1.21 | 56.79 | 1122 |
| `sqla-asyncpg` | 1.60 (0.8×) | 77.57 (0.7×) | 1594 (0.7×) |
| `sqla-psycopg` | 1.52 (0.8×) | 72.87 (0.8×) | 1560 (0.7×) |
| `ormcore-obj` | 0.57 (2.1×) | 28.24 (2.0×) | 657 (1.7×) |
| `ormcore-dict` | 0.63 (1.9×) | 29.50 (1.9×) | 600 (1.9×) |
| `ormcore-sync` | 0.43 (2.8×) | 21.31 (2.7×) | 428 (2.6×) |
| `rust-only` | 0.30 (4.1×) | 19.41 (2.9×) | 401 (2.8×) |

### Transport: tcp — median ms per call (× = speed-up vs `django-async`)

#### Read N posts

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.58 (1.4×) | 1.16 (1.3×) | 9.96 (1.1×) |
| `django-async` | 0.79 | 1.53 | 10.71 |
| `django-async-dict` | 1.14 (0.7×) | 1.47 (1.0×) | 7.15 (1.5×) |
| `sqla-asyncpg` | 1.13 (0.7×) | 1.43 (1.1×) | 7.99 (1.3×) |
| `sqla-psycopg` | 1.25 (0.6×) | 1.67 (0.9×) | 8.56 (1.3×) |
| `ormcore-obj` | 0.33 (2.4×) | 0.54 (2.8×) | 2.55 (4.2×) |
| `ormcore-dict` | 0.32 (2.4×) | 0.57 (2.7×) | 2.64 (4.1×) |
| `ormcore-sync` | 0.22 (3.6×) | 0.26 (6.0×) | 2.42 (4.4×) |
| `rust-only` | 0.18 (4.4×) | 0.25 (6.2×) | 2.07 (5.2×) |

#### Read N posts + author (JOIN)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 1.02 (1.4×) | 1.87 (1.2×) | 18.02 (1.0×) |
| `django-async` | 1.40 | 2.28 | 18.88 |
| `django-async-dict` | 1.47 (1.0×) | 2.00 (1.1×) | 12.33 (1.5×) |
| `sqla-asyncpg` | 1.41 (1.0×) | 2.22 (1.0×) | 11.57 (1.6×) |
| `sqla-psycopg` | 1.64 (0.9×) | 2.49 (0.9×) | 12.81 (1.5×) |
| `ormcore-obj` | 0.47 (3.0×) | 0.92 (2.5×) | 4.72 (4.0×) |
| `ormcore-dict` | 0.45 (3.1×) | 0.94 (2.4×) | 4.75 (4.0×) |
| `ormcore-sync` | 0.33 (4.3×) | 0.52 (4.4×) | 4.42 (4.3×) |
| `rust-only` | 0.23 (6.2×) | 0.42 (5.4×) | 3.53 (5.4×) |

#### Bulk insert N posts (one statement)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 1.15 (1.3×) | 4.00 (1.1×) | 44.78 (1.1×) |
| `django-async` | 1.55 | 4.45 | 48.28 |
| `sqla-asyncpg` | 1.90 (0.8×) | 4.76 (0.9×) | 53.18 (0.9×) |
| `sqla-psycopg` | 1.85 (0.8×) | 5.30 (0.8×) | 72.64 (0.7×) |
| `ormcore-obj` | 0.78 (2.0×) | 1.61 (2.8×) | 13.18 (3.7×) |
| `ormcore-dict` | 0.75 (2.1×) | 1.54 (2.9×) | 12.75 (3.8×) |
| `ormcore-sync` | 0.56 (2.8×) | 1.37 (3.3×) | 12.86 (3.8×) |
| `rust-only` | 0.51 (3.1×) | 1.23 (3.6×) | 13.69 (3.5×) |

#### Insert N posts one at a time (N calls, N commits)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.84 (1.7×) | 46.35 (1.5×) | 944 (1.4×) |
| `django-async` | 1.42 | 69.77 | 1305 |
| `sqla-asyncpg` | 1.86 (0.8×) | 94.80 (0.7×) | 1736 (0.8×) |
| `sqla-psycopg` | 1.82 (0.8×) | 89.36 (0.8×) | 1825 (0.7×) |
| `ormcore-obj` | 0.75 (1.9×) | 35.77 (2.0×) | 716 (1.8×) |
| `ormcore-dict` | 0.69 (2.1×) | 34.82 (2.0×) | 714 (1.8×) |
| `ormcore-sync` | 0.52 (2.8×) | 23.75 (2.9×) | 543 (2.4×) |
| `rust-only` | 0.45 (3.2×) | 21.61 (3.2×) | 411 (3.2×) |

