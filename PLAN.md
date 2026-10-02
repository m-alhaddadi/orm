# Universal ORM — Plan

## Vision

Build a language-independent ORM platform:

- Define database models once using a custom schema language.
- Provide native ORM APIs for multiple languages.
- Use Rust as the core implementation.
- Allow replacing internal engines without changing user-facing APIs.

The goal is not to generate separate ORMs for every language, but to build **one ORM
system with multiple language frontends**. The API layer is the product; the database
engine is replaceable.

---

## Architecture

### Long-term

```
              Schema DSL
                  |
            Schema Compiler
                  |
             ORM IR Layer
                  |
       +----------+----------+----------+
       |          |          |          |
 Python API   Node/Bun API  Rust API   (WASM, when required)
       |          |
     PyO3    napi addon
       |          |
       +----------+
            |
     ORM Runtime Core
            |
         Database
```

The ORM core owns: query building, relations, migrations, schema metadata,
transactions, database features, extensions. Language bindings provide the native
developer experience.

### MVP

Do not build a full ORM engine initially. The MVP started on SeaORM as the execution
engine; by the end of Phase 1 the planner and drivers are ours and SeaORM is gone:

```
Schema DSL → Schema Compiler → ORM IR → Python API → PyO3 → planner (sea-query) → driver → Database
```

sea-query stays as the SQL builder (one builder per dialect), and each database gets a
driver behind one trait (`native/src/db/`); tokio-postgres for Postgres today.

---

## Design Rules

### Public API rule

Never expose engine or driver concepts directly. The public API belongs to this project.

```python
User.where(User.email == email).first()     # yes
Entity.find().filter(Column.Email.eq(...))  # no
```

### FFI strategy

- Minimize boundary crossings: one Python → Rust call per query / mutation.
- Prefer batch conversion of results (Rust → Python objects in one pass).
- Avoid row-by-row Rust ↔ Python transfers.

Good data flow:

```
Python → Rust API (one call) → planner → driver → Database → Python objects (batch)
```

Bad: `Python → Rust → Python` repeatedly; one FFI conversion per row.

### Performance assumptions (to be validated — see Phase 0)

- FFI overhead is usually small.
- Object materialization is the expensive part.
- Database latency dominates most ORM workloads.

---

## Schema Language

Describes database concepts only: models/tables, columns, types, relations, indexes,
constraints, triggers, database-specific features, inheritance.

```prisma
model User {
  id    String @id @db.Uuid
  email String @unique
  posts Post[]
}

model Post {
  id        String @id @db.Uuid
  title     String
  author_id String @db.Uuid
  author    User   @relation(fields: [author_id], references: [id])
}
```

The syntax is Prisma's (see [`docs/prisma-syntax.md`](docs/prisma-syntax.md)).

### Separation of concerns

| Schema DSL owns            | Application code owns            |
|----------------------------|----------------------------------|
| Database structure         | Validators                       |
| Relational model           | Business logic                   |
| Migrations                 | Methods                          |
| Constraints, indexes       | Computed properties              |
| Database behavior          | Framework-specific behavior      |

Do not embed Python, Rust, or TypeScript code inside the schema language.

### Extension system

```
schema/
    models.schema
extensions/
    postgres/     # JSONB, PostGIS, RLS, custom indexes, triggers
    django/       # Django metadata
    sqlalchemy/   # SQLAlchemy options
    python/       # language-specific helpers
```

---

## ORM Intermediate Representation (IR)

The IR must not mirror any engine. It represents universal ORM concepts: Entity, Field,
Relation, Query AST, Mutation AST, Transaction, Migration operations, Database
capabilities (`orm_core::dialect`).

---

## Python Support

`Python → native Python API → PyO3 → Rust ORM runtime` (async; see Decisions)

Requirements: Pythonic API, type hints, IDE autocomplete, async support, native
exceptions.

## Code Generation

Optional. Used for model definitions, type hints (`.pyi`), IDE support. Not required for
execution — the runtime stays in Rust.

---

## Decisions

