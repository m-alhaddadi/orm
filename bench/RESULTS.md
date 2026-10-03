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


## Engine: SeaORM vs tokio-postgres

Phase 1 replaced SeaORM (sqlx underneath) with our own driver layer on tokio-postgres.
`bench/engine_bench.py` times the public Python API on the blog models (1000 posts, 10
users), release builds, localhost TCP, `sslmode=disable`, best of two interleaved runs,
median per run. Microseconds per operation.

| case | SeaORM / sqlx | tokio-postgres | change |
|---|---:|---:|---:|
| get by pk | 593 | 399 | −33% |
| read 50 | 805 | 645 | −20% |
| read 1000 | 2671 | 2545 | −5% |
| read 1000 + select_related | 5825 | 4813 | −17% |
| 10 users + prefetch 1000 posts | 4321 | 3718 | −14% |
| count with EXISTS filter | 744 | 551 | −26% |
| insert 1 | 915 | 832 | −9% |
| insert_many 50 | 1694 | 1724 | +2% |
| update 100 rows | 1632 | 1248 | −24% |
| transaction, 2 updates | 2348 | 2026 | −14% |
| 10 concurrent gets | 2594 | 2243 | −14% |

The machine (4-core VM) has a high floor: a bare `SELECT 1` through the whole stack
takes ~210 µs. With the default `sslmode=prefer` both engines use TLS, which adds ~80 µs
per query here and hides the difference: in a single TLS run the cases ranged from 9%
slower to 8% faster, within this machine's noise.

## Instances built in Rust

`afe76ab` moved building result objects (instances, `select_related` objects,
prefetched lists, `Row`s) from Python into Rust and added subqueries, window functions,
CTEs and nested / filtered prefetch; `e5f12ce` added shared windows and CTE joins.
`bench/engine_bench.py` on each commit (release builds side by side, localhost TCP,
`sslmode=disable`), three interleaved rounds, best of the three medians. Microseconds per
operation.

| case | `4ca5514` (before) | `afe76ab` | `e5f12ce` | change |
|---|---:|---:|---:|---:|
| get by pk | 433 | 450 | 378 | noise |
| read 50 | 624 | 559 | 560 | −10% |
| read 1000 | 2273 | 1882 | 1949 | −14% to −17% |
| read 1000 + select_related | 4266 | 3138 | 3358 | −21% to −26% |
| 10 users + prefetch 1000 posts | 3545 | 3160 | 3218 | −9% to −11% |
| count with EXISTS filter | 657 | 628 | 687 | noise |
| insert 1 | 796 | 758 | 798 | noise |
| insert_many 50 | 1448 | 1439 | 1524 | noise |
| update 100 rows | 1377 | 1394 | 1363 | noise |
| transaction, 2 updates | 2428 | 2330 | 2304 | noise |
| 10 concurrent gets | 2290 | 2212 | 2231 | noise |

The gain is in reads that build many objects; operations that return a count or one row
don't change. Between `afe76ab` and `e5f12ce` the table suggests reads got a few percent
slower, but a focused A/B of those two builds (four alternating rounds) put them level:
read 1000 at 1806–1894 vs 1794–1909 µs, `select_related` at a median ~3020 vs ~3080 µs,
inside the spread of either build; building the IR and planning costs ~15 µs in both.
`count with EXISTS filter` swings between ~450 and ~750 µs on this machine within one
build.


## Where a query's time goes

`bench/profile_query.py` (with the probes in `bench/profile_probes.patch`, which are not
part of the shipped module) times each stage of one query on the blog models: release
build, localhost TCP, `sslmode=disable`, 4-vCPU VM, Postgres on the same host. Medians,
µs. Runs vary by ±10–15% on this machine; the shares are stable.

### `get by pk` today, ~300–340 µs (asyncio)

| stage | measured alone | inside the real loop |
|---|---:|---:|
| Python: query set, IR dicts, `json.dumps` | 12 | 32–38 |
| Rust: parse IR, plan, render SQL (+ creating the future) | 4 | 25 |
| asyncio ↔ Tokio hand-off (`future_into_py`) | 120 | ~110 |
| Postgres round trip (prepared statement, cached per connection) | 82–94 | 82–94 |
| decode row, build the instance, coroutine layers | — | ~40 |

