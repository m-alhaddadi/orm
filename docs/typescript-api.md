# TypeScript API

The `orm` package in `js/` is the TypeScript counterpart of the Python package. It uses the
same schema file, the same Rust engine (`engine/`, via the N-API addon in `bindings/node`),
the same SQL and the same migrations. Its types are meant to catch as much as possible at
compile time: a column of the wrong model, a value of the wrong type, a relation that
wasn't loaded, a missing `param()` value, or an `outer()` reference with no enclosing
query all fail to type-check.

```ts
import { connect, exists, func, outer } from "orm";
import { Post, User } from "./models.js";

await connect("postgres://postgres:postgres@localhost/orm_test");
const yesterday = new Date(Date.now() - 86_400_000);

const users = await User.objects.filter(User.posts.createdAt.lt(yesterday)).all();
const rows = await User.objects
  .select({ name: User.name, posts: func.count(User.posts), popular: exists(Post.objects.filter(Post.authorId.eq(outer(User.id)), Post.published)) })
  .all(); // { name: string; posts: bigint; popular: boolean }[]
```

[`examples/blog/demo.ts`](../examples/blog/demo.ts) is a runnable tour, and it mirrors `demo.py`.

## Layout

| Path | What |
|---|---|
| `js/src/` | The package: `expr.ts` (expressions, `func`, `outer` / `exists`, windows), `model.ts` (`define()`, instances, related sets), `query.ts` (`QuerySet`, `Prefetch`, `Prepared`), `select.ts` (`select()`), `cte.ts`, `write.ts`, `build.ts` (rows → instances), `db.ts` (pools, transactions, locks), `migrations.ts`, `cli.ts` |
| `bindings/node/` | Rust crate `orm-node` (napi-rs) on top of `orm-engine`: `convert.rs` (JS ↔ values, strict), `js.rs`, `lib.rs` |
| `core/src/codegen/typescript.rs` | `orm generate typescript`: `models.ts` from the schema |
| `js/test/` | End-to-end tests (Node and Bun), SQL shape tests, `typing/check.ts` (compile-time checks) |

Node 20+ or Bun. Build and test:

```bash
cd js && npm install
npm run build:native        # cargo build of bindings/node → js/orm.node
npm test                    # Node (node:test), against ORM_TEST_DATABASE_URL
npm run test:bun            # the same suites under Bun
npm run typecheck           # tsc over the package, the typing checks and the example
```

## Models: the generated module

`npx orm generate [-o models.ts] [--import orm]` (or `orm generate typescript ...`)
writes one module with the compiled schema and a type for everything:

* `User`: the row type (an instance). Its fields are read-only and camelCase
  (`created_at` → `createdAt`; the column name stays the same).
* `User` (the value): the model, with `User.objects` (a `QuerySet<User>`), the columns
  (`User.email`, a `Column<string, "User">`), relation paths (`User.posts.views`),
  `User._meta`, `User.DoesNotExist` and `User.MultipleObjectsReturned`.
* `UserInsert`, `UserUpdate`, `UserUpdateRow`: the shapes `insert()`, `update()` and
  `updateMany()` take.
* Enums are string literal unions (`Role = "member" | "editor" | "admin"`) with a value
  object of the same name (`Role.admin`).

Scalar types follow Prisma's choices:

| Schema | TypeScript |
|---|---|
| `Int`, `Float` | `number` |
| `BigInt` | `bigint` (filters also take a `number`) |
| `Decimal` | `Decimal` from decimal.js, re-exported by `orm` (filters also take a `number` or a string) |
| `DateTime` | `Date`, with millisecond precision. Postgres keeps microseconds, so the last three digits are lost on read |
| `Date` | `Date` at 00:00 UTC |
| `Json` | `JsonValue` |
| `Uuid` | `string` |
| `String[]`, ... | `string[]`, ... |

Field names that would clash with the instance API are rejected by the generator.
The reserved names are `objects`, `_meta`, `DoesNotExist`, `MultipleObjectsReturned`,
`pk`, `update`, `delete`, `refresh`, `toJSON`, `constructor`, `toString` and `then`.

Without code generation, `define(schemaIR)` or `loads(schemaText)` builds the same models
at runtime, but untyped.

## Queries

Query sets are lazy and immutable. Nothing runs until a terminal method is called:

| Method | Gives |
|---|---|
| `.all()` | `Promise<R[]>` |
| `.first()` / `.last()` | `Promise<R \| null>` |
| `.get(...conditions)` | `Promise<R>`; throws `User.DoesNotExist` / `User.MultipleObjectsReturned` |
| `.count()` / `.exists()` | `Promise<number>` / `Promise<boolean>` |
| `.inBulk(keys?, { field })` | `Promise<Map<key, R>>` |
| `.batches(size)` / `.iterate(size)` | async generators that walk the primary key (keyset pagination) |
| `.sql()` | the SQL with values inlined, for reading |

A query set is deliberately not a thenable: building one never touches the database, and
every place a query runs carries a terminal method.

The builders are `filter(...)`, `exclude(...)`, `orderBy(...)`, `limit(n)`, `offset(n)`,
`slice(start, end)`, `selectRelated(...)`, `prefetchRelated(...)`, `lock(...)`,
`using(db)`, `select({...})`, `join(cte, on)`, `from(cte)` and `cte(name)`.

Comparisons are methods: `.eq .ne .lt .lte .gt .gte .between .in .notIn .isNull
.isNotNull`. String columns also have `.contains .icontains .startsWith .endsWith .like
.ilike`, and arrays have `.has .hasAll .hasAny .containedBy`. Arithmetic is `.add .sub
.mul .div`, and orderings are `.asc() / .desc()`. Conditions combine with `and()`,
`or()`, `not()`, or the methods of the same names. A boolean column is a condition by
itself (`filter(Post.published)`).

The types check the following:

* A condition reads only the query's model: `Post.objects.filter(User.email.eq("x"))`
  is an error.
* Values must have the column's type. `null` is accepted only by nullable columns, and
  enum values must be members.
* A to-many column (`User.posts.views`) works in filters, where it becomes `EXISTS`, but
  not in `orderBy()` or as a plain `select()` column, because those would repeat rows.
  Aggregates over it are fine.

### Relation filters

The semantics are Django's, with no duplicate rows. A to-many hop becomes a correlated
`EXISTS`. Conditions in one `filter()` call must hold for the same related row, while
separate calls are independent. `exclude()` is `NOT EXISTS`.

### Loading related objects

* `selectRelated(Comment.post.author, Comment.author)` follows to-one relations with
  LEFT JOINs. The row type gains the loaded objects: `c.post.author.name` is a `string`,
  and `c.author` is `User | null` because the key is nullable. Reading an unloaded
  relation is a type error, and at runtime it throws `NotLoaded`.
* `prefetchRelated(User.posts.comments, User.profile)` runs one extra `IN` query per hop.
  To-many results are in `u.posts.cached`, and to-one results are on the attribute.
* `new Prefetch(User.posts, Post.objects.filter(...).orderBy(...).limit(2), { toAttr: "top" })`
  sets a custom query (filtered, nested, or sliced per parent) and a typed target
  attribute (`u.top: Post[]`).

Related sets: `user.posts.all()`, `.filter()`, `.count()`, and `.insert({...})`, where the
key is filled in. Many-to-many sets also have `post.tags.add(tag, ...)`, `.remove()`,
`.clear()` and `.set([...])`.

### Prepared queries

`param("name")` is a placeholder. A query set with placeholders can't run directly (it
is a type error). `.prepare()` plans it once, and each call passes the values, which are
typed from the columns they meet:

```ts
const q = Post.objects.filter(Post.authorId.eq(param("author"))).limit(param("n")).prepare();
await q.all({ author: 1n, n: 10 }); // { author: bigint | number; n: number }
```

## Columns and aggregates: `select()`

`select({ alias: expr | Model })` gives object rows typed per key. `func` has `count sum
avg min max coalesce lower upper length abs now cardinality` plus the window functions.
The result types follow the SQL: `count` is a `bigint`, `sum(Int)` is `number | null`,
`sum(Decimal)` is `Decimal | null`, and `avg` is `number | null`. A column read through a
nullable relation becomes nullable.

Other `Select` methods:

* `.groupBy(...)`, `.having(...)`, `.distinct()` and `.orderBy(...)`.
* The terminals `.all()`, `.first()`, `.one()`, `.scalar()` and `.scalars()`.
  `scalar()` and `scalars()` type-check only for one column.

## Subqueries

* `exists(qs)`.
* `qs.select({ t: Post.title }).limit(1).asScalar()` for a scalar value.
* `col.in(select)` for a one-column select.
* `outer(User.id)` refers to the nearest enclosing `User` query. It can sit two or more
  levels down.

A query set that holds an `outer()` reference can't run on its own. Trying to is a type
error, and also a `QueryError` at runtime.

## Window functions

