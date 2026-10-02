# Phase 0 results: performance feasibility

**Question:** Is one Rust ORM core (SeaORM on Postgres), called natively from Python,
Node, Bun and Go, fast enough compared with each language's own ORM?

**Answer: yes for Python and JS, about even for Go.**

| language | binding | vs its native ORM | verdict |
|---|---|---|---|
| **Python** | PyO3 | **2–4.5× faster** than Django / SQLAlchemy async on reads and bulk writes | Big win: Python object building is slow and Rust replaces most of it. |
| **Node** | napi-rs | **up to 2× faster** on small queries, **3.3× faster** on bulk writes, **even** on 1000-row reads | Win, except large reads (napi builds JS objects slowly). |
| **Bun** | same napi addon | **up to 4.8× faster** on bulk writes; 1000-row reads **0.8–1.0×** | Same picture as Node. Bun's own `pg` path is fast at reads. |
| **Go** | cgo (C ABI) | **0.4–1.2×** vs GORM; raw pgx is faster than both | No speed win. Go is already compiled; Rust only adds a copy. |

The engine is worth it for speed in Python and JS. In Go it gives consistency (the same
models, queries and behaviour as the other languages), not speed.

## Summary

| question | answer |
|---|---|
| Sync or async: which is faster? | **Sync, by ~0.1–0.2 ms per call** (no event-loop hand-off). **Async handles more traffic** under concurrent load. Ship both, with async as the primary API. |
| Unix socket or TCP? | **About the same for everyone** (TCP ≤ ~0.3 ms slower per call), once sqlx's missing `TCP_NODELAY` is patched. Before the patch, large SeaORM writes over TCP were ~4× slower. |
| How much does the language → Rust call cost? | Go cgo call: **~70 ns**. Node/Bun async (Promise ↔ Tokio): **~40 µs**. Python async (asyncio ↔ Tokio): **~115 µs**. Sync calls in every language: **~0**. |

## Cross-language: median ms per call, Unix socket

Read 1 post:

| | native ORM | Rust core, async | Rust core, sync | pure Rust floor |
|---|---:|---:|---:|---:|
| Python | 0.79 (Django async) · 0.43 (Django sync) | 0.32 | 0.20 | 0.14 |
| Node | 0.36 (Drizzle) | 0.20 | 0.21 | 0.14 |
| Bun | 0.24 (Drizzle) | 0.23 | 0.21 | 0.14 |
| Go | 0.13 (GORM) · 0.11 (pgx) | — | 0.22 (cgo) | 0.14 |

Read 1000 posts:

| | native ORM | Rust core, async | Rust core, sync | pure Rust floor |
|---|---:|---:|---:|---:|
| Python | 10.25 (Django async) · 9.72 (Django sync) | 2.50 | 2.30 | 1.43 |
| Node | 3.32 (Drizzle) | 3.37 | 3.48 | 1.43 |
| Bun | 2.76 (Drizzle) | 3.63 | 3.40 | 1.43 |
| Go | 2.52 (GORM) · 0.90 (pgx) | — | 2.59 (cgo) | 1.43 |

Bulk insert 1000 posts:

| | native ORM | Rust core, async | Rust core, sync | pure Rust floor |
|---|---:|---:|---:|---:|
| Python | 41.94 (Django async) · 43.14 (Django sync) | 12.77 | 13.55 | 12.22 |
| Node | 50.01 (Drizzle) | 15.35 | 15.27 | 12.22 |
| Bun | 68.54 (Drizzle) | 15.84 | 14.38 | 12.22 |
| Go | 16.68 (GORM) · 12.48 (pgx) | — | 14.75 (cgo) | 12.22 |

"Pure Rust floor" is SeaORM timed inside Rust with no foreign objects (measured from
the Python harness, `rust-only`).

## What each language teaches

**Python.** Building objects is where Django and SQLAlchemy spend their time: ~8–9 µs
per row over the Rust floor, against ~0.6–0.8 µs for the PyO3 binding. That gap is the
whole case for the engine.

**Node / Bun.** Writes are the big win: the Rust side builds and sends a 1000-row INSERT
3–5× faster than Drizzle on `pg`. Large reads are even, because napi creates each JS
object property by property (~2 µs per row), which costs about what V8 saves by
parsing rows itself. Possible fixes: return rows as arrays or a columnar buffer
instead of objects, or build objects lazily.