Stages cost 3–5× more inside the loop than alone: between two queries the CPU runs the
Tokio worker and the Postgres backend, so the Python and planner code runs on cold
caches. Estimates below use the in-loop numbers.

### The hand-off is the largest cost we control

| | asyncio | uvloop |
|---|---:|---:|
| no-op async call, `future_into_py` (today) | 120 | 64 |
| no-op async call, eventfd completion (prototype) | 50 | 44 |
| `SELECT 1`, Rust loop with no Python | 82 | 87 |
| `SELECT 1`, blocking call from Python | 79 | 81 |
| `SELECT 1`, async, `future_into_py` (today) | 181 | 122 |
| `SELECT 1`, async, eventfd completion (prototype) | 103 | 101 |

`future_into_py` finishes the asyncio future from the Tokio thread. That thread has to
take the GIL from the event loop thread and then wake it through the loop's self-pipe,
which costs several thread wake-ups. In the prototype the Tokio task writes to an
eventfd that the loop watches (`loop.add_reader`), and the loop picks up the result on
its own thread. With that change an async call costs the same as a blocking one.
uvloop by itself also removes about half of the cost.

### Plan cache: measured A/B

`_probe_run_cached` stores the SQL, arguments and output shape under the IR JSON key,
which is the upper bound of a Rust-side plan cache. A "prepared query" builds the IR
once and reuses it. Interleaved runs:

| | get by pk, asyncio | read 50 + 2 filters, asyncio | get by pk, uvloop | read 50 + 2 filters, uvloop |
|---|---:|---:|---:|---:|
| today, end to end | 343 | 518 | 201 | 400 |
| prepared query (IR built once) | 229 | 383 | 154 | 274 |
| prepared query + Rust plan cache | 206 | 324 | 137 | 227 |

The Rust plan cache alone saves 17–58 µs per query (7–15%): parse, plan and render
take 4 µs in a microbenchmark, but more on cold caches. Skipping the Python IR build
saves more than that (65–135 µs). Postgres already caches its own plans, because
statements are prepared once per connection.

### Schema JSON at runtime: a startup cost only

| schema | `json.loads` | `define()` (Python classes) | `json.dumps` | Rust parse + index |
|---|---:|---:|---:|---:|
| blog, 6 models, 9 KB | 0.06–0.18 ms | 0.65–0.85 ms | 0.12–0.17 ms | 0.12–0.14 ms |
| 204 models, 277 KB | 1.2–1.5 ms | 9.9–10.5 ms | 2.4 ms | 1.2–1.5 ms |

This cost is paid once per process. Queries never touch the JSON: `Schema::from_ir`
indexes models, fields and relations into hash maps, and the planner reads those.
Creating the Python classes costs more than parsing the JSON. Compiling the schema into
Rust would save about 5 ms of startup on a 200-model schema and nothing per query.

### Prepared queries

`qs.prepare()` (built once, values bound per call) against the same query built per
call. The two variants alternate (21 rounds of 200 calls each), release build, same
machine. Median, µs.

| case | asyncio | prepared | uvloop | prepared |
|---|---:|---:|---:|---:|
| get by pk | 305 | 239 (−22%) | 214 | 166 (−22%) |
| read 50 + 2 filters | 498 | 344 (−31%) | 306 | 266 (−13%) |

`bench/engine_bench.py` now has prepared cases. Sequential runs of it on this machine
swing by ±20% (an unchanged "read 50" went from 313 to 509 µs between runs), so the
table above uses alternating runs.


## Current Rust engine versus SeaORM — 2026-10-03

Direct Rust comparison of the current `orm-engine` against unmodified SeaORM
2.0.4 / SQLx 0.9.0. This measures the current planner and database driver,
separately from the older SeaORM-based `ormcore` prototype above.