`func.rowNumber().over({ partitionBy, orderBy, rows })`, and the other window functions:
`rank denseRank percentRank cumeDist ntile lag lead firstValue lastValue nthValue`.
Aggregates also take `.over(...)`. `window({...})` defines a named window that queries
can share. A window function that has no `.over()` is a type error.

## CTEs: `WITH`

```ts
const totals = Post.objects.select({ authorId: Post.authorId, n: func.count() }).groupBy(Post.authorId).cte("totals");
await User.objects.join(totals, totals.c.authorId.eq(User.id)).select({ name: User.name, n: totals.c.n }).all();

const ranked = Post.objects.select({ post: Post, rank: func.rowNumber().over({ orderBy: Post.views }) }).cte("ranked");
await Post.objects.from(ranked).filter(ranked.c.rank.lte(3)).all();

const chain = User.objects.filter(User.id.eq(1)).cte("chain", { recursive: (c) => User.objects.filter(User.id.eq(c.c.id.add(1))) });
```

`cte.c.<column>` is typed. Using a CTE column in a query that doesn't read the CTE is a
type error. `from()` accepts only a CTE that has the model's columns.

## Writes

Writes run when they are called and return a `Promise`:

* `insert(row, { onConflict, doNothing | doUpdate | set })` gives the stored row (or
  `null` with `doNothing`). `insertMany(rows, ...)` is the bulk version.
* `qs.update({...}, { returning })` gives a count, or the rows when `returning` is set.
* `qs.delete()`.
* `updateMany(rows, { batchSize, returning })` does a bulk update by primary key with
  `UPDATE ... FROM (VALUES ...)`, falling back to `CASE`. All its batches run in one
  transaction.
* On an instance: `post.update({...})` (refreshed from `RETURNING`), `post.delete()` and
  `post.refresh()`.

Values in `update()` can be expressions over the same model (`Post.views.add(1)`).
`excluded(col)` reads the proposed row in an upsert. A related row can stand in for its
key (`{ author: alice }`); passing both the row and the key is a type error.

### Transactions

`db.transaction(async () => {...})` commits when the callback resolves, and rolls back
when it throws. The current transaction follows the async call chain through
`AsyncLocalStorage`, so queries inside it need no handle. Nested calls are savepoints.
A transaction that is never finished is rolled back when it is garbage-collected.

### Locks

* `qs.lock({ exclusive, nowait, skipLocked })` adds `FOR UPDATE` / `FOR SHARE` on the
  model's rows. Setting both `nowait` and `skipLocked` is a type error.
* `db.lock(key, { exclusive, nowait })` takes a transaction-scoped advisory lock. String
  keys hash the way Python's do (BLAKE2b with an 8-byte digest), so both languages lock
  the same name.

Both throw `TransactionRequired` when called outside a transaction.

## Errors

Errors map to classes with Python's names: `ORMError`, plus `DatabaseError`,
`IntegrityError`, `LockNotAvailable`, `QueryError`, `SchemaError`, `NotConnected`,
`NotLoaded`, `TransactionRequired`, `DoesNotExist`, `MultipleObjectsReturned` and
`MigrationError`. Values of the wrong type throw a `TypeError` before any SQL runs.

## Migrations and the CLI

`npx orm` is the one `orm` command line (Rust, `cli/`), run through the addon: the same
program as `python -m orm` and the standalone `orm` binary, with the same commands
(`check`, `generate`, `makemigrations [--check]`, `sqlmigrate`, `migrate`, `rollback`,
`showmigrations`; see [`schema.md`](schema.md#migrations)). Under `npx`, `generate` writes
TypeScript and `package.json`'s `"orm"` key is read first. `Migrations` and `Migrator`
are the programmatic API; they call the same Rust migrator (`engine/src/migrate.rs`).

## Decisions

* **Explicit terminal methods** (`.all()` rather than `await qs`), so it is always
  visible where a query runs.
* **`bigint` and decimal.js**, following Prisma. `BigInt` keys never lose precision, and
  decimals stay exact on the wire.
* **`Date` for timestamps.** It gives millisecond precision. Temporal can replace it once
  runtimes ship it.
* **`_meta`** rather than `meta` holds a model's metadata, because `meta` is a common
  column name.
* **Strict conversions** in the addon (`bindings/node/src/convert.rs`). A `number` for a
  `BigInt` column must be a safe integer, an `Int` must fit in 32 bits, and a `Decimal`
  must be finite. Anything else is a `TypeError`, never a silent coercion.
