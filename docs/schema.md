# Schema language, extensions and migrations

The schema is written once, in a `.prisma` file, and every language uses it:

```
schema.prisma ──▶ orm-core (Rust) ──▶ schema IR (JSON) ──▶ Python: orm.load() / generated models.py + .pyi
   ▲                 │                                └──▶ JS / Go / ...: the same IR (bindings to come)
extensions/*.toml    │
                     ├──▶ migrations/NNNN_name/{up.sql, down.sql, snapshot.json}
                     └──▶ PostgreSQL / SQLite DDL (create_tables)
```

The file uses Prisma's schema syntax, so Prisma's VS Code extension highlights,
formats and completes it. What Prisma has no syntax for (checks, exclusions,
triggers, functions, ...) is written as our own attributes and blocks; the editor
marks those lines as errors, which is expected. Nothing calls Prisma's CLI or
engines: our parser, migrator and code generators do the work. The reasoning, and
the mapping from the old `.orm` syntax, is in [`prisma-syntax.md`](prisma-syntax.md).

No Python, Rust or JS class describes the schema. The parser, the extension catalog,
the migration generator and the code generators live in the binding-free `core/` crate,
so each language binding only loads the result. The same crate builds the `orm`
command-line tool, which needs no Python.

| Path | What |
|---|---|
| `core/src/dsl/` | parser (`syntax.rs`) and lowering to the IR (`lower.rs`) |
| `core/src/ext.rs`, `core/extensions/postgres/*.toml` | extension files and the catalog built from them |
| `core/src/migrate/` | snapshot model, diff, PostgreSQL / SQLite renderers, migration folders |
| `core/src/codegen/python.rs` | `models.py` / `models.pyi` generator |
| `core/src/main.rs` | the `orm` CLI |
| `python/orm/model.py` | `orm.load()` / `orm.loads()` / `orm.define()`: model classes from the IR |
| `python/orm/migrations.py`, `__main__.py` | migration runner and `python -m orm` |

## SQLite

Set the schema target explicitly; omitting a datasource keeps PostgreSQL as the
default. See [the SQLite example](../examples/sqlite/schema.prisma).

```prisma
datasource db {
  provider = "sqlite"
}

enum Status {
  draft
  published
  @@storage(text)
}
```

Use `sqlite://:memory:` for an in-memory database or
`sqlite:///absolute/path.db` for a persistent file. Load or generate the models
from that SQLite schema, then pass the URL to Python's `orm.connect()` or
TypeScript's `connect()`. A registry accepts schemas for one dialect; connecting
it to another dialect fails before queries run.

Each client owns one serialized connection on a dedicated worker thread.
`max_connections` / `maxConnections` does not create a SQLite pool. Transactions
hold that connection until commit or rollback; nested transactions use savepoints.
Foreign key enforcement is enabled. Separate clients using `:memory:` have
separate databases.

Enums must choose `@@storage(text)` or `@@storage(int)` explicitly. JSON and UUID
values use text storage; `String @db.Uuid` retains UUID conversion. Dates use
`DateTime @db.Date`, and timestamps round-trip in UTC. `now()` uses SQLite's
`CURRENT_TIMESTAMP`. AUTOINCREMENT requires an integer primary key. Aggregates,
windows, CTEs, sliced prefetch and bulk updates use the shared query APIs.

Unsupported features fail schema validation: exact Decimal, arrays, native enums,
PostgreSQL extensions and declared SQL functions, native type overrides and SQL
conversion templates, comments, exclusion constraints, covering indexes, non-btree
index methods, operator classes, explicit NULL ordering in indexes, index storage
parameters, deferrable uniqueness and NULLS NOT DISTINCT. SQLite supports inline
row triggers with one INSERT, UPDATE or DELETE event. Statement triggers, TRUNCATE,
INSTEAD OF on tables, trigger functions, languages and arguments are unsupported.
Row/advisory locks and DISTINCT ON fail query validation. Raw SQL expressions and
trigger bodies must use SQLite syntax. Generated/computed columns remain out of
scope.

Any schema change currently rebuilds every managed table in one transaction.
Rebuilds copy rows, apply table/column rename hints, preserve AUTOINCREMENT high
water marks, recreate managed indexes/triggers, and validate foreign keys before
commit. A failure rolls back the schema, data and migration record. Unmanaged
indexes or triggers attached to a managed table stop the migration with
`orm_unmanaged_index_or_trigger`; incorporate them into the schema or explicitly
remove them before retrying. Review generated up/down SQL before applying it:
dropped columns cannot recover their old values on downgrade. Avoid rebuilding
large databases without planning for the copy cost.