Environment: macOS ARM64, Rust 1.97.0, release build with LTO, isolated PostgreSQL
18.6 (Homebrew), localhost TCP with TLS disabled. Each ORM has one pooled
connection and runs on the same current-thread Tokio runtime. PostgreSQL uses
default durability settings.

Median batch-average latency in **µs per operation**; lower is better.
“Speed ratio” is SeaORM latency divided by our direct-IR latency.

| Case | SeaORM | Our direct IR + planner | Our JSON parse + planner | Speed ratio |
|---|---:|---:|---:|---:|
| get by pk | 70.87 | 37.26 | 37.89 | 1.90× |
| read 50 | 95.46 | 55.63 | 56.39 | 1.72× |
| read 1000 | 383.33 | 306.26 | 306.49 | 1.25× |
| filtered count | 105.76 | 62.26 | 63.98 | 1.70× |
| update 100 | 138.94 | 106.32 | 107.40 | 1.31× |

Our engine had lower median latency in all five measured cases: **1.25–1.90×**
the operation rate implied by SeaORM's latency in this sequential workload.
Skipping JSON parsing reduced median latency by only **0.23–1.73 µs** in these
cases. These results do not demonstrate a benefit from generated Rust entities
or precompiled SQL; neither is implemented in this comparison.

Method: 1,000 seeded rows with two integer fields, a title, and a 200-byte body.
Both implementations bind query values and decode every selected field into
exactly the same owned Rust model. The harness asserts complete result equality
and expected counts before timing. Each contender receives 30 warm-up calls,
followed by 15 batches of 20–150 operations; contender order rotates each round.
Connections, schema compilation, and seeding are outside timing. Query building,
planning, execution, result allocation, and result destruction are inside timing.
The direct-IR path clones a typed operation template for each call; the JSON path
parses a prepared JSON string per call, without timing JSON serialization.

The update case sets `views = 0` on 100 rows and returns the affected-row count.
Its batch averages varied considerably: SeaORM **135.49–191.44 µs**, direct IR
**101.14–157.68 µs**, and JSON IR **103.96–159.57 µs**. The primary-key case also
had a SeaORM outlier of **145.24 µs** versus its **70.87 µs** median. Reported
values are medians of batch averages, not per-request tail latencies.

Scope: warm-cache sequential operations against a local database. The comparison
includes different drivers (`tokio-postgres` versus SQLx), so it cannot attribute
the difference to the planner alone. It does not cover joins, prefetching,
inserts, concurrent load, remote databases, or production workloads.

Harness and methodology: [rust-compare/README.md](rust-compare/README.md).
Raw batch measurements: [rust-compare/results.json](rust-compare/results.json).


### Completed verification — three independent runs

Completed two additional release runs against the isolated PostgreSQL instance,
with a fresh table and seed data for each run. The initial measurements above
and their raw JSON remain unchanged. All three runs completed all five cases
and all correctness assertions: **63,450 timed operations** in total, plus
untimed warm-up and validation calls.

The verification runs additionally assert the exact seeded ID, title, body,
and view count of every returned row. For each of the three update paths, the
harness resets `views = id`, runs the update, and independently checks that
exactly IDs 1–100 have `views = 0` while IDs 101–1000 retain their original views.
These checks and resets are outside timing.

The following latency values are the **median of the three run medians**, in
µs per operation. The final column gives the range of SeaORM/direct-IR latency
ratios across those runs, rather than a confidence interval.

| Case | SeaORM | Our direct IR + planner | Our JSON parse + planner | Speed ratio across runs |
|---|---:|---:|---:|---:|
| get by pk | 71.45 | 37.26 | 37.89 | 1.90–1.92× |
| read 50 | 95.36 | 55.63 | 56.13 | 1.70–1.72× |
| read 1000 | 387.77 | 305.26 | 305.16 | 1.25–1.30× |
| filtered count | 102.63 | 62.64 | 63.29 | 1.63–1.70× |
| update 100 | 140.14 | 107.13 | 107.40 | 1.22–1.31× |

