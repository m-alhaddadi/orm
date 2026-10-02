# Schema objects, extensions and migrations

Models describe columns and relations (see [`python-api.md`](python-api.md)). Everything
else the database holds is declared next to them: indexes, constraints, triggers,
functions and extensions. Migrations are generated from that description, written as
plain SQL, and applied by a small runner.

```
models (Python)  ──IR──▶  native/src/migrate/model.rs   build the database schema (snapshot)
                          native/src/migrate/diff.rs    previous snapshot → ops (up and down)
                          native/src/migrate/pg.rs      ops → Postgres DDL
                          native/src/ext.rs             which extension provides what
python/orm/migrations.py  files, runner, `orm_migrations` table
python/orm/__main__.py    `python -m orm makemigrations / migrate / rollback / ...`
```

The generator diffs against the **snapshot stored with the last migration**, not
against a live database, so `makemigrations` works offline and gives the same output on
every machine. The diff, the naming rules and the SQL all live in Rust, so other
language frontends get them for free.

## Declaring schema objects

```python
from orm import Check, Exclude, Index, Key, Model, Sql, Trigger, Unique
from orm import fields as f
from orm.ext import btree_gist, citext, pg_trgm, pgvector


class Room(Model, table="rooms"):
    id = f.Uuid(primary_key=True, default=Sql("gen_random_uuid()"))
    name = citext.CIText(unique=True)            # case-insensitive, needs citext
    floor = f.Integer(check="floor >= 0")        # column-level CHECK
    features = f.Json(nullable=True, comment="free-form attributes")
    embedding = pgvector.Vector(384, nullable=True)
    updated_at = f.DateTime(default_now=True)

    class Meta:
        indexes = [
            Index("floor", "-updated_at", where="features IS NOT NULL"),  # partial, DESC
            Index(Sql("lower(features->>'kind')"), name="rooms_kind_idx"),  # expression
            Index("floor", include=["name"]),                               # covering
            Index(Key("name", collation="C"), name="rooms_name_c_idx"),
            pg_trgm.TrigramIndex("name"),                                    # GIN gin_trgm_ops
            pgvector.HnswIndex("embedding", ops="vector_cosine_ops", m=16),
        ]
        constraints = [
            Unique("floor", "name", nulls_not_distinct=True),
            Check("floor < 200", name="rooms_floor_sane"),
        ]
        triggers = [
            Trigger("touch", before=("update",),
                    body="BEGIN NEW.updated_at := now(); RETURN NEW; END;"),
        ]
        comment = "bookable rooms"


class Booking(Model, table="bookings"):
    id = f.BigInt(primary_key=True, auto_increment=True)
    room_id = f.Uuid(index=True)
    starts_at = f.DateTime()
    ends_at = f.DateTime()
    room = f.BelongsTo("Room", via="room_id", on_delete="restrict", deferrable="deferred")

    class Meta:
        constraints = [
            btree_gist.NoOverlap("room_id", start="starts_at", end="ends_at"),
            # the same, spelled out:
            # Exclude(("room_id", "="), (Sql('tstzrange("starts_at", "ends_at")'), "&&"),
            #         requires=["btree_gist"]),
        ]
```

| Object | Declared with | Notes |
|---|---|---|
| index | `Index(*keys, name, unique, method, where, include, with_, nulls_not_distinct)` | keys: field name, `"-field"` (DESC), `Sql("expr")`, or `Key(target, opclass, desc, nulls, collation)` |
| single-column unique / index | `f.X(unique=True)` / `f.X(index=True)` | |
| unique constraint | `Unique(*fields, name, nulls_not_distinct, deferrable)` | for expressions or a `WHERE`, use `Index(..., unique=True)` |
| check | `Check(expr, name)` or `f.X(check=...)` | raw SQL with column names |
| exclusion | `Exclude((key, operator), ..., method="gist", where, deferrable)` | |
| foreign key | `f.BelongsTo(..., on_delete, on_update, deferrable)` | `on_delete` adds `set_default` |
| trigger | `Trigger(name, before= / after= / instead_of=, update_of, for_each, when, body= / function=, args)` | `body` generates `<table>_<name>()` |
| function | `Function(name, body, returns="trigger", args, language, volatility, security_definer)` | add with `registry.add(fn)` or `Meta.functions` |
| extension | `Extension(name, schema, version)` | only needed to pin schema / version (see below) |
| comment | `f.X(comment=...)`, `Meta.comment` | |
| server default | `f.X(default=Sql("gen_random_uuid()"))` | literals and `default_now` as before |
| rename | `f.X(renamed_from="old")`, `Meta.renamed_from = "old_table"` | see *Renames* |

`Meta` options are checked: an unknown option or a wrong object type raises at class
creation; a field name that doesn't exist raises when the schema is compiled
(`QueryError` naming the model and field). Raw SQL (predicates, expressions, bodies) is
taken as is.

