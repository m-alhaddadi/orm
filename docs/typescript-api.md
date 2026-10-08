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

const users = await User.objects.filter(User.posts.createdAt.lt(yesterday));
const rows = await User.objects
  .select({ name: User.name, posts: func.count(User.posts), popular: exists(Post.objects.filter(Post.authorId.eq(outer(User.id)), Post.published)) });
// { name: string; posts: bigint; popular: boolean }[]
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
| `DateTime` | `Date`, with millisecond precision; the microseconds below it stay in a hidden property (see below) |
| `Date` | `Date` at 00:00 UTC |
| `Json` | `JsonValue` |
| `Uuid` | `string` |
| `String[]`, ... | `string[]`, ... |

Field names that would clash with the instance API are rejected by the generator.
The reserved names are `objects`, `_meta`, `DoesNotExist`, `MultipleObjectsReturned`,
`pk`, `update`, `delete`, `refresh`, `toJSON`, `constructor`, `toString` and `then`.

Without code generation, `define(schemaIR)` or `loads(schemaText)` builds the same models
at runtime, but untyped.

## Custom query-set methods

Named filters (Django's custom managers) go in a subclass of `QuerySet`, in your own module:

```ts
// queries.ts
import { QuerySet } from "orm";
import { Post, type PostSpec } from "./models.js";

export class PostQueries extends QuerySet<PostSpec> {
  published(): this {
    return this.filter(Post.published.eq(true)) as this;
  }

  popular(views = 100): this {
    return this.filter(Post.views.gte(views)) as this;
  }
}
```

Generate the models with the class, as `specifier#Export` (the specifier is relative to `models.ts`):

```bash
npx orm generate --query-set Post=./queries.js#PostQueries
```

or in `package.json`: `"orm": { "querySets": { "Post": "./queries.js#PostQueries" } }`.

Then `Post.objects` is a `PostQueries`, and the methods chain with every builder method in both orders:
`await Post.objects.published().filter(Post.authorId.eq(1n)).popular()`.
`PostSpec` gets `queries: PostQueries`, and builder methods return `QuerySetOf<PostSpec, ...>`, a query set with those methods.

* `models.ts` imports the module as a namespace and calls `useQuerySet(Post, () => _q0.PostQueries)`.
  The class is read on the first use of `Post.objects`, so `queries.ts` can import `models.ts` without an error from the import cycle.
  `useQuerySet(Model, cls)` also takes the class itself, for models from `define()` or `loads()`.
* Relation sets have the methods too: `user.posts.published()`, `post.tags.<method>()`, typed in the generated row type.
* `new Prefetch(User.posts, Post.objects.published())` uses them for related rows.
* A method returns `this` with a cast. TypeScript can not change the type arguments of `this`, so a custom method gives the class's own row type:
  call custom methods before `selectRelated()`, `prefetchRelated()` or `only()`, whose row types they would drop.

## Queries

Query sets are lazy and immutable: building one never touches the database. Awaiting
one runs it (a query set is a *thenable*, like Prisma's and Drizzle's queries, and like
`await qs` in Python), and so do the terminal methods:

| Method | Gives |
|---|---|
| `await qs` / `for await (const u of qs)` | `R[]`, cached per query set (below) |
| `.all()` | `Promise<R[]>`, always a fresh query |
| `.first()` / `.last()` | `Promise<R \| null>` |
| `.get(...conditions)` | `Promise<R>`; throws `User.DoesNotExist` / `User.MultipleObjectsReturned` |
| `.count()` / `.exists()` | `Promise<number>` / `Promise<boolean>` |
| `.inBulk(keys?, { field })` | `Promise<Map<key, R>>` |
| `.batches(size)` / `.iterate(size)` | async generators that walk the primary key (keyset pagination) |
| `.paginate({ first, after })` / `.paginate({ last, before })` | one page by keyset: `{ items, hasNext, hasPrevious, nextCursor, previousCursor }` |
| `.sql()` | the SQL with values inlined, for reading |

**The result cache.** A query set runs on its first `await`; awaiting the same query set
again gives the same rows (a new array each time) without another query, and concurrent
awaits share one run, as with Django's result cache. Every builder returns a new query
set with an empty cache, and `.all()` always queries, so `await qs.all()` is the way to
re-read. A failed run isn't cached. `User.objects` itself lives as long as the model, so
`await User.objects` queries every time. Writes don't clear caches: re-read with a new
query set or `.all()`. `select()` caches the same way.

Awaiting a query set that can't run on its own (`param()` placeholders, `outer()`
references) is a type error. One thing to keep in mind with thenables: an `async` function
that *returns* a query set resolves it, so its caller gets rows. Return it from a plain
function to pass the query on.