SQLite snapshots use version 2 and record their dialect. PostgreSQL snapshots
retain version 1 for compatibility. Cross-dialect snapshots are rejected; use a
separate migrations directory for each target.

## A schema

```prisma
// examples/blog/schema.prisma (excerpt)
model Post {
  id         BigInt    @id @default(autoincrement())
  author_id  BigInt
  title      String    @db.VarChar(200)
  body       String    @db.Text
  views      Int       @default(0)
  published  Boolean   @default(false)
  created_at DateTime  @default(now())
  author     User      @relation(fields: [author_id], references: [id], onDelete: Cascade)
  comments   Comment[]

  @@index([author_id])
  @@index([author_id, created_at(sort: Desc)], where: raw("published"))
  @@index([title(ops: raw("gin_trgm_ops"))], type: Gin) // pulls in pg_trgm
  @@map("posts")
  @@check("views >= 0", name: "posts_views_not_negative")
}
```

Using it from Python:

```python
models = orm.load("schema.prisma")       # compile at runtime; models["Post"] ...
# or generate a module (typed, autocompletes):  python -m orm generate
from blog.models import Post, User
```

## Language reference

As in Prisma, each field and each attribute is on its own line. Comments are `// ...`.
Strings are `"..."` (escapes `\" \\ \n \t`) or `"""..."""`, taken verbatim with common
indentation removed, which suits SQL bodies. Raw SQL (predicates, expressions,
bodies) is written as it appears in the DDL, using column names.

Two rules keep a file intact when the Prisma formatter runs on save, and the parser
enforces both:

1. **Each attribute is on one line.** The formatter would tear a split `@@trigger(...)`
   apart. Put a long trigger body in a `function` block.
2. **`"""` strings only appear in top-level blocks** (`function`, `datasource`), never
   inside a model. Inside a model, write `"...\n..."` on one line.

The parser doesn't depend on the order of `@@` attributes (the formatter puts Prisma's
own first and ours after them).

### Top level

```prisma
import "extensions/acme.toml" // an extension file (path relative to this file)

datasource db {
  provider   = "postgresql"
  url        = env("DATABASE_URL") // accepted and ignored: the URL comes from ORM_DATABASE_URL
  extensions = [postgis(schema: "ext", version: "3.4"), uuid_ossp(map: "uuid-ossp")]
}

function audit_row {
returns  = trigger
args     = ""
language = plpgsql
body     = """
BEGIN
INSERT INTO audit_log (tbl) VALUES (TG_TABLE_NAME);
RETURN NULL;
END;
"""
}
```

* The `datasource` is optional (Postgres is implied), but Prisma's editor wants one.
  `extensions` pins where an extension goes and which version; extensions the schema
  uses are added anyway. `map:` gives the real name of one that isn't an identifier.
* `function name { ... }` is a stand-alone SQL function: `returns` (default `trigger`),
  `args` (the SQL argument list, e.g. `"a integer, b text"`), `language` (default
  `plpgsql`), `volatility` (`immutable` / `stable` / `volatile`), `security_definer`,
  `body`. The formatter removes the indentation inside it, which SQL doesn't mind.
