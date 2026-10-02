# Phase 0 — performance feasibility benchmark

Can `Python (asyncio) → PyO3 → SeaORM → Postgres` compete with Django and SQLAlchemy once
FFI and Python object materialization are counted? Results: [`RESULTS.md`](RESULTS.md).

## Layout

| Path | What |
|---|---|
| `django_bench/` | Django 5.2 project, app `blog` with `Author`, `Post`, `Comment`; `manage.py seed` |
| `ormcore/` | Rust `cdylib` (PyO3 0.29 + SeaORM 2.0 + `pyo3-async-runtimes`) exposing an async client |
| `sa_models.py` | SQLAlchemy 2.0 mapping of the same tables |
| `run_bench.py` | Benchmark harness, writes `results-<transport>.json` |
| `report.py` | Renders result JSON as markdown tables |

## Reproduce

```bash
# Postgres 16 with user postgres/postgres; local socket auth must allow passwords
# (pg_hba.conf: `local all all scram-sha-256`).
sudo service postgresql start
psql -U postgres -c "CREATE DATABASE ormbench"

uv venv .venv && . .venv/bin/activate
uv pip install "django>=5" "psycopg[binary]" "sqlalchemy[asyncio]>=2" asyncpg maturin

(cd bench/django_bench && python manage.py migrate && python manage.py seed)
(cd bench/ormcore && maturin develop --release)

python bench/run_bench.py --transport unix     # all drivers over the Unix socket
python bench/run_bench.py --transport tcp      # all drivers over localhost TCP
python bench/report.py bench/results-unix.json bench/results-tcp.json
```

`--quick` runs 10% of the iterations; `--only ormcore-obj,django-async` picks contenders.

## Methodology

- Seed data: 50 authors, 1000 posts, 2000 comments. Reads use `ORDER BY id LIMIT N`.
- Everything except `django-sync` runs inside one asyncio event loop and is timed around the
  `await` with `perf_counter_ns`. Each contender has one pooled connection.
- Each timed read **touches every field of every row**, including `author.*` for joins.
- Writes: rows are prepared as plain dicts (untimed). Building ORM objects from them is
  timed. Rows inserted are deleted after every iteration (untimed), and the table is
  vacuumed before each operation.
- `write_loop` = N separate inserts, each its own call and autocommit, which exercises
  the "row-by-row across the FFI" pattern.
- `rust-only` runs the same SeaORM queries timed inside Tokio. It builds no Python
  objects and doesn't hop through the event loop, so it's the floor the PyO3 path
  approaches.
- Default Postgres durability (`synchronous_commit=on`, `fsync=on`).