**Go.** pgx is a very fast native driver: it reads 1000 rows in 0.9 ms, faster than
SeaORM itself (1.43 ms). The cgo call is cheap (~70 ns), but copying each row out of
Rust memory into Go strings and times costs about as much as GORM's reflection, so we
land level with GORM. Each cgo call also ties up an OS thread while it waits on Postgres,
which matters under heavy concurrency. Use the Rust core from Go for consistency across
languages, not for speed.

**Single-row inserts** are dominated everywhere by the per-commit WAL fsync
(0.3–1.5 ms per row). The ORM matters least there.

## Transport: Unix socket vs TCP

Every contender in every language was measured over both the Unix socket and localhost
TCP. All drivers in a run use the same transport. Results were consistent between the
two: TCP is up to ~0.1–0.3 ms slower per call, and the rankings didn't change.

### sqlx doesn't set `TCP_NODELAY` (fixed by a vendored patch)

Before the patch, SeaORM's 1000-row bulk insert over TCP took **~55 ms**. Postgres's
own logs showed it spending only ~8 ms on the statement, and the same insert over the
Unix socket took ~12 ms. The cause: sqlx 0.9 never calls `set_nodelay(true)`, so large
multi-packet requests stall on Nagle's algorithm combined with delayed ACKs, adding
about 40 ms. libpq, asyncpg, `pg` and pgx all set `TCP_NODELAY`.

**Fix:** `bench/ormcore/patches/sqlx-core` holds sqlx-core 0.9.0 plus a one-line
`stream.set_nodelay(true)` change, wired in through `[patch.crates-io]`. It applies to all
four bindings. We should send this fix upstream and drop the patch once sqlx ships it.

| Rust core bulk insert, N=1000, TCP | before patch | after patch |
|---|---:|---:|
| Python `ormcore-obj` | 54.5 ms | 13.9 ms |
| Python `rust-only` | 55.9 ms | 12.6 ms |

## Async vs sync

An async call into Rust pays to hand the result from Tokio back to the language's event
loop. A sync call blocks the caller on the runtime and pays nothing extra:

| language → Rust | median per call |
|---|---:|
| Python `future_into_py` (asyncio ↔ Tokio) | ~113 µs |
| Python: Django `sync_to_async`, for comparison | ~132 µs |
| Node napi async fn (Promise ↔ Tokio) | ~41 µs |
| Bun napi async fn | ~37 µs |
| Go cgo call (always sync) | ~70 ns |
| Any sync call (`block_on`), excluding I/O | ~0.2 µs |

In practice:
- **Python:** sync is ~0.1–0.2 ms faster on small queries (read 1 post: 0.20 vs 0.32 ms).
- **Node/Bun:** the gap is smaller (~40 µs), and sync and async are often within noise.
- **Large results:** async and sync are about the same, because materialization and the
  database dominate.

This is single-query latency. Async exists so a server can keep other requests moving
while one waits on Postgres, and this sequential benchmark doesn't measure that.
Recommendation: **async as the primary API, plus a sync API** (scripts, sync Django views,
Celery, CLIs). Go uses its normal blocking style.

## Notes and caveats

- Hardware: shared cloud VM, 4 vCPU Xeon @ 2.1 GHz, Postgres 16.14 on the same host,
  default durability settings. Absolute numbers are noisy (see p95 in the JSON); the
  rankings were stable across runs and transports.
- Versions: Python 3.11, Django 5.2.17 (psycopg 3.3), SQLAlchemy 2.1.2 (asyncpg, psycopg);
  Node 22.22, Bun 1.3.14, Drizzle 0.45 (`pg` 8.23); Go 1.25, GORM 1.31, pgx 5.11;
  SeaORM 2.0.4 (sqlx 0.9 + `TCP_NODELAY` patch), PyO3 0.29, napi-rs 3.
- Defaults were kept for each ORM, so every contender runs as idiomatic code:
  - SQLAlchemy opens a transaction per session.
  - GORM wraps each `Create` in BEGIN/COMMIT.
  - Drizzle on `pg` doesn't prepare statements.