* `generator` blocks (Prisma Client's) are read and ignored. `type` and `view`
  aren't supported yet; `enum` is below.

A model's table is the model name in lower case, or `@@map("name")`.
`@@comment("...")` and `@@renamed_from("old_table")` set its comment and previous name.

### Optional schema imports

Prefer a single schema file for an ordinary project. Explicit schema imports are
available for reusable libraries or a small module that needs its own generated
models; no app registration or directory discovery is required.

```prisma
// schema.prisma: the main schema
import "billing/schema.prisma" (prefix: "billing_")
import "audit/schema.prisma" // prefix is optional

datasource db {
  provider = "postgresql"
  extensions = [pg_trgm]
}
```

Import paths resolve relative to the file containing them. Only `.prisma` imports
load schemas; existing `.toml` imports still load extension definitions. Imports
must be explicit so compilation has one main schema that owns the datasource,
dialect and extension configuration. Imported schemas cannot declare a datasource
or import extension definitions; put those settings and definitions in the main
schema. Imported models can use the main schema's extension types. As usual,
extension usage is inferred across the complete compiled schema, and the database
URL comes from the connection API or `ORM_DATABASE_URL`.

All imported models, enums and SQL functions compile together, with one migration
history. Relations can reference models in another file directly. Model and enum
names must be globally unique; a table prefix does not create a model namespace.
A prefix is prepended to default table names, explicit `@@map` names and
`@@renamed_from` table names. Nested imports concatenate their prefixes. Enum type
names, explicit constraint/index names and raw SQL are preserved; use unique
explicit names and write raw SQL with the final table names. Cycles and repeated
imports, including the same file through different paths, are errors.

Python generation writes `models.py` and `models.pyi` beside each schema. Keep
schema modules in separate directories. Use a shared Python package tree, or put
the main schema in an importable package when importing an external library. Each
public module exports its own models, enums and generated typing helpers. A private
`_orm_models.py` / `.pyi` beside the main output holds the complete compiled schema
and shared classes, allowing either public module to be imported first:

```python
from myproject.models import User
from myproject.billing.models import Invoice
```

`-o` relocates the main module and its private shared module; imported modules stay
beside their schema files. Generate from the main schema so every module receives
the same database settings and prefixes. TypeScript generation currently writes
one combined module from the same compiled schema.

### Fields

`name Type[?] @attr...`, where `?` makes the column nullable.

| Type | SQL | Python value |
|---|---|---|
| `BigInt`, `Int` | `bigint`, `integer` | `int` |
| `Float` | `double precision` | `float` |
| `Boolean` | `boolean` | `bool` |
| `String`, `String @db.VarChar(n)` | `varchar`, `varchar(n)` | `str` |
| `String @db.Text` | `text` | `str` |
| `String @db.Uuid` | `uuid` | `uuid.UUID` (strings accepted) |
| `DateTime`, `DateTime @db.Date` | `timestamp with time zone`, `date` | `datetime` (aware), `date` |
| `Json` | `jsonb` | dicts, lists, scalars |
| `Decimal`, `Decimal @db.Decimal(p, s)` | `numeric`, `numeric(p, s)` | `decimal.Decimal` (exact; `int`, `float`, `"12.50"` accepted) |
| an enum (below) | a Postgres enum type, `text` or `integer` | members of a generated `StrEnum` / `IntEnum` |
| `Type[]`, e.g. `String[]`, `Int[]`, `Role[]` | `text[]`, `integer[]`, `"role"[]`, ... | `list` of the element type |
| `String @db.Citext` | `citext` (the extension type) | `str` |
| `Unsupported("vector(384)")`, `Unsupported("geography(Point, 4326)")` | an extension type, from its extension file | from the file's `value` |

Prisma's other `@db.*` types (`@db.SmallInt`, `@db.Timestamp(3)`, `@db.Char(2)`,
`@db.Inet`, ...) set the column's SQL type; values convert as the field type's.

Decimals travel in Postgres' binary `numeric` format, digit for digit, so money never
goes through a float: `SUM` of a decimal column is a `Decimal`, and so is `AVG` (other
averages are floats).

Arrays (`Type[]`) work for every scalar type and enum, but not for extension types or
primary keys. `@default([])` / `@default(["a", "b"])` give list defaults (`[member]`
for enum arrays). `String[]` is `text[]`; `String[] @db.VarChar(20)` is
`varchar(20)[]`, and values are cast to it. `Type[]?` makes the array itself nullable;
elements can always be `NULL` (`None`).

### Enums

```prisma
enum Role {            // a Postgres enum type: CREATE TYPE "role" AS ENUM (...)
  member
  editor
  admin  @map("ADMIN") // stored as 'ADMIN'
  @@map("user_role")   // the type's name (default: the enum name in lower case)
}

enum Color {           // a text column with CHECK ("color" IN ('red', 'green'))
  red
  green
  @@storage(text)
}

enum Priority {        // an integer column with CHECK ("priority" IN (1, 2, 3))
  low    @value(1)
  normal @value(2)
  high   @value(3)
  @@storage(int)
}

model Tag {
  id       BigInt   @id
  role     Role     @default(member)
  priority Priority @default(normal)
  colors   Color[]  @default([red])
}
```