The builders are `filter(...)`, `exclude(...)`, `orderBy(...)`, `limit(n)`, `offset(n)`,
`slice(start, end)`, `selectRelated(...)`, `prefetchRelated(...)`, `lock(...)`,
`using(db)`, `select({...})`, `join(cte, on)`, `from(cte)` and `cte(name)`.

Comparisons are methods: `.eq .ne .lt .lte .gt .gte .between .in .notIn .isNull
.isNotNull`. String columns also have `.contains .icontains .startsWith .endsWith .like
.ilike .concat`, and arrays have `.has .hasAll .hasAny .containedBy .element`. Arithmetic
is `.add .sub .mul .div`, and orderings are `.asc() / .desc()`. `.asc({ nulls: "first" })` and `.desc({ nulls: "last" })` place NULLs. `orderBy` also takes field names, with
`-` for descending: `orderBy("-createdAt", "id")`. A name that is not a field of the
model is a type error and a `TypeError`. Conditions combine with `and()`,
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

### Partial rows, OR of query sets, column paths

* `only(Comment.body, Comment.post.title)`: a column through to-one relations loads the relation with `selectRelated` and trims the joined instance to the given fields.
  Without a column of the model itself, its instances keep only their hidden keys. A to-many path throws. The row type does not show the joined relation; cast it.
* `qs1.or(qs2)`: one query set with the filter `(filters of qs1) OR (filters of qs2)`. `qs2` sets nothing but filters, neither is sliced, and each side has at most one `filter()`/`exclude()` call.
* `new Prefetch(User.posts, Post.objects.orderBy("-views"), { toAttr: "best", one: true })` stores the first related row, or `null`, in `user.best` (typed `Post | null`). It needs `toAttr` and takes no slice.
* `column(Bundle, "items.product.title")` is the column a dotted path of TypeScript names gives, for adapters that map request names to columns.

### Cursor pagination

```ts
const page = await Post.objects.orderBy("-createdAt").paginate({ first: 20 });
const next = await Post.objects.orderBy("-createdAt").paginate({ first: 20, after: page.nextCursor });
const back = await Post.objects.orderBy("-createdAt").paginate({ last: 20, before: page.previousCursor });
```

The rules are the same as Python's `paginate()` (see [`python-api.md`](python-api.md)).
A nullable order column needs `{ nulls }`: `Post.rank.desc({ nulls: "last" })`.
A cursor from Python works in TypeScript for the same schema and order, and the other way.
A `Date` holds milliseconds. A `DateTime` value read from the database keeps its microseconds in a hidden property, so its cursor and `filter(Post.createdAt.eq(row.createdAt))` find the exact row. `new Date(row.createdAt)` drops them, and so does a change of the `Date` (`setTime()`, `setUTCHours()`, ...).
`{ first, before: null }` and `{ last, after: null }` are accepted, as in Python.

### Relation filters

The semantics are Django's, with no duplicate rows. A to-many hop becomes a correlated
`EXISTS`. Conditions in one `filter()` call must hold for the same related row, while
separate calls are independent. `exclude()` is `NOT EXISTS`.

A `belongsTo` relation compares with an instance of its target:
`Post.author.eq(alice)` is `Post.authorId.eq(alice.id)` (no join), `.ne(alice)` the
opposite, and `.eq(null)` is `IS NULL`. The types allow `eq` / `ne` only on to-one
relations and only with the target's row type. At runtime, other relation kinds, other
objects and an instance without a key throw `TypeError`.

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

* `await prefetch(instances, ...paths)` loads relations onto instances you already have, with the same paths and `Prefetch` objects.
  Only the prefetch queries run: the keys come from the instances. A last `{ using: db }` argument names the database.
  The row type does not change; read the relations as `cached` or cast.