Our direct Rust path had lower median latency in **every case in every run**.
The observed speed ratios span **1.22–1.92×**. The 100-row update remained the
most variable case: run medians were 138.94, 179.91, and 140.14 µs for SeaORM,
and 106.32, 147.15, and 107.13 µs for direct IR. Both implementations slowed
in the same repeat run; the measurements do not isolate the cause. Read timing
was steadier. The small direct-IR versus JSON differences include measurement
noise; JSON was marginally faster in some cases in the last run.

Validation completed: release compilation, Rust formatting check, Clippy on
the benchmark crate with warnings treated as errors, report whitespace check,
and all runtime result/state assertions in both verification runs. There were
no benchmark failures. The same workload and driver limitations stated above
still apply; these checks do not establish a universal performance ranking.

Additional raw measurements:
[repeat 1](rust-compare/results-repeat-1.json),
[repeat 2](rust-compare/results-repeat-2.json).
The harness accepts `ORM_BENCH_OUT` to preserve each run in a separate file.


### Diesel added — 2026-10-03

Expanded the Rust comparison with **Diesel 2.3.13 / diesel-async 0.9.2**, using
its typed query DSL and `AsyncPgConnection`. Diesel uses a deadpool pool limited
to one connection, with fast recycling. Both Diesel's async adapter and our
engine use `tokio-postgres`; SeaORM uses SQLx. All contenders run in the same
process, on the same current-thread Tokio runtime, against the same isolated
PostgreSQL 18.6 server and seeded data. This tests Diesel's async adapter,
not its synchronous libpq connection implementation.

Ran the complete expanded comparison **three times**, recreating and seeding
the table for each run. Four contenders now rotate through **16 batches** per
case, giving each contender each execution-order position four times.
The three runs contain **90,240 timed operations**, plus warm-up and validation.
All contenders decode reads into the identical owned Rust model; Diesel derives
`Queryable` on the same struct used by SeaORM and our engine. All read-result,
expected-field, affected-row-count, and independent update-state checks passed.

Latencies below are the **median of the three run medians**, in µs per operation.
These are fresh measurements for all contenders, not Diesel measurements merged
with the earlier three-contender timings. Lower is better.

| Case | SeaORM | Our direct IR + planner | Our JSON parse + planner | Diesel async |
|---|---:|---:|---:|---:|
| get by pk | 71.58 | 37.35 | 38.08 | 36.47 |
| read 50 | 95.77 | 55.74 | 56.01 | 55.60 |
| read 1000 | 388.76 | 302.57 | 302.05 | 262.08 |
| filtered count | 102.48 | 62.43 | 62.75 | 61.48 |
| update 100 | 185.15 | 152.52 | 152.69 | 191.00 |

**Measured outcome:** Diesel and our engine are close for primary-key lookup,
50-row reads, and the filtered count. Across runs, Diesel's latency was about
2–4% lower for lookup, within 0.4% of our engine for 50-row reads, and about
1–4% lower for count. Those small differences should not be treated as a broad
performance advantage.

For 1,000-row reads, Diesel was consistently faster: its latency was **13–15%
lower**, equivalent to about **1.15–1.18×** our operation rate in this sequential
workload. For the 100-row update, our direct-IR path was consistently faster:
its latency was **20–26% lower**, equivalent to about **1.25–1.36×** Diesel's
operation rate. Our engine continued to have lower median latency than SeaORM
in every case in every run.

Write timings remain variable. Diesel's update run medians were **191.00,
193.51, and 144.45 µs**; our direct-IR medians were **152.52, 155.28, and
106.21 µs**. These results therefore support a workload-specific comparison,
not a universal winner. They do not isolate SQL generation, model decoding,
connection-pool behavior, or driver overhead, and still exclude joins, inserts,
concurrent load, remote databases, and production workloads.

Validation: release compilation, formatting, benchmark Clippy with warnings
treated as errors, report whitespace checks, and all runtime assertions passed.
Earlier report sections and raw measurement files were preserved.