- The Rust core uses compile-time SeaORM entities that mirror the Django tables. This
  matches the decision that schema → IR → engine translation happens at compile time.
- Not measured yet: concurrent load (many in-flight queries), free-threaded Python,
  uvloop, alternative result encodings for JS and Go.

## Full results

Generated by `python bench/report.py bench/results-*.json`. Raw data (median, p95, mean,
iterations) is in `bench/results-<lang>-<transport>.json`. Run everything with
`bench/run_all.sh`.

### python 3.11.15, transport: unix — median ms per call (× = speed-up vs `django-async`)

#### Read N posts

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.43 (1.9×) | 0.86 (1.6×) | 9.72 (1.1×) |
| `django-async` | 0.79 | 1.38 | 10.25 |
| `django-async-dict` | 0.92 (0.9×) | 1.39 (1.0×) | 6.84 (1.5×) |
| `sqla-asyncpg` | 0.91 (0.9×) | 1.12 (1.2×) | 7.75 (1.3×) |
| `sqla-psycopg` | 0.96 (0.8×) | 1.30 (1.1×) | 7.90 (1.3×) |
| `ormcore-obj` | 0.32 (2.5×) | 0.49 (2.8×) | 2.50 (4.1×) |
| `ormcore-dict` | 0.28 (2.9×) | 0.44 (3.2×) | 2.38 (4.3×) |
| `ormcore-sync` | 0.20 (4.1×) | 0.23 (5.9×) | 2.30 (4.5×) |
| `rust-only` | 0.14 (5.9×) | 0.20 (7.0×) | 1.43 (7.2×) |

#### Read N posts + author (JOIN)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.95 (1.4×) | 1.82 (1.2×) | 18.98 (1.0×) |
| `django-async` | 1.30 | 2.16 | 18.57 |
| `django-async-dict` | 1.35 (1.0×) | 2.02 (1.1×) | 12.24 (1.5×) |
| `sqla-asyncpg` | 1.12 (1.2×) | 1.76 (1.2×) | 11.32 (1.6×) |
| `sqla-psycopg` | 1.21 (1.1×) | 2.11 (1.0×) | 11.57 (1.6×) |
| `ormcore-obj` | 0.53 (2.5×) | 0.81 (2.7×) | 4.16 (4.5×) |
| `ormcore-dict` | 0.50 (2.6×) | 0.86 (2.5×) | 4.59 (4.0×) |
| `ormcore-sync` | 0.28 (4.6×) | 0.49 (4.4×) | 4.28 (4.3×) |
| `rust-only` | 0.20 (6.4×) | 0.38 (5.7×) | 3.36 (5.5×) |

#### Bulk insert N posts (one statement)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.93 (1.4×) | 3.99 (1.0×) | 43.14 (1.0×) |
| `django-async` | 1.33 | 3.95 | 41.94 |
| `sqla-asyncpg` | 1.55 (0.9×) | 4.71 (0.8×) | 57.15 (0.7×) |
| `sqla-psycopg` | 1.67 (0.8×) | 5.58 (0.7×) | 75.15 (0.6×) |
| `ormcore-obj` | 0.67 (2.0×) | 1.62 (2.4×) | 12.77 (3.3×) |
| `ormcore-dict` | 0.72 (1.9×) | 1.67 (2.4×) | 13.29 (3.2×) |
| `ormcore-sync` | 0.49 (2.7×) | 1.33 (3.0×) | 13.55 (3.1×) |
| `rust-only` | 0.29 (4.6×) | 1.14 (3.5×) | 12.22 (3.4×) |

#### Insert N posts one at a time (N calls, N commits)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.64 (1.8×) | 38.43 (1.3×) | 801 (1.4×) |
| `django-async` | 1.13 | 48.79 | 1123 |
| `sqla-asyncpg` | 1.58 (0.7×) | 72.86 (0.7×) | 1397 (0.8×) |
| `sqla-psycopg` | 1.73 (0.7×) | 83.41 (0.6×) | 1515 (0.7×) |
| `ormcore-obj` | 0.66 (1.7×) | 32.08 (1.5×) | 654 (1.7×) |
| `ormcore-dict` | 0.69 (1.6×) | 31.27 (1.6×) | 681 (1.6×) |
| `ormcore-sync` | 0.55 (2.0×) | 24.51 (2.0×) | 488 (2.3×) |
| `rust-only` | 0.33 (3.4×) | 14.90 (3.3×) | 352 (3.2×) |