`@@storage` picks how values are stored: `native` (the default, Prisma's behaviour)
uses the database's enum type where the dialect has one; `text` and `int` work on any
database and are kept to the enum's values by a `CHECK` constraint named
`<table>_<column>_enum_check`. Integer enums give each value with `@value(n)` (there is
no implicit numbering, so reordering the schema never changes stored data).

In Python each enum is a class of the generated module: a `StrEnum` whose values are the
stored labels, or an `IntEnum`. Columns read back as members (`profile.role is
Role.member`); writes and filters take members or the stored values (`"admin"`, `3`).

Migrations create the type before the tables using it and drop it after them. Values
added to a native enum become `ALTER TYPE ... ADD VALUE ... [BEFORE ...]`, which keeps
their place; Postgres can't use a new value in the transaction that added it, so the
step carries a warning. A removed or reordered value recreates the type: the old one is
renamed, the new one created, every column using it converted through text (defaults
dropped and restored), and the old type dropped. That fails if rows still hold a removed
value, and says so in a warning.

| Attribute | Meaning |
|---|---|
| `@id` | primary key |
| `@default(autoincrement())` | `GENERATED BY DEFAULT AS IDENTITY` |
| `@unique` | single-column unique constraint |
| `@default(0)` `@default("x")` `@default(true)` | literal default |
| `@default("{\"a\": 1}")` on `Json` | JSON default, written as JSON text |
| `@default(now())` | `now()` |
| `@default(dbgenerated("gen_random_uuid()"))` | any SQL default |
| `@client_default(...)` | ORM insert default (below); not in the DDL |
| `@map("db_name")` | column name, if different from the field name |
| `@db.*` | the SQL type (above) |
| `@check("views >= 0")` | column check constraint |
| `@comment("...")` | column comment |
| `@renamed_from("old")` | previous column name: the migration renames it instead of dropping |

### Client defaults

`@default(...)` is the database default.
It goes into the DDL and the migrations, and it applies to every writer.
`@client_default(...)` is the ORM default.
It fills an omitted insert value before native transforms and validators, in Python and TypeScript.
It is not in the DDL or in a migration snapshot.

```
id      String   @id @client_default(uuid7()) @db.Uuid
token   String   @client_default(uuid())
status  Status   @default(OLD) @client_default(ACTIVE)
created DateTime @default(now())
```

* `@client_default` takes a literal, `uuid()`, `uuid7()` or `now()`.
  A literal has the form of a `@default` literal: an enum member name, a list, or JSON text on a `Json` field.
  `"null"` on a `Json` field is the JSON value null, like `@default("null")`.
  A `Decimal` takes a number or text (`1.5` or `"1.5"`) and keeps every digit, and a `DateTime` takes an RFC 3339 timestamp with an offset.
  An enum field takes the member name, not a string.
* A field takes one client default; `@default(uuid())` counts as one.
* `uuid()` and `uuid7()` fit a `String` field, also with `@db.Uuid`.
  `now()` fits a `DateTime` field, also with `@db.Date` (the UTC date).
* `autoincrement()` and `dbgenerated(...)` fail, because only the database can make them.
* A field can have both defaults.
  The client default fills ORM inserts, and the database default covers other writers.
* An explicit value, also `NULL`, wins over both.
* `insert`, bulk insert and upsert make a new `uuid()`, `uuid7()` or `now()` value for each row.
* A field with either default is optional in the generated insert types.

### Relations

```
author   User      @relation(fields: [author_id], references: [id])  // to-one: holds the key
author   User?     @relation(fields: [author_id], references: [id], onDelete: SetNull)
posts    Post[]                                                         // to-many: the other side
```

To-one relations name their key field in `fields:` and the target field in
`references:`, and create the foreign key. Their options are `onDelete:` / `onUpdate:`
(`Cascade`, `SetNull`, `SetDefault`, `Restrict`, `NoAction`; the default is the
database's, no action), `deferrable: immediate | deferred`, and `map: "name"`, the
foreign key's name (default `<table>_<column>_fkey`). The relation is
optional (`User?`) exactly when the key field is nullable; the compiler checks this.
A to-many relation (`Post[]`) is paired with the to-one relation on the other model.
When two models have more than one relation between them, name both sides:
`@relation("author", fields: ...)` and `@relation("author")`. Composite keys aren't
supported yet.

```
profile  Profile?                                   // one-to-one: Profile.user_id is @unique
tags     Tag[]    @relation(through: PostTag)       // many-to-many through a join model
followers User[]  @relation(through: Follow, through_fields: [followee, follower])
```

* **One-to-one** is a to-one relation (`user User @relation(fields: [user_id], ...)`)
  whose key is unique (`@unique`, `@id` or a one-field `@@unique`), and its other side
  without `fields:`, which must be optional (`Profile?`): a user may have no profile.
* **Many-to-many** goes through an explicit join model with a to-one relation to each
  side (`PostTag.post`, `PostTag.tag`). `through:` names it; the relations are found by
  their targets, or named with `through_fields: [<to this model>, <to the target>]`
  (needed when both point at the same model, as in followers). The join model is an
  ordinary model: give it a `@@unique([post_id, tag_id])` and any extra columns, and
  query it directly too. Filters, aggregates and `prefetch_related` go through it in one
  hop (see [`python-api.md`](python-api.md)). Prisma's implicit many-to-many (no join
  model) isn't supported.

### Model attributes (`@@`)

```
@@index([author_id])
@@index([author_id, created_at(sort: Desc, nulls: last)], where: raw("published"), name: "x")
@@index([author_id, -created_at(nulls: last)])   // -field is sort: Desc
@@index([title(ops: raw("gin_trgm_ops"))], type: Gin)
@@index([sql("lower(email)", collate: "C")], unique: true)
@@index([floor], include: [name], nulls_not_distinct: true)
@@index([embedding(ops: vector_cosine_ops)], type: hnsw, with: { m: 16, ef_construction: 64 })

@@unique([author_id, slug], nulls_not_distinct: true, deferrable: deferred)
@@check("starts_at < ends_at", name: "bookings_order")
@@exclude([room_id(op: "="), sql("tstzrange(starts_at, ends_at)", op: "&&")], where: "not cancelled")

@@trigger(touch, before: [update], update_of: [title], when: "OLD.title <> NEW.title", body: "BEGIN NEW.updated_at := now(); RETURN NEW; END;")
@@trigger(audit, after: [insert, update, delete], for_each: statement, function: audit_row, args: ["posts"])
```

* Index keys are field names or `sql("expression")`, with options `sort: Asc|Desc`,
  `nulls: first|last`, `ops: <operator class>` (a name or `raw("...")`),
  `collate: "..."`, and `op: "..."` in `@@exclude`.
* `-field` is the short form of `field(sort: Desc)`, and gives the same migration.
  An expression key keeps `sort: Desc`.
  `@@unique` takes no order, because a unique constraint has none:
  use `@@index([a, -b], unique: true)`.
* `@@index([field])` with one plain key is that column's index.
* `type:` is the access method (default btree for indexes, gist for exclusions):
  Prisma's `BTree`, `Hash`, `Gist`, `Gin`, `SpGist`, `Brin`, or any other method
  (`hnsw`, `ivfflat`, `bloom`). `with:` sets storage parameters. `where:` takes
  `raw("...")` or a string. The database name is `name:` or `map:`.
* For a unique *expression* or a partial unique rule, use `@@index(..., unique: true)`.
* `@@exclude` with `=` on a plain column under GiST pulls in `btree_gist` by itself.
* A trigger fires on exactly one of `before:` / `after:` / `instead_of:`. Give it
  either a one-line `body:` (a function `<table>_<name>()` is generated and replaced in
  place when the body changes) or a `function:` declared in a `function` block.

Mistakes are reported with file, line and column:

```
schema.prisma:22:14: Post.title: unknown type Strin; expected a model or BigInt, Int, Float, Boolean, String, ...
schema.prisma:30:24: Post: @@index: unknown argument `wher`
schema.prisma:44:14: relation Comment.author: author_id is not nullable, so the relation type is `User`
schema.prisma:34:3: model Post: an attribute must be on one line (put a long trigger body in a `function` block)
```

### Protected writes (`@@protected_write`)

`@@protected_write` is an application-level check in the ORM. It does not protect the database.
Raw SQL (`db.execute`), migrations, other ORM processes without this schema, and other database clients can still write.
For database-level protection, use `@@trigger` or database grants.

```prisma
model Post {
  id    Int    @id
  title String
  @@protected_write
}
```

* All ORM writes to a protected model fail outside an `allow_writes` scope (`allowWrites` in TypeScript) with `WriteProtected`:
  inserts, bulk inserts, updates, `update_many`, deletes, upserts, instance writes and composed writes.
* The check is on the table that the SQL writes.
  `post.tags.add()` writes `PostTag` rows, so `PostTag` must be protected for it to fail.
  A proxy writes the table of its root, so the protection of the root applies to the proxy.
  A composed write also writes the tables of its storage ancestors.
* A scope allows the tables of the models it names. Scopes nest: an inner scope adds its models to the outer ones.
* Reads are not changed.
* `@@protected_write` creates no trigger, no grant and no DDL. Migrations ignore it.

A database-level option is a trigger that rejects writes (PostgreSQL).
It rejects every writer, also the service that should write; exempt that service with a database role or grants, not with this trigger:

```prisma
model Post {
  id    Int    @id
  title String
  @@trigger(read_only, before: [insert, update, delete], body: "BEGIN RAISE EXCEPTION 'posts are read-only'; END;")
}
```

### Names

Generated names follow what Postgres itself would choose: `posts_pkey`,
`users_email_key`, `posts_author_id_fkey`, `posts_author_id_created_at_idx`,
`posts_views_check`, `bookings_room_id_expr…_excl`. Expressions contribute a stable hash.
Names over 63 bytes are cut and get a hash suffix. Indexes and constraints share one
namespace. Two objects that would get the same name (a btree and a trigram index on
one column, say) are an error, so give one an explicit `name:`.

## Extensions

An extension is a TOML file saying what it adds to the schema language:

```toml
# core/extensions/postgres/vector.toml (excerpt)
name = "vector"                                   # the CREATE EXTENSION name
index_methods = ["hnsw", "ivfflat"]
opclasses = ["vector_l2_ops", "vector_cosine_ops"]
functions = ["cosine_distance"]

[types.vector]                                    # a field type: Unsupported("vector(384)")
sql = "vector({dims})"                            # {arg} placeholders come from `args`
args = ["dims"]
# defaults = { dims = "3" }                       # optional default per argument
value = "json"                                    # how values travel: text, json, big_int, uuid, ...
read = "CAST(CAST({} AS text) AS jsonb)"          # optional: SQL around the column when read
write = "CAST(CAST({} AS text) AS vector({dims}))" # optional: SQL around each bound value
python = "list[float]"                            # optional type hints for generated code
typescript = "number[]"
```

Built in (`core/extensions/postgres/`, compiled into the core):

| file | adds |
|---|---|
| `citext` | type `citext`, written `String @db.Citext` (str; values are cast, so comparisons, `in_()` and `ON CONFLICT` ignore case) |
| `pg_trgm` | opclasses `gin_trgm_ops`, `gist_trgm_ops`; `similarity()`, ... |
| `vector` (pgvector) | types `vector(dims)`, `halfvec(dims)` (`list[float]`); methods `hnsw`, `ivfflat`; `*_ops` opclasses |
| `postgis` | types `geometry(shape, srid)`, `geography(shape, srid)` (EWKT strings); GiST opclasses; `ST_*` functions |
| `btree_gist`, `btree_gin`, `hstore` | operator classes |
| `bloom` | method `bloom` |
| `pgcrypto`, `uuid-ossp`, `unaccent` | functions (`crypt()`, `uuid_generate_v4()`, ...) |

Anything an extension provides pulls it in: a column of its type, an index using its
method or operator class, or a call to one of its functions in a default, check or
predicate. The next migration then starts with `CREATE EXTENSION IF NOT EXISTS`, and
the down migration drops it last (with a warning). `extensions = [name(schema:, version:)]`
in the `datasource` only pins where it goes and which version. Your own extensions are
files next to the schema, loaded with `import "extensions/acme.toml"`; their types are used
as `Unsupported("money(12)")`. Unknown keys in a file are an error.

The planner applies `read` / `write` in `SELECT`, `RETURNING`, `INSERT`, `UPDATE` and
comparisons, so queries on extension-typed columns look like any other.

## Code generation

```bash
python -m orm generate                   # models.py + models.pyi next to the schema (or -o path)
npx orm generate                         # models.ts
orm generate python -o app/models.py     # the standalone binary: say which language
```

`models.py` embeds the compiled IR and builds the classes with `orm.define()`, so no
schema detail is restated in Python. `models.pyi` gives editors and type checkers the
typed classes, relation paths (`User.posts.created_at` is `ColumnRef[datetime]`),
insert / update `TypedDict`s and query sets (see [`python-api.md`](python-api.md)).
`tests/test_migrations.py` checks that the committed blog module is up to date.
Without generation, `orm.load("schema.prisma")` returns the same classes at runtime.
TypeScript: see [`typescript-api.md`](typescript-api.md).

## Migrations

```bash
python -m orm makemigrations [name]          # migrations/NNNN_name/{up.sql,down.sql,snapshot.json}
python -m orm makemigrations --check         # exit 1 if the schema changed without a migration (CI)
python -m orm makemigrations --empty data    # an empty migration for hand-written SQL or a data step
python -m orm sqlmigrate 2 [--down]
python -m orm migrate [target]               # apply pending migrations
python -m orm rollback [--steps N | --to 0002_x | --to zero]
python -m orm showmigrations
python -m orm pull [-o schema.prisma] [--force]   # the schema file from a live database
python -m orm baseline                       # mark the first migration applied (after pull)
python -m orm drift                          # exit 1 if the database differs from the last snapshot
```

There is one command line, written in Rust (`cli/`): `python -m orm`, `npx orm` and the
standalone `orm` binary (`cargo install --path cli`) all run it, through the Python
extension, the Node addon or on their own, so they take the same arguments, write the
same files and apply migrations the same way. The bindings only change the defaults:
the name in `--help`, the configuration file read first, and the language `generate`
writes (the binary needs `python` / `typescript`, or an `-o` path that tells).

Settings come from flags (`--schema`, `--dir`, `--url`), else `[tool.orm]` in
`pyproject.toml` or the `"orm"` key of `package.json` (`schema`, `migrations`), and the
URL from `ORM_DATABASE_URL`. Exit codes: 0 success, 1 failure (including
`makemigrations --check` finding changes), 2 bad usage. This
repository's `pyproject.toml` points at the blog example; its first migration is
[`examples/blog/migrations/0001_initial`](../examples/blog/migrations/0001_initial/up.sql).

From Python: `Migrations("migrations", "schema.prisma")` (or a `Registry`) has
`.plan()`, `.make(name)` and `.all()`. `Migrator(db, migrations)` has `.upgrade()`,
`.downgrade()`, `.status()`, `.baseline()` and `.drift()`; `await orm.migrations.pull(db)`
reads a live database.

### What the generator does

The generator diffs the schema against the **snapshot stored with the last migration**,
not a live database, so it works offline and gives the same files everywhere.

* **Tables and columns** are matched by name first, then by `@renamed_from` /
  `@@renamed_from`. A hint applies only when the current name is not in the snapshot
  and the hinted name is, on Postgres and on SQLite. Two hints that name the same old
  table, or the same old column of one table, are an error. The snapshot stores only the new
  name, so the hint is safe to remove after the migration is generated; a kept hint
  does nothing in later migrations. New tables are created
  in foreign-key order with their keys and constraints inline; a foreign-key cycle is
  closed with `ALTER TABLE ... ADD CONSTRAINT`. Changed columns are altered in place
  (`TYPE ... USING`, `SET / DROP NOT NULL`, `SET / DROP DEFAULT`, identity).
* **Indexes and constraints** are matched by name, then by definition. If only the name
  changed (a renamed table or column), the object is renamed. If its definition
  changed, it is dropped and recreated.
* **Triggers** are recreated when they change. A changed trigger body only replaces its
  function.
* **Order**: extensions, functions, then drops (triggers, foreign keys, indexes,
  constraints), renames, dropped tables, new tables, column changes, new constraints
  (keys before foreign keys), indexes, triggers, comments, unused functions, and
  unused extensions last.
* **Down** is the same diff in reverse.
* **Warnings** (CLI output and `-- WARNING:` lines) flag steps that can lose data or
  fail on existing rows: drops, type changes, `SET NOT NULL`, a `NOT NULL` column
  without a default, new unique / check / exclusion / foreign-key constraints,
  dropping an extension.

### Applying

Each migration runs in one transaction together with its `orm_migrations` row (name,
SHA-256 of `up.sql`, time), under an advisory lock. A failing migration leaves the
database at the previous one, and concurrent deploys don't apply one twice. The runner
stops if an applied `up.sql` changed, if migrations were applied out of order, if one
recorded in the database is missing from the directory, or if two migrations share a
number. `create_tables()` / `drop_tables()` remain as development helpers (idempotent
DDL of the whole schema; drop with `CASCADE`).

### Data migrations

A migration folder may hold a data step next to `up.sql`: `data.py` with
`async def run(db)`, or `data.ts` with `export async function run(db)`.

```python
# migrations/0002_slug/data.py
async def run(db):
    await db.execute("UPDATE author SET slug = lower(name)")
```

The step runs after `up.sql`, in the same transaction and under the same advisory lock,
before the migration's `orm_migrations` row. Queries on `db` inside it, and ORM queries
on that database, go into that transaction, so they see the new columns. An error rolls
back the SQL, the data changes and the row. `makemigrations --empty name` gives a folder
for a step without schema changes.

* Python applies `data.py`: `Migrator.upgrade()`, or `python -m orm migrate`, which then
  runs through `Migrator` instead of the Rust runner. It imports the data modules
  before it connects, so the models they import are in the default registry.
* TypeScript applies `data.ts` the same way: `Migrator.upgrade()` or `npx orm migrate`
  (Node strips the types of a `.ts` file, by default in current versions; Bun runs it).
* Each tool refuses the other's file, and the standalone `orm` binary refuses both,
  before it applies anything.
* `rollback` runs `down.sql` only; a data step has no down step.
* The checksum covers `up.sql` only, so a change to a data step after it ran is not
  detected.

### Adopting a live database: pull, baseline, drift

`orm pull` reads the database the URL names and writes the schema file (`--schema`, or
`-o FILE`; `--force` overwrites one). Postgres is read from `pg_catalog` for the current
schema, SQLite from `sqlite_master` and its `PRAGMA`s. It reads tables, columns,
defaults, comments, primary keys, unique, check, exclusion and foreign-key constraints,
indexes, enum types, extensions, SQL functions and triggers. Database names are kept:
an index or constraint whose name differs from the generated one gets `name:` / `map:`,
and a foreign key gets `@relation(..., map: "name")`. Model names are the tables in
PascalCase with `@@map`, a relation field is its key column without `_id`, and the other
side is `<table>_set` (or `<table>` for a one-to-one).

Then `pull` checks its own result: it creates the pulled schema in a shadow and compares
it with the database. It prints `The schema reproduces the database.`, or the steps a
migration from the schema would still run. What the schema language cannot say is left
out, printed, and listed at the end of the file as `// gap:` lines:

* rules (`CREATE RULE`), views, row-level security policies, domains, composite and
  range types, partitioned tables, generated columns, column collations;
* tables with a composite primary key or none, composite foreign keys, columns of a type
  without a schema type (`tstzrange`, `interval`, ...);
* a `serial` column becomes an identity column (`@default(autoincrement())`), so drift
  reports `drop default` and `make ... an identity column` for it;
* a primary key not named `<table>_pkey`; `GENERATED ALWAYS` identities (read as
  `BY DEFAULT`); constraint triggers and triggers that call a function of another schema;
* on SQLite: triggers, and column types other than the storage class (`INTEGER`,
  `REAL`, `TEXT`).

`orm baseline` writes the first migration from the schema when the directory has none,
and records it in `orm_migrations` without running it, so the next `makemigrations`
holds only new changes. It fails if the database already has an applied migration, and
warns when the database differs from the migration.

`orm drift` compares the database with the snapshot of the newest migration and exits 1
when they differ, printing the steps that would bring the database back to the
snapshot. Rules, views and the other gaps above are listed as not compared.

Postgres prints expressions in its own form (`CHECK ((views >= 0))`,
`'x'::character varying`, `role = ANY (ARRAY[...])`), so the snapshot is never compared
as text. Drift and `pull` run the snapshot's DDL in a shadow (Postgres: a schema
`orm_shadow` in a transaction that is rolled back; SQLite: an in-memory database), read
the shadow back the same way as the database, and compare the two. The live schema is
re-created in a second shadow too, and an object both shadows agree on counts as equal,
because Postgres does not always print a parsed expression back the same way. So drift
needs the right to create a schema, and the extensions the snapshot uses must be
installed.

### Why not SeaORM's or Refinery's tooling

`sea-orm-migration` and Refinery are runners for hand-written migrations. Refinery
has no down migrations and doesn't use sqlx. SeaORM's entity schema sync only adds
missing tables and columns. None has triggers, exclusion constraints, extensions,
renames or snapshot diffs, and using them would tie the migration IR to one engine. If
the runner moves into Rust so every binding shares it, `sqlx::migrate` (already a
dependency) is the natural base. `sea-schema` introspection fits drift detection.

## Not done yet

* TypeScript generation and the JS binding.
* `CREATE INDEX CONCURRENTLY` / non-transactional migrations.
* Generated columns, views, domains, row-level security, partitioning, multiple
  database schemas, composite keys (primary and foreign).
* Renaming a native enum value in place (`ALTER TYPE ... RENAME VALUE`): a renamed
  value is a removal plus an addition today.
