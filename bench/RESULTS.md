# Phase 0 results: performance feasibility

**Question:** Is `Python (asyncio) → PyO3 → SeaORM → Postgres` fast enough to build on,
once FFI and Python object materialization are counted?

**Answer: yes.** For reads and bulk writes, the PyO3 + SeaORM path (`ormcore-*`) was
**2.2–4.5× faster than Django's async ORM** and **2.0–3.6× faster than SQLAlchemy async** at
every size tested (1, 50 and 1000 rows). It also stays within 1.2–2.7× of pure Rust.
Building the Python objects costs about **0.9 µs per row**, compared with
**~8 µs per row** for Django. The one place the Rust stack lost was a
**driver bug over TCP** (sqlx doesn't set `TCP_NODELAY`; details below), and it has a
clear fix.

## Headline numbers (Unix socket, median ms per call, lower is better)

| operation | N | django-async | sqla-asyncpg | **ormcore-obj** | rust-only (floor) |
|---|---:|---:|---:|---:|---:|
| read posts | 1 | 0.90 | 0.79 | **0.35** | 0.13 |
| read posts | 50 | 1.48 | 1.18 | **0.51** | 0.19 |
| read posts | 1000 | 9.75 | 7.70 | **2.22** | 1.35 |
| read posts + author JOIN | 1000 | 18.62 | 11.61 | **4.15** | 2.78 |
| bulk insert | 50 | 3.93 | 4.50 | **1.52** | 1.12 |
| bulk insert | 1000 | 42.65 | 49.22 | **13.63** | 11.25 |
| insert one-by-one (N commits) | 1000 | 1122 | 1572 | **686** | 481 |

## What the data says about the design assumptions

| Assumption in PLAN.md | Verdict | Evidence |
|---|---|---|
| FFI overhead is usually small | **True for the call itself; the async bridge is the real fixed cost** | One `await` through `pyo3-async-runtimes` costs ~115 µs with no DB work, about the same as Django's `sync_to_async` (~127 µs). That accounts for most of the 0.35 ms vs 0.13 ms gap at N=1. |
| Object materialization is the expensive part | **True, and it's where Rust wins** | Read 1000: Rust floor 1.35 ms, PyO3 objects 2.22 ms (≈0.9 µs/row incl. a tz-aware `datetime`; ≈1.4 µs/row with the joined author object), Django 9.75 ms (≈8.4 µs/row over the floor). |
| Database latency dominates | **True for small and per-commit workloads** | Single-row inserts sit at 0.4–1.1 ms per row for everyone, dominated by WAL fsync on commit. At N=1 every stack is within ~1 ms. |
| Batch, don't cross the boundary row by row | **Confirmed** | 1000 rows: bulk insert 13.6 ms vs 686 ms one-by-one (50×). That's mostly per-commit cost; each extra crossing adds ~0.2 ms on top. |
| `#[pyclass]` objects vs dicts | **No meaningful difference** | `ormcore-obj` ≈ `ormcore-dict` everywhere, because fields are converted to Python once at materialization time, not lazily per access. |

## Finding: sqlx does not set `TCP_NODELAY`

Over localhost TCP, SeaORM's 1000-row bulk insert took **~55 ms**. Server-side logging
showed Postgres spending only ~8 ms on it. Over a Unix socket the same insert takes
**~12 ms**.

The cause is in sqlx 0.9 (SeaORM's driver). It never calls `set_nodelay(true)` on its
`TcpStream` (`sqlx-core/src/net/socket/mod.rs`). Large multi-packet requests
(here 43 KB of SQL plus 6000 bind parameters) then stall on Nagle's algorithm combined
with delayed ACKs, adding about 40 ms. libpq (psycopg) and asyncpg both set `TCP_NODELAY`,
so Django and SQLAlchemy don't have this problem. Small requests are unaffected:
reads over TCP look the same as over the Unix socket.

**This must be fixed before any real use.** Options: a custom connector that sets
`TCP_NODELAY`, an upstream patch to sqlx, or a different driver
(`tokio-postgres` sets it). This also supports keeping the engine replaceable behind
our own IR.

## Where the remaining overhead is (ormcore vs rust-only)

- **~115 µs fixed per `await`**: the asyncio ↔ Tokio hop (Tokio worker wake-up,
  `call_soon_threadsafe`, event-loop wake-up). This dominates N=1 calls. Candidates to
  try: a current-thread runtime, completing the asyncio future without the extra
  thread hop, or a sync fast path for very cheap queries.
- **~0.9 µs per row** to build Python objects (string copies plus `datetime`
  construction); ~1.4 µs per row including a joined author object.
- **~0.7–2.4 µs per row** on writes to extract Python dicts into Rust structs (bulk insert
  1000: 11.9–13.6 ms vs 11.25 ms; `-obj` and `-dict` run the same write code, so the
  spread is noise).

## Notes and caveats

- Hardware: shared cloud VM, 4 vCPU Xeon @ 2.1 GHz, Postgres 16.14 on the same host,
  default durability settings. Absolute numbers are noisy (look at p95 in the JSON). The
  ratios were stable across the Unix and TCP runs.
- Versions: Python 3.11, Django 5.2.17 (psycopg 3.3), SQLAlchemy 2.1.2 (asyncpg / psycopg),
  SeaORM 2.0.4 (sqlx 0.9), PyO3 0.29, pyo3-async-runtimes 0.29.
- Django's async ORM still runs queries in a worker thread via `sync_to_async`, so for
  small queries Django sync is faster than Django async (0.45 vs 0.90 ms at N=1).
- SQLAlchemy opens a transaction per session (BEGIN … ROLLBACK/COMMIT round trips).
  That's idiomatic usage, so it was left in.
- `ormcore` uses compile-time SeaORM entities that mirror the Django tables. This matches
  the decision that schema → IR → engine translation happens at compile time.
- Not measured yet: concurrency (many in-flight queries), free-threaded Python, uvloop,
  Node bindings.

## Full results

Generated by `python bench/report.py bench/results-unix.json bench/results-tcp.json`.
Raw data (median, p95, mean, iterations): `results-unix.json`, `results-tcp.json`.

### Async bridge cost (no DB work)

Generated by `python bench/bridge_overhead.py`.

| bridge | median µs per await |
|---|---:|
| plain Python coroutine | 0.2 |
| PyO3 `future_into_py` (Tokio thread → asyncio) | 115.3 |
| Django `sync_to_async` (thread_sensitive) | 127.0 |

### Transport: unix — median ms per call (× = speed-up vs `django-async`)

#### Read N posts

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.45 (2.0×) | 0.87 (1.7×) | 9.82 (1.0×) |
| `django-async` | 0.90 | 1.48 | 9.75 |
| `django-async-dict` | 1.00 (0.9×) | 1.31 (1.1×) | 6.92 (1.4×) |
| `sqla-asyncpg` | 0.79 (1.1×) | 1.18 (1.3×) | 7.70 (1.3×) |
| `sqla-psycopg` | 1.07 (0.8×) | 1.31 (1.1×) | 7.80 (1.2×) |
| `ormcore-obj` | 0.35 (2.5×) | 0.51 (2.9×) | 2.22 (4.4×) |
| `ormcore-dict` | 0.27 (3.3×) | 0.40 (3.7×) | 2.25 (4.3×) |
| `rust-only` | 0.13 (6.7×) | 0.19 (7.7×) | 1.35 (7.2×) |

#### Read N posts + author (JOIN)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.92 (1.4×) | 1.77 (1.2×) | 18.48 (1.0×) |
| `django-async` | 1.29 | 2.09 | 18.62 |
| `django-async-dict` | 1.29 (1.0×) | 1.86 (1.1×) | 12.38 (1.5×) |
| `sqla-asyncpg` | 1.11 (1.2×) | 1.64 (1.3×) | 11.61 (1.6×) |
| `sqla-psycopg` | 1.37 (0.9×) | 2.35 (0.9×) | 11.76 (1.6×) |
| `ormcore-obj` | 0.46 (2.8×) | 0.80 (2.6×) | 4.15 (4.5×) |
| `ormcore-dict` | 0.49 (2.6×) | 0.81 (2.6×) | 4.27 (4.4×) |
| `rust-only` | 0.19 (6.6×) | 0.36 (5.8×) | 2.78 (6.7×) |

#### Bulk insert N posts (one statement)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.97 (1.5×) | 3.84 (1.0×) | 43.38 (1.0×) |
| `django-async` | 1.41 | 3.93 | 42.65 |
| `sqla-asyncpg` | 1.57 (0.9×) | 4.50 (0.9×) | 49.22 (0.9×) |
| `sqla-psycopg` | 1.65 (0.9×) | 5.23 (0.8×) | 70.89 (0.6×) |
| `ormcore-obj` | 0.64 (2.2×) | 1.52 (2.6×) | 13.63 (3.1×) |
| `ormcore-dict` | 0.69 (2.0×) | 1.42 (2.8×) | 11.92 (3.6×) |
| `rust-only` | 0.42 (3.3×) | 1.12 (3.5×) | 11.25 (3.8×) |

#### Insert N posts one at a time (N calls, N commits)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.84 (1.5×) | 41.34 (1.3×) | 795 (1.4×) |
| `django-async` | 1.26 | 52.95 | 1122 |
| `sqla-asyncpg` | 1.64 (0.8×) | 77.42 (0.7×) | 1572 (0.7×) |
| `sqla-psycopg` | 1.58 (0.8×) | 75.86 (0.7×) | 1689 (0.7×) |
| `ormcore-obj` | 0.67 (1.9×) | 33.95 (1.6×) | 686 (1.6×) |
| `ormcore-dict` | 0.71 (1.8×) | 34.37 (1.5×) | 698 (1.6×) |
| `rust-only` | 0.52 (2.4×) | 20.13 (2.6×) | 481 (2.3×) |

### Transport: tcp — median ms per call (× = speed-up vs `django-async`)

#### Read N posts

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.49 (2.1×) | 1.09 (1.3×) | 9.60 (1.1×) |
| `django-async` | 1.01 | 1.45 | 10.38 |
| `django-async-dict` | 1.15 (0.9×) | 1.48 (1.0×) | 7.11 (1.5×) |
| `sqla-asyncpg` | 1.10 (0.9×) | 1.43 (1.0×) | 8.15 (1.3×) |
| `sqla-psycopg` | 1.10 (0.9×) | 1.57 (0.9×) | 8.67 (1.2×) |
| `ormcore-obj` | 0.31 (3.3×) | 0.39 (3.8×) | 2.48 (4.2×) |
| `ormcore-dict` | 0.32 (3.1×) | 0.48 (3.0×) | 2.60 (4.0×) |
| `rust-only` | 0.15 (6.5×) | 0.24 (6.1×) | 1.60 (6.5×) |

#### Read N posts + author (JOIN)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.96 (1.4×) | 1.88 (1.2×) | 17.60 (1.0×) |
| `django-async` | 1.35 | 2.27 | 18.07 |
| `django-async-dict` | 1.44 (0.9×) | 2.07 (1.1×) | 12.67 (1.4×) |
| `sqla-asyncpg` | 1.36 (1.0×) | 2.04 (1.1×) | 11.72 (1.5×) |
| `sqla-psycopg` | 1.47 (0.9×) | 2.29 (1.0×) | 12.53 (1.4×) |
| `ormcore-obj` | 0.43 (3.2×) | 0.79 (2.9×) | 4.69 (3.9×) |
| `ormcore-dict` | 0.44 (3.1×) | 0.82 (2.8×) | 4.63 (3.9×) |
| `rust-only` | 0.23 (6.0×) | 0.41 (5.5×) | 3.45 (5.2×) |

#### Bulk insert N posts (one statement)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.75 (2.2×) | 3.98 (1.1×) | 43.10 (1.0×) |
| `django-async` | 1.61 | 4.21 | 42.68 |
| `sqla-asyncpg` | 1.93 (0.8×) | 4.73 (0.9×) | 50.99 (0.8×) |
| `sqla-psycopg` | 1.79 (0.9×) | 5.18 (0.8×) | 67.17 (0.6×) |
| `ormcore-obj` | 0.74 (2.2×) | 1.51 (2.8×) | 54.53 (0.8×) |
| `ormcore-dict` | 0.76 (2.1×) | 1.63 (2.6×) | 54.38 (0.8×) |
| `rust-only` | 0.35 (4.6×) | 1.17 (3.6×) | 55.92 (0.8×) |

#### Insert N posts one at a time (N calls, N commits)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.87 (1.6×) | 35.47 (1.9×) | 800 (1.7×) |
| `django-async` | 1.44 | 67.14 | 1359 |
| `sqla-asyncpg` | 1.72 (0.8×) | 78.87 (0.9×) | 1740 (0.8×) |
| `sqla-psycopg` | 1.78 (0.8×) | 88.07 (0.8×) | 1749 (0.8×) |
| `ormcore-obj` | 0.76 (1.9×) | 34.75 (1.9×) | 716 (1.9×) |
| `ormcore-dict` | 0.76 (1.9×) | 37.70 (1.8×) | 727 (1.9×) |
| `rust-only` | 0.42 (3.4×) | 23.95 (2.8×) | 407 (3.3×) |