Related sets: `await user.posts`, `.filter()`, `.count()`, and `.insert({...})`, where the
key is filled in. Many-to-many sets also have `post.tags.add(tag, ...)`, `.remove()`,
`.clear()` and `.set([...])`. An options object as the last argument of `add()` (or
the second argument of `set()`) gives other fields of the new join rows:
`post.tags.add(tag, { throughDefaults: { position: 1 } })`. Existing links keep their
values.

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
avg min max coalesce lower upper length abs now cardinality unnest concat trim ltrim rtrim
replace substr strpos` plus the window functions. `func.concat(a, " ", b)` reads a `null`
part as an empty string; `a.concat(b)` (`a || b`) is `null` when either side is `null`.
`Profile.links.element(1)` is SQL's 1-based element access (`null` out of range), and
`func.unnest(...)` is valid only as a `select()` column; SQLite has no array columns.
`func.case([cond, value], ..., { default })` is `CASE WHEN ... END`: the value of the
first true condition, else `default` (`null` without one). It works in `select()`,
`filter()`, `orderBy()` and `update()`, for example
`func.sum(func.case([Post.published, 1], { default: 0 }))` for a conditional count.
Aggregates take `{ filter: cond }` (`FILTER (WHERE cond)`), for example
`func.count({ filter: Post.published })` or
`func.count(User.posts, { filter: User.posts.published })`.
JSON columns: `Doc.meta.get("author", "name").eq("Ann")` (`meta -> 'author' -> 'name'`,
compared as JSON), `.asText()` for the last step as text (`->>`),
`Doc.meta.jsonContains({ kind: "post" })` (`@>`), `.jsonContainedBy(...)` (`<@`),
`.hasKey("tags")` (`?`) and `update({ meta: Doc.meta.jsonMerge({ seen: true }) })` (`||`).
Keys are strings, indexes 0-based integers. PostgreSQL only.
Full-text search (PostgreSQL only): `func.toTsvector("english", Post.body).matches("running dogs")`
(`@@`, a plain string is `plainto_tsquery` with the vector's configuration),
`func.toTsquery`, `func.plaintoTsquery`, `func.websearchToTsquery` and `func.tsRank(vector, query)`.
The configuration is written into the SQL as `'english'::regconfig`, so it matches a GIN
index on the same expression: `@@index([sql("to_tsvector('english', body)")], type: Gin)`.
The result types follow the SQL: `count` is a `bigint`, `sum(Int)` is `number | null`,
`sum(Decimal)` is `Decimal | null`, and `avg` is `number | null`. A column read through a
nullable relation becomes nullable.

Other `Select` methods:

* `.groupBy(...)`, `.having(...)`, `.distinct()` and `.orderBy(...)`.
* `await sel` (cached, like a query set) and the terminals `.all()`, `.first()`, `.one()`,
  `.scalar()` and `.scalars()`.
  `scalar()` and `scalars()` type-check only for one column.

## Subqueries

* `exists(qs)`.
* `qs.select({ t: Post.title }).limit(1).asScalar()` for a scalar value.
* `col.in(select)` for a one-column select.
* `outer(User.id)` refers to the nearest enclosing `User` query. It can sit two or more
  levels down. `outer(Post.author.name)` reads a related row through to-one relations;
  a to-many hop is a type error and a `QueryError`.

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
await User.objects.join(totals, totals.c.authorId.eq(User.id)).select({ name: User.name, n: totals.c.n });

const ranked = Post.objects.select({ post: Post, rank: func.rowNumber().over({ orderBy: Post.views }) }).cte("ranked");
await Post.objects.from(ranked).filter(ranked.c.rank.lte(3));

const chain = User.objects.filter(User.id.eq(1)).cte("chain", { recursive: (c) => User.objects.filter(User.id.eq(c.c.id.add(1))) });
```

`cte.c.<column>` is typed. Using a CTE column in a query that doesn't read the CTE is a
type error. `from()` accepts only a CTE that has the model's columns.

## Writes

`insert()` and `insertMany()` give a statement that runs when it is awaited, once, in
the async context where it was built (its transaction, `allowWrites` and `scope`).
The other writes run when they are called and return a `Promise`:

* `insert(row)` gives the stored row. A field left out gets its `@client_default`
  (filled natively, as in Python), else the database default; an explicit value, also
  `null`, wins.