1. **Async only.** The Python API is async (asyncio), bridged to Tokio via
   `pyo3-async-runtimes`; JS will be async too. No sync API: it would double every
   terminal method (Django's `get` / `aget` split) for one benefit, skipping the
   asyncio ↔ Tokio hand-off (~115 µs per call in Phase 0). If that cost matters, make the
   bridge cheaper (complete the Python future from Tokio directly) instead.
2. **No codegen for now.** Typed stubs / autocomplete are deferred. A rough API is
   acceptable while checking performance.
3. **Schema → IR → engine translation happens at compile time**, not at runtime: the
   schema is compiled into Rust code (entities) and built. Out of scope for Phase 0.

---

## Roadmap

### Phase 0 — Performance feasibility check  ✅ done, see [`bench/RESULTS.md`](bench/RESULTS.md)

Question: is `Python → PyO3 → SeaORM → Postgres` competitive with a mature Python ORM
once you count FFI and Python object materialization?

- All async (asyncio). Django project on PostgreSQL with 3 models (`Author`, `Post`, `Comment`), seeded with
  1000 posts.
- Read and write 1, 50 and 1000 objects through:
  - Django ORM (model instances, and `.values()` dicts)
  - SQLAlchemy 2.0 ORM
  - PyO3 + SeaORM (returning Python dicts, and `#[pyclass]` objects)
  - Pure Rust SeaORM (no Python, as a lower bound)
  - Node and Bun (napi-rs addon vs Drizzle ORM) and Go (cgo vs GORM and pgx)
- Also measure the "bad" pattern: N separate single-row calls across the FFI boundary.
- Deliverable: `bench/RESULTS.md`.
- Outcome: one Rust core now runs from Python (PyO3), Node and Bun (napi-rs) and Go (cgo).
  It is 2–4.5× faster than Django / SQLAlchemy, up to 4.8× faster than Drizzle (biggest on writes) except
  even on large reads, and level with GORM (pgx is faster). sqlx's missing `TCP_NODELAY`
  is fixed with a vendored patch. Sync calls skip the event-loop hand-off (Python ~115 µs,
  JS ~40 µs per call), so ship async as the primary API plus a sync API.

### Phase 1 — First prototype  (in progress, see [`docs/python-api.md`](docs/python-api.md))

1. Schema parser — ✅ `.prisma` schema language (Prisma syntax + our attributes), compiled by `core/` (see below)
2. ORM IR — ✅ first cut: schema IR + query/mutation IR (`core/src/ir.rs`)
3. Python API prototype — ✅ `python/orm`: Django-style managers and loading,
   SQLAlchemy-style typed expressions, relation-path filters (`User.posts.created_at`), subqueries
   (`exists()`, scalar, `outer()`), window functions, CTEs (recursive, subqueries in
   `FROM`), nested / filtered / per-parent-sliced prefetch, instances built in Rust
4. PyO3 binding — ✅ `native/` (`orm._native`), one call per operation
5. Engine — ✅ IR → sea-query planner; first on SeaORM's pool, now our own driver
   layer (`native/src/db/`, tokio-postgres) with per-dialect capabilities, transactions,
   savepoints, row and advisory locks
6. PostgreSQL support — ✅ (only backend)

Avoid initially: multiple databases, full migration engine, advanced ORM features,
multiple language bindings. Validate the architecture and API first.

### Schema language, migrations, extensions  ✅ first cut, see [`docs/schema.md`](docs/schema.md)

- One schema for every language: `.prisma` files, parsed and compiled by the binding-free
  `core/` crate (`orm-core`) into the schema IR. Python loads the IR (`orm.load`) or a
  module generated from it (`models.py` + typed `.pyi`). The JS binding will consume
  the same IR. The `orm` CLI (compile, check, generate, makemigrations, sqlmigrate)
  needs no Python.
- Schema objects: indexes (expression, partial, covering, any method, opclasses,
  storage params), unique / check / exclusion constraints, FK `on_update` /
  deferrable, triggers and functions, comments, UUID and JSON columns, rename hints.
- Extensions are TOML files (`core/extensions/postgres/*.toml`; custom ones are
  `import`ed): types (SQL template, value conversion, `read` / `write` SQL, per-language
  type hints), index methods, opclasses and functions. Using any of them makes the
  migration `CREATE EXTENSION` it.
- Migration generator: schema → database snapshot → diff against the last migration's
  snapshot → Postgres DDL, up and down, with warnings. The runner (checksums, advisory
  lock) is in Python for now; `sqlx::migrate` is the candidate for a shared Rust one.
  SeaORM's and Refinery's tools were not used (see the doc).

### Next: JS / TypeScript binding (deferred)

1. Split the engine out of the Python binding: `engine/` (`orm-engine`: planner + drivers,
   neutral parameter values, typed row accessors) with thin `bindings/python` (PyO3) and
   `bindings/node` (napi-rs; Node, Bun, Deno). No behaviour change; Python tests guard it.
2. TypeScript codegen from the same schema and the same API shape: methods instead of
   operators (`.eq() .lt()`), `select({ name: expr })` object rows, `AsyncLocalStorage`
   for the current transaction. Tests on Node and Bun, `tsc` type checks, a benchmark
   against Drizzle and Prisma.

Open questions: camelCase field names in TS (recommended); `BigInt` columns as `number`
with an error past 2^53 (recommended) vs `bigint`; `Date` for timestamps (ms precision)
vs waiting for `Temporal`; `await qs` (recommended, like Python) vs `.execute()`.

### Later

More databases (a dialect + a driver each), introspection / drift
detection.

---

## Main Risks

- **Semantic complexity:** relations, loading strategies, transactions, inheritance,
  caching, migrations, database-specific features.
- **Ecosystem adoption:** Django ORM, SQLAlchemy, Prisma and SeaORM are mature. The
  project must provide clear advantages.
