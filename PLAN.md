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

Do not build a full ORM engine initially. Use SeaORM as the execution engine.

```
Schema DSL → Schema Compiler → ORM IR → Python API → PyO3 → SeaORM Adapter → Database
```

Later replace `ORM IR → SeaORM` with `ORM IR → Custom ORM Engine` without changing the
user-facing API.

---

## Design Rules

### Public API rule

Never expose SeaORM concepts directly. The public API belongs to this project.

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
Python → Rust API (one call) → SeaORM → Database → Rust objects → Python objects (batch)
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

```
model User {
    id: UUID @primary
    email: String @unique
    posts: Post[]
}

model Post {
    id: UUID @primary
    title: String
    author: User
}
```

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

The IR must not mirror SeaORM. It represents universal ORM concepts: Entity, Field,
Relation, Query AST, Mutation AST, Transaction, Migration operations, Database
capabilities. SeaORM is only an execution backend for the MVP.

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

1. **Async first.** The Python API is async (asyncio), bridged to Tokio via
   `pyo3-async-runtimes`, the simplest working approach. Both Django and SQLAlchemy support
   async, so all benchmarks compare async paths.
2. **No codegen for now.** Typed stubs / autocomplete are deferred. A rough API is
   acceptable while checking performance.
3. **Schema → IR → engine translation happens at compile time**, not at runtime: the
   schema is compiled into Rust code (entities) and built. Out of scope for Phase 0.

---

## Roadmap

### Phase 0 — Performance feasibility check  ← current

Question: is `Python → PyO3 → SeaORM → Postgres` competitive with a mature Python ORM
once you count FFI and Python object materialization?

- Django project on PostgreSQL with 3 models (`Author`, `Post`, `Comment`), seeded with
  1000 posts.
- Read and write 1, 50 and 1000 objects through:
  - Django ORM (model instances, and `.values()` dicts)
  - SQLAlchemy 2.0 ORM
  - PyO3 + SeaORM (returning Python dicts, and `#[pyclass]` objects)
  - Pure Rust SeaORM (no Python, as a lower bound)
- Also measure the "bad" pattern: N separate single-row calls across the FFI boundary.
- Deliverable: `bench/RESULTS.md`.

### Phase 1 — First prototype

1. Schema parser
2. ORM IR
3. Python API prototype
4. PyO3 binding
5. SeaORM adapter
6. PostgreSQL support

Avoid initially: multiple databases, full migration engine, advanced ORM features,
multiple language bindings. Validate the architecture and API first.

### Later

Node/Bun binding, migrations engine, more databases, extension system, custom engine
replacing SeaORM.

---

## Main Risks

- **Semantic complexity:** relations, loading strategies, transactions, inheritance,
  caching, migrations, database-specific features.
- **Ecosystem adoption:** Django ORM, SQLAlchemy, Prisma and SeaORM are mature. The
  project must provide clear advantages.