* `insertMany(rows)` and every `.onConflict(...)` give the number of rows inserted or
  updated (no `RETURNING` in the SQL); `.returning()` gives the rows. One row with
  `{ update: true }` gives the row; with `{ update: false }` (or a `boolean` that is not
  a literal) the row or `null`. Many rows give an array in input order, without the
  rows that `{ update: false }` skipped.

  ```ts
  const n = await Post.objects.insertMany(rows);
  const posts = await Post.objects.insertMany(rows).returning();
  const bob = await User.objects.insert(row).onConflict(User.email, { update: true }).returning();
  const maybe = await User.objects.insert(row).onConflict(User.email, { update: false }).returning(); // null if it existed
  await Post.objects.insertMany(rows).onConflict(Post.slug, {
    update: true,
    updateFields: [Post.title],
    updateValues: { views: Post.views.add(excluded(Post.views)) },
  });
  ```
* `onConflict(columns, { update, updateFields, updateValues, where })`: `columns` are
  the column(s) of one unique constraint. `update` is required. `update: false` keeps
  the existing row (`DO NOTHING`). `update: true` updates it: `updateFields` copy the
  proposed values, `updateValues` set plain values or expressions (keys are field
  names). Both together update the union; a field in both throws. The fields you don't
  name keep their values. With neither option, every field given to the insert except
  the conflict columns is overwritten. An empty `updateFields` or `updateValues`, and
  either one with `update: false`, throw `TypeError`.
* `onConflict(columns, { where: cond, update })` picks a partial unique index:
  `ON CONFLICT (...) WHERE cond`. The condition must match the index predicate
  without parameters (`Task.deletedAt.isNull()`, a boolean column).
* `insertMany(rows, { copy: true })` loads the rows with Postgres `COPY` (binary) and
  gives the row count. A duplicate key stops the whole load. A field must be set in
  every row or in none. `onConflict`, `batchSize` and SQLite are rejected.
* `getOrInsert(lookup, { defaults })` gives `[row, created]`: the row that matches
  `lookup`, or a new row of `lookup` and `defaults`. The insert is
  `ON CONFLICT (lookup) DO NOTHING`, so concurrent calls give one row. The lookup
  fields must be the fields of one unique constraint; a `null` lookup value throws.
* `insertMany` splits the rows so that no statement has more parameters than the
  database accepts (65,535 on Postgres, 32,766 on SQLite). `{ batchSize: n }` sets a
  lower number of rows for each statement. All the statements run in one transaction.
* `qs.update({...}, { returning })` gives a count, or the rows when `returning` is set.
* `qs.delete()`.
* `updateMany(rows, { batchSize, returning })` does a bulk update by primary key with
  `UPDATE ... FROM (VALUES ...)`, falling back to `CASE`. All its batches run in one
  transaction.
* On an instance: `post.update({...})` (refreshed from `RETURNING`), `post.delete()` and
  `post.refresh(...fields)`.
* With the column-role extensions (`schema-extensions.md`): `@timestamps.updated_at` and
  `@locking.version` are set on each update; on a `@soft_delete.deleted_at` model,
  `delete()` soft-deletes, and `hardDelete()`, `undelete()`, `allWithDeleted()` and
  `deletedOnly()` exist (instances: the `SoftDeletable` type). A stale versioned
  instance write throws `VersionConflict`.

Values in `update()` can be expressions over the same model (`Post.views.add(1)`).
`excluded(col)` reads the proposed row in an upsert. A related row can stand in for its
key (`{ author: alice }`); passing both the row and the key is a type error.

### Transactions

`db.transaction(async () => {...})` commits when the callback resolves, and rolls back
when it throws. The current transaction follows the async call chain through
`AsyncLocalStorage`, so queries inside it need no handle. Nested calls are savepoints.
A transaction that is never finished is rolled back when it is garbage-collected.

`await db.onCommit(fn)` calls `fn()` after the outermost transaction on `db` commits, and awaits a promise result.
A rollback drops the callback. A rolled-back savepoint drops only the callbacks registered inside it.
Outside a transaction, `fn()` runs at once.
Callbacks run in registration order, outside the transaction.
An error in a callback rejects the `transaction()` promise, and the later callbacks do not run; the transaction is already committed.

### Protected writes