New column types: `f.Uuid` (`uuid`, values `uuid.UUID`, strings accepted) and `f.Json`
(`jsonb`, values are dicts / lists / scalars).

Constraint violations of any kind (unique, foreign key, check, exclusion, not-null;
SQLSTATE class 23) raise `orm.IntegrityError`.

### Names

Generated names follow what Postgres itself would choose: `users_pkey`,
`users_email_key`, `posts_author_id_fkey`, `posts_author_id_created_at_idx`,
`posts_views_check`, `bookings_room_id_expr…_excl`. Expressions contribute a stable
hash (`expr1a2b3c4d`). Two objects that would get the same generated name (say, a btree
and a trigram index on one column) are an error: give one an explicit `name=`. Names longer than 63 bytes are cut and get a hash suffix, so two
long names never collapse into one. Indexes and constraints share one namespace and
duplicates are rejected.

## Extensions

An extension contributes types, index methods, operator classes and functions. The
engine knows what the common ones provide (`native/src/ext.rs`):

| extension | pulled in by |
|---|---|
| `citext` | type `citext` |
| `pg_trgm` | opclasses `gin_trgm_ops`, `gist_trgm_ops`; `similarity()`, ... |
| `vector` (pgvector) | types `vector`, `halfvec`, `sparsevec`; methods `hnsw`, `ivfflat`; `vector_*_ops` |
| `postgis` | types `geometry`, `geography`; `ST_*` constructors |
| `hstore` | type `hstore`, its opclasses |
| `btree_gist`, `btree_gin` | their opclasses (scalar equality in a GiST exclusion: declare `requires=["btree_gist"]`) |
| `bloom` | method `bloom` |
| `pgcrypto`, `uuid-ossp`, `unaccent` | their functions (`uuid_generate_v4()`, `crypt()`, ...) in defaults, checks, predicates |

Using any of these is enough: the next migration starts with `CREATE EXTENSION IF NOT
EXISTS`, and the down migration drops it last (with a warning, since other schemas may
use it). Declare `Extension("postgis", schema="extensions", version="3.4")` (in
`Meta.extensions` or `registry.add(...)`) to pin where and which version. An extension
the engine doesn't know is taught with the names that should pull it in:

```python
registry.add(Extension("acme", types=("acme_money",), functions=("acme_slug",)))
```

### Frontend helpers: `orm.ext`

Each module wraps one extension with fields and index helpers; they are thin and are the
template for adding more:

| module | provides |
|---|---|
| `orm.ext.citext` | `CIText()`: `str` values; parameters are cast so comparisons, `in_()` and `on_conflict` are case-insensitive |
| `orm.ext.pg_trgm` | `TrigramIndex(*fields, method="gin"/"gist")`: indexes `contains()` / `icontains()` |
| `orm.ext.pgvector` | `Vector(dims)`: `list[float]` values; `HnswIndex(field, ops, m, ef_construction)`, `IvfflatIndex(field, ops, lists)` |
| `orm.ext.postgis` | `Geometry(shape, srid)`, `Geography(...)`: EWKT strings; `SpatialIndex(field)` |
| `orm.ext.btree_gist` | `NoOverlap(*equal_fields, start=, end=)`: exclusion constraint over a time range |

A field of an extension type sets three things on top of the usual field options:

* `db_type`: the SQL type (`vector(3)`);
* `read_sql`: SQL wrapped around the column when it is selected or returned, `{}` being
  the column (`CAST(CAST({} AS text) AS jsonb)` lets the driver decode a vector as JSON);
* `write_sql`: SQL wrapped around every bound value assigned to or compared with the
  column (`CAST({} AS citext)`).

Its Python value type is the `type_name` of the field class it extends (text, json, ...).
The planner applies the templates in `SELECT`, `RETURNING`, `INSERT`, `UPDATE` and in
comparisons, so the rest of the API works unchanged. (`citext` values decode as text
natively; pgvector has no driver codec, hence the JSON round trip.)

## Migrations

```bash
python -m orm makemigrations [name]          # write migrations/NNNN_name/{up.sql,down.sql,snapshot.json}
python -m orm makemigrations --check         # exit 1 if models changed without a migration (CI)
python -m orm makemigrations --empty data    # an empty migration for hand-written SQL
python -m orm sqlmigrate 2 [--down]          # print the SQL
python -m orm migrate [target]               # apply pending migrations
python -m orm rollback [--steps N | --to 0002_x | --to zero]
python -m orm showmigrations
```

Settings come from flags (`--models`, `--dir`, `--url`, `--pythonpath`) or
`[tool.orm]` in `pyproject.toml`; the URL also from `ORM_DATABASE_URL`. This repository's
`pyproject.toml` points at the blog example, whose first migration is in
[`examples/blog/migrations`](../examples/blog/migrations/0001_initial/up.sql).