### python 3.11.15, transport: tcp — median ms per call (× = speed-up vs `django-async`)

#### Read N posts

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.50 (2.0×) | 0.96 (1.6×) | 9.51 (1.1×) |
| `django-async` | 0.99 | 1.56 | 10.49 |
| `django-async-dict` | 1.17 (0.9×) | 1.54 (1.0×) | 7.44 (1.4×) |
| `sqla-asyncpg` | 1.36 (0.7×) | 1.82 (0.9×) | 8.85 (1.2×) |
| `sqla-psycopg` | 1.13 (0.9×) | 1.63 (1.0×) | 8.80 (1.2×) |
| `ormcore-obj` | 0.30 (3.3×) | 0.55 (2.8×) | 2.74 (3.8×) |
| `ormcore-dict` | 0.29 (3.4×) | 0.50 (3.1×) | 2.63 (4.0×) |
| `ormcore-sync` | 0.23 (4.3×) | 0.27 (5.9×) | 2.34 (4.5×) |
| `rust-only` | 0.17 (5.9×) | 0.25 (6.2×) | 1.86 (5.6×) |

#### Read N posts + author (JOIN)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 1.02 (1.4×) | 1.93 (1.2×) | 17.81 (1.1×) |
| `django-async` | 1.46 | 2.34 | 19.59 |
| `django-async-dict` | 1.50 (1.0×) | 2.02 (1.2×) | 12.24 (1.6×) |
| `sqla-asyncpg` | 1.82 (0.8×) | 2.39 (1.0×) | 13.12 (1.5×) |
| `sqla-psycopg` | 1.89 (0.8×) | 2.37 (1.0×) | 13.19 (1.5×) |
| `ormcore-obj` | 0.48 (3.0×) | 0.78 (3.0×) | 4.56 (4.3×) |
| `ormcore-dict` | 0.42 (3.5×) | 0.83 (2.8×) | 4.93 (4.0×) |
| `ormcore-sync` | 0.33 (4.4×) | 0.65 (3.6×) | 4.37 (4.5×) |
| `rust-only` | 0.23 (6.4×) | 0.61 (3.8×) | 4.20 (4.7×) |

#### Bulk insert N posts (one statement)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 1.34 (1.3×) | 3.89 (1.2×) | 40.97 (1.2×) |
| `django-async` | 1.70 | 4.49 | 47.28 |
| `sqla-asyncpg` | 1.93 (0.9×) | 4.86 (0.9×) | 53.79 (0.9×) |
| `sqla-psycopg` | 1.91 (0.9×) | 5.49 (0.8×) | 79.52 (0.6×) |
| `ormcore-obj` | 0.85 (2.0×) | 1.71 (2.6×) | 13.87 (3.4×) |
| `ormcore-dict` | 0.82 (2.1×) | 1.58 (2.8×) | 12.43 (3.8×) |
| `ormcore-sync` | 0.70 (2.4×) | 1.44 (3.1×) | 12.38 (3.8×) |
| `rust-only` | 0.68 (2.5×) | 1.48 (3.0×) | 12.55 (3.8×) |

#### Insert N posts one at a time (N calls, N commits)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `django-sync` | 0.86 (1.5×) | 44.71 (1.4×) | 1003 (1.2×) |
| `django-async` | 1.32 | 63.66 | 1231 |
| `sqla-asyncpg` | 1.90 (0.7×) | 91.96 (0.7×) | 1870 (0.7×) |
| `sqla-psycopg` | 2.08 (0.6×) | 102 (0.6×) | 2044 (0.6×) |
| `ormcore-obj` | 0.75 (1.8×) | 39.26 (1.6×) | 779 (1.6×) |
| `ormcore-dict` | 0.84 (1.6×) | 42.32 (1.5×) | 819 (1.5×) |
| `ormcore-sync` | 0.65 (2.0×) | 32.50 (2.0×) | 683 (1.8×) |
| `rust-only` | 0.51 (2.6×) | 29.30 (2.2×) | 635 (1.9×) |