Raw expanded-comparison results:
[run 1](rust-compare/results-diesel-1.json),
[run 2](rust-compare/results-diesel-2.json),
[run 3](rust-compare/results-diesel-3.json).
Implementation: [rust-compare/src/main.rs](rust-compare/src/main.rs).
Adapter reference: [diesel-async documentation](https://docs.rs/diesel-async/latest/diesel_async/).


### Diesel investigation and pool-setting correction — 2026-10-03

**Correction to the preceding comparisons:** SeaORM was using its default
`test_before_acquire = true`, which performs a connection-health ping for each
pool checkout. Our engine and Diesel used fast recycling. The reported timings
remain measurements of those configurations, but their speed ratios also
include this health-check difference and overstate the advantage attributable
to ORM/driver implementation alone. The historical “SeaORM / sqlx” driver-swap
measurements were full-stack measurements through SeaORM and the Python API;
they did not establish a ranking against raw SQLx. Earlier sections and raw
files have been preserved; use the controlled results below for this distinction.

Both libraries document the default ping behavior:
[SeaORM ConnectOptions](https://docs.rs/sea-orm/latest/sea_orm/struct.ConnectOptions.html),
[SQLx PoolOptions](https://docs.rs/sqlx/latest/sqlx/pool/struct.PoolOptions.html).

#### Full ORM comparison with matched health-check settings

Repeated all five cases three times with SeaORM's per-checkout ping disabled,
matching the fast-recycling policy of our engine and Diesel. Pool size, database,
query results, runtime, correctness checks, and rotating batch order remain as
in the expanded comparison. This is another **90,240 timed operations**.
Values are medians of three run medians, in µs per operation.

| Case | SeaORM, fast checkout | Our direct IR + planner | Our JSON parse + planner | Diesel async |
|---|---:|---:|---:|---:|
| get by pk | 56.53 | 37.04 | 37.74 | 35.52 |
| read 50 | 78.11 | 55.21 | 55.18 | 55.09 |
| read 1000 | 362.57 | 303.32 | 303.12 | 262.96 |
| filtered count | 90.76 | 62.23 | 62.29 | 61.59 |
| update 100 | 171.26 | 151.65 | 151.75 | 188.52 |

Our engine still had lower median latency than SeaORM in all five cases in each
matched run. The ratios are smaller than with default health checks: primary-key
lookup is about 1.5×, rather than the earlier 1.9×. Diesel remains faster for
1,000-row reads, small reads/counts remain close, and our update remains faster
than Diesel. This correction does not reverse those workload-specific outcomes.

Matched-setting raw results:
[run 1](rust-compare/results-matched-1.json),
[run 2](rust-compare/results-matched-2.json),
[run 3](rust-compare/results-matched-3.json).
Set `ORM_BENCH_SEA_FAST=1` in the original harness to select this configuration;
the output now records `seaorm_test_before_acquire` explicitly.

#### Read-path diagnosis, including raw SQLx

Added [src/bin/diagnose.rs](rust-compare/src/bin/diagnose.rs) to separate planner
cost, runtime cell decoding, raw-row buffering, and checkout pings. Nine variants
read identical seeded rows into the same owned Rust model, with one pooled
connection per variant. Raw SQLx uses `query_as` with a typed `FromRow` that reads
columns by index; it does not use the compile-time `query!` macro. The prebuilt
SQL variants bind the same limit and use cached prepared statements.

Each variant passes full expected-row equality checks. Eighteen batches rotate
execution order, so each variant occupies each position twice. Three clean runs
measure **160,380 database operations**, plus warm-up and separate CPU tests.
The first diagnostic pass overlapped compilation and is retained only as
`results-diagnose-calibration.json`; it is excluded from these results. Clean
runs started after compilation and Clippy finished, and ran sequentially.

Median of three run medians, in µs per operation:

| Read path | 1 row | 50 rows | 1,000 rows |
|---|---:|---:|---:|
| Our direct IR + planner | 40.53 | 61.48 | 305.36 |
| Our driver, prebuilt SQL | 38.72 | 59.43 | 298.88 |
| tokio-postgres, typed buffered rows | 38.32 | 59.12 | 296.52 |
| tokio-postgres, typed streamed rows (prototype) | 38.25 | 58.78 | 247.84 |
| Diesel async | 39.42 | 61.03 | 253.14 |
| Raw SQLx, default health checks | 74.86 | 99.69 | 335.67 |
| Raw SQLx, fast checkout | 57.65 | 83.17 | 314.18 |
| SeaORM, default health checks | 77.11 | 101.58 | 385.02 |
| SeaORM, fast checkout | 60.06 | 83.92 | 369.80 |

For these ordered 1,000-row reads, Diesel has lower latency than both our
current engine and the raw SQLx `query_as` path, even after matching health-check
settings. Raw SQLx is substantially closer to our engine than SeaORM is. This is
not a general ranking across SQLx APIs or production workloads.

The default health checks add about 16–21 µs in this read-only diagnostic when
comparing run-level aggregate medians. This accounts for part of the earlier
gap; other driver and ORM costs remain. Ordered one-row reads here are a
separate query from the primary-key lookup in the full comparison, so their
absolute timings should not be merged.

#### What we can learn from Diesel

Source inspection of diesel-async 0.9.2 shows that its PostgreSQL
`load_prepared` calls `query_raw`, wraps each arriving row, and maps the stream
through `U::build_from_row` before collecting the final models. Our PostgreSQL
`query` first calls `Client::query` to collect a `Vec<Row>` and returns a boxed
`RowSet`; the caller then builds a second vector of models through runtime
`Cell` conversion. Diesel's `Queryable` supplies the result shape and field
conversions through Rust types.

Relevant source:
[Diesel async row-to-model mapping](https://docs.rs/diesel-async/latest/src/diesel_async/run_query_dsl/mod.rs.html),
[Diesel async PostgreSQL execution](https://docs.rs/diesel-async/latest/src/diesel_async/pg/mod.rs.html),
[our PostgreSQL driver](../engine/src/db/postgres.rs).

The controlled buffered-versus-streamed tokio-postgres pair uses the same pool,
SQL, prepared-statement cache, row decoder, capacity for the final model vector,
and owned result type. Only collection of the intermediate raw rows changes.
At 1,000 rows, its median drops from **296.52 to 247.84 µs**: **48.68 µs / 16.4%**
lower latency. The streamed prototype is close to Diesel's **253.14 µs**. This
supports streaming directly into final results as the strongest measured
opportunity. The experiment does not separate reduced buffering/allocation from
changes in when decoding work happens while receiving rows.

The separate CPU-only test repeatedly decodes already fetched 1,000-row sets,
including model allocation and destruction, without database I/O. Runtime
`RowSet::cell` decoding takes **56.11 µs** versus **48.18 µs** for typed
`tokio_postgres::Row::try_get`: a smaller **7.93 µs** difference. This identifies
an opportunity for a reusable decoder chosen from the output schema, while
retaining support for dynamic models and extension types.

Skipping our planner entirely changes the diagnostic median from **305.36 to
298.88 µs**, only **6.48 µs**. Precompiling every query is therefore a lower
priority for this workload. Diesel also has a typed query identity for cached
prepared statements; our engine already caches prepared statements by SQL and
parameter types. Inspection of this diesel-async version shows that it still
renders SQL before the cache lookup, so its result should not be explained as
all SQL having been precompiled at build time.

**Recommended implementation order:** prototype an internal query/row-consumer
API that materializes final results from the incoming stream; keep the buffered
path available where prefetching or consumers require a row set. Then measure
a reusable schema-driven decoder. A generated typed Rust decoder can be an
additional native-Rust optimization. These prototypes validate a direction;
the current production engine has not been replaced by the diagnostic path.

Release builds, formatting, Clippy with warnings treated as errors, result/state
assertions, and report whitespace checks passed. These diagnostics cover simple
sequential local reads, not joins, transactions, inserts, concurrency, remote
latency, or every SQLx execution API.

Diagnostic raw results:
[run 1](rust-compare/results-diagnose-1.json),
[run 2](rust-compare/results-diagnose-2.json),
[run 3](rust-compare/results-diagnose-3.json).