From Python:

```python
from orm.migrations import Migrations, Migrator

migs = Migrations("migrations")          # default registry; Migrations(dir, registry) otherwise
plan = migs.plan()                       # .up / .down steps (summary, sql, warning), .snapshot
migs.make("add bookings")                # None if nothing changed
await Migrator(db, migs).upgrade()       # / .downgrade(steps=1, target=None) / .status()
```

### What the generator does

* **Tables and columns** are matched by name. New tables are created in foreign-key
  order with their primary key, uniques, checks, exclusions and foreign keys inline; a
  foreign-key cycle is closed with `ALTER TABLE ... ADD CONSTRAINT` after both tables
  exist. Changed columns are altered in place: `TYPE ... USING col::type`,
  `SET / DROP NOT NULL`, `SET / DROP DEFAULT`, `ADD / DROP IDENTITY`.
* **Indexes and constraints** are matched by name, then by definition: if only the
  name differs (because the table or a column was renamed) the object is renamed; if
  the definition changed it is dropped and recreated.
* **Triggers** are dropped and recreated when they change. A trigger function whose
  body changed is replaced with `CREATE OR REPLACE FUNCTION`; the trigger stays.
* **Order**: extensions → functions → drop triggers / foreign keys / indexes /
  constraints → renames → drop tables → create tables → add / alter / drop columns →
  add constraints (primary keys and uniques before foreign keys) → indexes → triggers →
  comments → drop unused functions → drop unused extensions.
* **Down** is the same diff in the other direction, so it is always the exact inverse
  of up at the schema level (data a drop removed doesn't come back).
* **Warnings** are emitted (in the CLI output and as `-- WARNING:` lines in `up.sql`)
  for steps that can lose data or fail on existing rows: dropping tables or columns,
  type changes, `SET NOT NULL`, a `NOT NULL` column without a default, new unique /
  check / exclusion / foreign-key constraints, dropping an extension.

### Renames

Without a hint, a renamed field is a dropped column plus a new one. `renamed_from`
turns it into `RENAME`:

```python
class Post(Model, table="articles"):
    title = f.String(200, column="headline", renamed_from="title")   # column rename

    class Meta:
        renamed_from = "posts"                                         # table rename
```

Generated constraint and index names follow the rename (`posts_pkey` →
`articles_pkey`). The hints only matter for the migration that performs the rename;
they can stay (they are ignored once the old name is gone) or be removed afterwards.

### Applying

* Each migration runs in **one transaction** together with its row in
  `orm_migrations` (`name`, `checksum`, `applied_at`), under a transaction-level advisory
  lock, so concurrent deploys don't apply a migration twice and a failing migration
  leaves the database at the previous one.
* `up.sql` is run as one script, so it may hold anything Postgres accepts in a
  transaction, including hand-written data changes. (`CREATE INDEX CONCURRENTLY`
  can't run in a transaction and is not supported yet.)
* The SHA-256 of each applied `up.sql` is recorded; if an applied file changes, the
  runner stops. A migration applied after a later one, or one recorded in the database
  but missing from the directory, also stops it.
* Migrations are numbered (`0001_`, `0002_`, ...). Two branches that both add `0003_`
  are reported; renumber one of them and regenerate it so its snapshot builds on the
  other.

### `create_tables()` and `drop_tables()`

Still there for development and tests. `create_tables()` runs the full initial
migration with `IF NOT EXISTS` / `OR REPLACE` forms in one transaction (it never alters
existing objects); `drop_tables()` drops every model table with `CASCADE` and the
generated functions, leaving extensions in place.

## Why not SeaORM's migration tooling

SeaORM ships `sea-orm-migration` (a runner plus a Rust DSL for writing migrations by
hand) and, in 2.0, entity-first schema sync (it creates missing tables and columns). The
generator here needs things neither has: triggers and functions, exclusion constraints,
extensions, partial / expression / opclass indexes, renames and down migrations.
Building on them would also tie the migration IR to SeaORM, which the plan treats as a
replaceable engine. Its introspection crate `sea-schema` is a good fit for the next
step, diffing a live database instead of a snapshot to detect drift.

## Not done yet

* Introspection of a live database: drift detection, adopting an existing schema.
* `CREATE INDEX CONCURRENTLY` / non-transactional migrations.
* Generated columns, views / materialized views, sequences, enum and domain types,
  row-level security policies, partitioning, multiple database schemas.
* Composite foreign keys; changing a function's return type (Postgres can't replace
  it in place; drop the trigger in a hand-edited migration).
* The schema DSL: `examples/blog/schema.orm` shows the planned `@@index` / `@@check`
  syntax, but `models.py` is still written by hand.