### node 22.22.0, transport: unix — median ms per call (× = speed-up vs `drizzle-pg`)

#### Read N posts

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.36 | 0.50 | 3.32 |
| `ormcore-async` | 0.20 (1.8×) | 0.42 (1.2×) | 3.37 (1.0×) |
| `ormcore-sync` | 0.21 (1.8×) | 0.37 (1.4×) | 3.48 (1.0×) |

#### Read N posts + author (JOIN)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.65 | 1.06 | 7.20 |
| `ormcore-async` | 0.42 (1.5×) | 0.78 (1.4×) | 6.08 (1.2×) |
| `ormcore-sync` | 0.32 (2.1×) | 0.60 (1.8×) | 6.56 (1.1×) |

#### Bulk insert N posts (one statement)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.74 | 3.16 | 50.01 |
| `ormcore-async` | 0.51 (1.4×) | 1.52 (2.1×) | 15.35 (3.3×) |
| `ormcore-sync` | 0.51 (1.4×) | 1.42 (2.2×) | 15.27 (3.3×) |

#### Insert N posts one at a time (N calls, N commits)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.65 | 32.91 | 564 |
| `ormcore-async` | 0.50 (1.3×) | 27.26 (1.2×) | 557 (1.0×) |
| `ormcore-sync` | 0.51 (1.3×) | 25.14 (1.3×) | 494 (1.1×) |

### node 22.22.0, transport: tcp — median ms per call (× = speed-up vs `drizzle-pg`)

#### Read N posts

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.50 | 0.49 | 3.27 |
| `ormcore-async` | 0.25 (2.0×) | 0.51 (1.0×) | 3.98 (0.8×) |
| `ormcore-sync` | 0.23 (2.1×) | 0.29 (1.7×) | 3.41 (1.0×) |

#### Read N posts + author (JOIN)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.63 | 0.88 | 5.62 |
| `ormcore-async` | 0.46 (1.4×) | 0.92 (1.0×) | 6.40 (0.9×) |
| `ormcore-sync` | 0.32 (2.0×) | 0.51 (1.7×) | 5.92 (0.9×) |

#### Bulk insert N posts (one statement)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.95 | 3.19 | 50.33 |
| `ormcore-async` | 0.69 (1.4×) | 1.63 (2.0×) | 15.10 (3.3×) |
| `ormcore-sync` | 0.69 (1.4×) | 1.48 (2.2×) | 13.68 (3.7×) |

#### Insert N posts one at a time (N calls, N commits)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.87 | 31.23 | 654 |
| `ormcore-async` | 0.67 (1.3×) | 33.51 (0.9×) | 752 (0.9×) |
| `ormcore-sync` | 0.64 (1.4×) | 30.29 (1.0×) | 720 (0.9×) |

### bun 1.3.14, transport: unix — median ms per call (× = speed-up vs `drizzle-pg`)

#### Read N posts

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.24 | 0.41 | 2.76 |
| `ormcore-async` | 0.23 (1.1×) | 0.46 (0.9×) | 3.63 (0.8×) |
| `ormcore-sync` | 0.21 (1.2×) | 0.37 (1.1×) | 3.40 (0.8×) |

#### Read N posts + author (JOIN)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.53 | 0.87 | 7.10 |
| `ormcore-async` | 0.42 (1.3×) | 0.77 (1.1×) | 6.63 (1.1×) |
| `ormcore-sync` | 0.28 (1.9×) | 0.83 (1.0×) | 6.49 (1.1×) |

#### Bulk insert N posts (one statement)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.82 | 4.03 | 68.54 |
| `ormcore-async` | 0.58 (1.4×) | 1.54 (2.6×) | 15.84 (4.3×) |
| `ormcore-sync` | 0.50 (1.6×) | 1.34 (3.0×) | 14.38 (4.8×) |

#### Insert N posts one at a time (N calls, N commits)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.80 | 37.48 | 670 |
| `ormcore-async` | 0.57 (1.4×) | 31.10 (1.2×) | 606 (1.1×) |
| `ormcore-sync` | 0.51 (1.6×) | 26.92 (1.4×) | 489 (1.4×) |

### bun 1.3.14, transport: tcp — median ms per call (× = speed-up vs `drizzle-pg`)