`@@protected_write` is an application-level check in the ORM. It does not protect the database.
Raw SQL (`db.execute`), migrations, other ORM processes without this schema, and other database clients can still write.
For database-level protection, use `@@trigger` or database grants.

```ts
await allowWrites([Post], async () => { await post.update({ title: "new" }); });
await Post.objects.filter(...).update({ title: "x" });   // throws WriteProtected
```

Every ORM write to a `@@protected_write` model fails outside `allowWrites(models, fn)` with `WriteProtected`:
`insert`, `insertMany`, upserts, `update`, `updateMany`, `delete`, instance writes and composed writes.
The check is on the table that the SQL writes, so `post.tags.add()` needs `allowWrites([PostTag], ...)` when `PostTag` is protected.
The scope follows the async call chain through `AsyncLocalStorage`, like the transaction: work started inside `fn` gets it.
A nested call adds its models to the outer ones. `allowWrites` starts no transaction and gives what `fn` gives.
A write reads the scope when it is called, not when it is awaited (Python reads it at the `await`).
`prepareInsert` and `prepareUpdate` also check protection, so a file field uploads nothing for a rejected write.
Wrong arguments give a rejected promise with a `TypeError`.
See `docs/schema.md`, "Protected writes".

### Locks

* `qs.lock({ exclusive, nowait, skipLocked })` adds `FOR UPDATE` / `FOR SHARE` on the
  model's rows. Setting both `nowait` and `skipLocked` is a type error.
* `post.refresh(...fields, { lock: true, exclusive, nowait, skipLocked })` reloads the row
  with the same lock and gives `true`. A refresh of some fields locks the whole row. With
  `skipLocked`, a row locked elsewhere (or deleted) gives `false` and leaves the
  instance unchanged; otherwise a missing row throws `DoesNotExist`. Lock options
  without `lock: true` are a type error and throw `TypeError`.
* `db.lock(key, { exclusive, nowait })` takes a transaction-scoped advisory lock. String
  keys hash the way Python's do (BLAKE2b with an 8-byte digest), so both languages lock
  the same name.

* `db.lock(key, { session: true, timeout: 5 }, async () => {...})` is a session advisory
  lock: it holds the lock while the function runs, with no transaction, on a pool
  connection of its own, and gives what the function gives. It waits at most `timeout`
  seconds (no limit when absent, not at all with `nowait`) and throws `LockNotAvailable`
  when another session still holds the lock. The lock is released when the function
  settles; when the unlock fails, the connection is closed, so the server releases it.

The transaction-scoped forms throw `TransactionRequired` when called outside a transaction.

### Read replicas

`connect(primaryUrl, { replicas: [replica1Url, replica2Url] })` sends reads (`select`, `count`, `exists`, prepared queries) outside a transaction to the next replica, in turn.
Writes, raw `db.execute`, migrations, and every statement inside `db.transaction()` go to the primary.
`db.primary` is a view of the database without its replicas; it shares the transactions of `db`.
`qs.using("primary")` is `qs.using(<the query set's database>.primary)`, resolved when it is called.
A replica can lag behind the primary: to read your own write, read in the same transaction or use `using("primary")`.
There are no health checks or failover. `maxConnections` applies to each pool, and `db.close()` closes all of them.

### Tenants and row-level security

`await db.tenant(shop.id, async () => {...})` runs `SELECT set_config('app.tenant', '<id>', true)` (`SET LOCAL`) at the start of every transaction on `db` in the function, so RLS policies can read `current_setting('app.tenant', true)`.
A statement outside a transaction runs in a transaction of its own (four round trips instead of one, estimated).
A transaction that is already open keeps its setting, and the setting ends with each transaction.
Replicas get the same setting. SQLite throws `QueryError`.
`scope({ shop }, fn)` is the application-side filter (see `docs/selection-and-defaults.md`, "Scope values").

### Finding N+1 queries: `debug`

The ORM never loads a relation by itself, so an N+1 comes from explicit code: a `loadX()` call or a query in a loop.
`debug.nPlusOne` finds it:

```ts
import { debug } from "orm";

await debug.nPlusOne(async () => {
  for (const c of customers) await c.loadPerson();
}, { threshold: 5, fail: true });
// NPlusOne: 20 queries with one shape `SELECT ... FROM "person" WHERE "person"."id" = $1 ...`
//   at src/views.ts:42; use selectRelated(Customer.person)
```