#### Read N posts

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.24 | 0.43 | 3.67 |
| `ormcore-async` | 0.27 (0.9×) | 0.46 (0.9×) | 3.80 (1.0×) |
| `ormcore-sync` | 0.23 (1.0×) | 0.34 (1.3×) | 3.68 (1.0×) |

#### Read N posts + author (JOIN)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.63 | 0.85 | 6.17 |
| `ormcore-async` | 0.49 (1.3×) | 0.80 (1.1×) | 6.82 (0.9×) |
| `ormcore-sync` | 0.29 (2.2×) | 0.56 (1.5×) | 6.72 (0.9×) |

#### Bulk insert N posts (one statement)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.90 | 3.96 | 61.27 |
| `ormcore-async` | 0.65 (1.4×) | 1.56 (2.5×) | 13.62 (4.5×) |
| `ormcore-sync` | 0.73 (1.2×) | 1.60 (2.5×) | 17.19 (3.6×) |

#### Insert N posts one at a time (N calls, N commits)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `drizzle-pg` | 0.86 | 37.80 | 827 |
| `ormcore-async` | 0.72 (1.2×) | 39.33 (1.0×) | 768 (1.1×) |
| `ormcore-sync` | 0.82 (1.0×) | 39.30 (1.0×) | 715 (1.2×) |

### go1.25.0, transport: unix — median ms per call (× = speed-up vs `gorm`)

#### Read N posts

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `gorm` | 0.13 | 0.26 | 2.52 |
| `pgx` | 0.11 (1.2×) | 0.12 (2.1×) | 0.90 (2.8×) |
| `ormcore-cgo` | 0.22 (0.6×) | 0.28 (0.9×) | 2.59 (1.0×) |

#### Read N posts + author (JOIN)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `gorm` | 0.18 | 0.39 | 4.84 |
| `pgx` | 0.14 (1.3×) | 0.21 (1.8×) | 1.84 (2.6×) |
| `ormcore-cgo` | 0.28 (0.6×) | 0.43 (0.9×) | 4.34 (1.1×) |

#### Bulk insert N posts (one statement)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `gorm` | 0.66 | 1.56 | 16.68 |
| `pgx` | 0.45 (1.5×) | 1.10 (1.4×) | 12.48 (1.3×) |
| `ormcore-cgo` | 0.53 (1.3×) | 1.41 (1.1×) | 14.75 (1.1×) |

#### Insert N posts one at a time (N calls, N commits)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `gorm` | 0.62 | 25.44 | 482 |
| `pgx` | 0.43 (1.5×) | 14.95 (1.7×) | 291 (1.7×) |
| `ormcore-cgo` | 0.53 (1.2×) | 26.79 (0.9×) | 545 (0.9×) |

### go1.25.0, transport: tcp — median ms per call (× = speed-up vs `gorm`)

#### Read N posts

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `gorm` | 0.11 | 0.23 | 2.79 |
| `pgx` | 0.08 (1.4×) | 0.13 (1.8×) | 1.02 (2.7×) |
| `ormcore-cgo` | 0.24 (0.4×) | 0.28 (0.8×) | 2.78 (1.0×) |

#### Read N posts + author (JOIN)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `gorm` | 0.17 | 0.37 | 4.10 |
| `pgx` | 0.13 (1.3×) | 0.24 (1.6×) | 2.33 (1.8×) |
| `ormcore-cgo` | 0.34 (0.5×) | 0.55 (0.7×) | 4.64 (0.9×) |

#### Bulk insert N posts (one statement)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `gorm` | 0.79 | 1.66 | 15.88 |
| `pgx` | 0.52 (1.5×) | 1.16 (1.4×) | 11.63 (1.4×) |
| `ormcore-cgo` | 0.71 (1.1×) | 1.56 (1.1×) | 13.78 (1.2×) |

#### Insert N posts one at a time (N calls, N commits)

| contender | N=1 | N=50 | N=1000 |
|---|---:|---:|---:|
| `gorm` | 0.75 | 32.84 | 659 |
| `pgx` | 0.50 (1.5×) | 19.29 (1.7×) | 385 (1.7×) |
| `ormcore-cgo` | 0.62 (1.2×) | 34.84 (0.9×) | 602 (1.1×) |