* The scope counts the statements of `fn` by shape: the SQL with placeholders, without values.
  It reads the same events as `db.onQuery`, so prefetch queries and `updateMany` batches count one by one.
  Work that `fn` starts counts too, and so do the queries of an inner scope.
  An inner scope cannot raise the threshold of an outer scope; the outer scope also counts the inner queries.
  The pages of one ORM loop (`batches()`, `iterate()`, the chunks of `inBulk()`) count as one query when they have one shape.
* When `fn` resolves, a shape that ran more than `threshold` times (default 5) throws `debug.NPlusOne` with `fail: true`, or emits an `NPlusOneWarning` process warning.
  `error.report` has each shape, its SQL, its count, the call site of its first query and the fix.
* The fix is `selectRelated(...)` for a repeated `loadX()`, and `prefetchRelated(...)` for a repeated unchanged to-many or many-to-many query (`post.comments.all()`, `post.tags.all()`).
* The call site and the SQL text are captured only inside the scope.
  Outside it, each query pays one `AsyncLocalStorage` read (about 2 ns, measured).
* In tests, `await debug.expectNoNPlusOne(fn, { threshold })` throws `NPlusOne` when `fn` sends an N+1.
  Without source maps (`node --enable-source-maps`), the call site is a line of the compiled JavaScript.

### Raw SQL and query plans

```ts
const rows = await db.fetch("SELECT id, email FROM users WHERE created_at > $1 AND name = $2", since, "Ann");
// [{ id: 7n, email: "ann@example.com" }]
console.log(await Post.objects.filter(Post.authorId.eq(7n)).explain());
console.log(await Post.objects.filter(Post.authorId.eq(7n)).explain({ analyze: true }));
```

`db.fetch(sql, ...params)` gives an array of objects by column name, and `qs.explain({ analyze })` gives the plan as text.
They work as in Python (`docs/python-api.md`, "Raw SQL and query plans").
A parameter's type comes from its JS value: `bigint` and integer numbers are `bigint`, strings are `text`, plain objects and arrays are JSON, and `Date` is `timestamptz`.
`bigint` columns come back as `bigint`, and `timestamptz` and `date` as `Date`.

### Query hooks and OpenTelemetry

`db.onQuery(hook)` calls `hook(event)` after each statement that the database runs, and gives a function that removes the hook:

```ts
const off = db.onQuery((e: QueryEvent) => {
  if (e.duration > 100) console.warn(`${e.duration.toFixed(0)} ms, ${e.rows} rows: ${e.sql}`);
});
```

* `QueryEvent` has `sql` (the SQL with placeholders, never the values), `start` (Unix milliseconds), `duration` (milliseconds), `rows` and `error` (`null` on success).
* The events, the timing and the context are the same as in Python (`docs/python-api.md`, "Query hooks and OpenTelemetry").
  The hook runs in the async context of the caller, after the ORM call ends. An error that the hook throws rejects that call.

`instrument(db, { tracer })` from `orm/otel` gives each statement a client span with the same name, attributes and status as in Python, and gives a function that stops the spans.
Without `tracer`, it loads `@opentelemetry/api` (an optional peer dependency) and uses `trace.getTracer("orm")`.

### Hooks for packages

A package that changes writes and reads from outside the ORM (for example
`@orm/file-storage`) uses these public methods, not internal names:

* `qs.prepareInsert(values)` checks one insert as `insert()` does (required fields,
  conversion, native planning) without SQL or I/O. `prepared.execute(values?)` inserts it.
* `qs.prepareUpdate(values)` checks `qs.update(values)` the same way. `prepared.unique`
  is true when the engine proves that the filters pin one row by a non-null primary
  key or unique field. `prepared.execute(values?, { returning })` runs it.
* `Model._meta.addRowDecoder(decode)` runs `decode(row)` on each instance that a query
  or write returns. A partial instance has only its loaded fields as own properties.

## Model metadata and test factories

The ORM has no factory library. It gives the two things a factory library needs:

* `describe(Model)`: plain data about the model, with TypeScript (camelCase) names.
  `fields` gives per field the name, column, schema type, `nullable`, `array`, `enum` (the enum's name), `maxLength`, `primaryKey`, `unique`, `default` (`"database"`, `"client"` or `null`) and `insert` (whether `insert()` takes it).
  `relations` gives the kind, the target model, the `from`/`to` fields, the `through` model and `nullable`. `unique` lists the unique keys, the primary key first.
* The insert path: `await Model.objects.insert(values)`, typed by the generated `PostInsert`.

With fishery, the factory builds `PostInsert` values and `onCreate` inserts them:

```ts
const postFactory = Factory.define<PostInsert, {}, Post>(({ sequence, onCreate }) => {
  onCreate((values) => Post.objects.insert(values));
  return { title: `post ${sequence}`, body: "...", authorId: 1n };
});
const post = await postFactory.create();
```

## Errors

Errors map to classes with Python's names: `ORMError`, plus `DatabaseError`,
`IntegrityError`, `LockNotAvailable`, `QueryError`, `SchemaError`, `NotConnected`,
`NotLoaded`, `TransactionRequired`, `DoesNotExist`, `MultipleObjectsReturned`,
`MigrationError`, `WriteProtected` and `VersionConflict`. Values of the wrong type throw a `TypeError` before any SQL runs.

A `DatabaseError` (and its subclasses) has `sqlstate`, `constraint` and `detail`, each `null` when the database did not give it.
They are the same as in Python (see `docs/python-api.md`, "Database errors"): SQLite constraint failures get the Postgres SQLSTATE, and only Postgres gives `constraint` and `detail`.

```ts
try {
  await User.objects.insert({ email, name });
} catch (e) {
  if (!(e instanceof IntegrityError) || e.constraint !== "users_email_key") throw e;
  // the email is taken
}
```

The ORM does not retry a transaction.
Run the whole transaction again on a serialization failure (`40001`) or a deadlock (`40P01`):

```ts
async function withRetry<T>(fn: () => Promise<T>, attempts = 3): Promise<T> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await db.transaction(fn);
    } catch (e) {
      const retry = e instanceof DatabaseError && (e.sqlstate === "40001" || e.sqlstate === "40P01");
      if (!retry || attempt === attempts - 1) throw e;
      await new Promise((r) => setTimeout(r, 50 * 2 ** attempt));
    }
  }
}
```

## Migrations and the CLI

`npx orm` is the one `orm` command line (Rust, `cli/`), run through the addon: the same
program as `python -m orm` and the standalone `orm` binary, with the same commands
(`check`, `generate`, `makemigrations [--check]`, `sqlmigrate`, `migrate`, `rollback`,
`showmigrations`, `pull`, `baseline`, `drift`; see [`schema.md`](schema.md#migrations)).
Under `npx`, `generate` writes TypeScript and `package.json`'s `"orm"` key is read first.
`Migrations` and `Migrator` are the programmatic API; they call the same Rust migrator
(`engine/src/migrate.rs`). `await pull(db)` reads a live database into a `Pulled`
(`schema`, `gaps`, `differences`, `write(path)`), `migrator.baseline()` marks the first
migration applied without running it, and `migrator.drift()` gives the `Drift`
(`migration`, `steps`, `gaps`) between the database and the newest snapshot
(see [`schema.md`](schema.md#adopting-a-live-database-pull-baseline-drift)). A migration
may hold a `data.ts` with `export async function run(db)`, run by `migrator.upgrade()` and
`npx orm migrate` in the migration's transaction (see [`schema.md`](schema.md#data-migrations)).

## Decisions

* **Awaitable query sets** (`await qs`), as in Python, Prisma and Drizzle, with a result
  cache so one query set queries once; `.all()` is the explicit fresh query.
* **`bigint` and decimal.js**, following Prisma. `BigInt` keys never lose precision, and
  decimals stay exact on the wire.
* **`Date` for timestamps.** It gives millisecond precision. Temporal can replace it once
  runtimes ship it.
* **`_meta`** rather than `meta` holds a model's metadata, because `meta` is a common
  column name.
* **Strict conversions** in the addon (`bindings/node/src/convert.rs`). A `number` for a
  `BigInt` column must be a safe integer, an `Int` must fit in 32 bits, and a `Decimal`
  must be finite. Anything else is a `TypeError`, never a silent coercion.
