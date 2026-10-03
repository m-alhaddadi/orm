# Roadmap

Improvements we want but haven't planned yet. Phases and what's done are in
[`PLAN.md`](PLAN.md).

## SQLite

The first implementation is available in Rust, Python, Node and Bun; see
[usage and limitations](docs/schema.md#sqlite). URLs are `sqlite://:memory:` and
`sqlite:///absolute/path.db`. One serialized connection belongs to each client;
`max_connections` does not create a pool. Enums require explicit text/int storage.
Exact Decimal, arrays, native enums, PostgreSQL extensions/functions and
unsupported trigger/index features fail validation. Generated/computed columns
remain out of scope.

Future work: reduce migrations from rebuilding every managed table to rebuilding
only affected tables, and add introspection/drift detection. Current migrations
preserve rename hints and AUTOINCREMENT counters, validate foreign keys, roll back
failures and reject unmanaged indexes/triggers before dropping tables. SQLite
snapshots are version 2, PostgreSQL snapshots stay version 1, and cross-dialect
snapshots are rejected.

## Performance

### Shortcut joins through a shared key — not scheduled

A relation path `A → B → C` where B reaches C through the same key that A uses to
reach B, e.g. `Order.shop.config` with `ShopConfig.shop_id` both the key and a FK to
`Shop.id`:

```sql
-- today
LEFT JOIN shops j1 ON j1.id = orders.shop_id
LEFT JOIN shop_configs j2 ON j2.shop_id = j1.id
-- shortcut
LEFT JOIN shop_configs j2 ON j2.shop_id = orders.shop_id
```

* **Skip B** when nothing else of B is read (reading `B.<key>` resolves to
  `A.<fk>`) and either relation is a FK, so B's row is known to exist. This is the
  main win: one table fewer in joins (`select()`, `order_by`, `group_by`) and one
  `EXISTS` level fewer in filters.
* **Keep B but link C to A** when B is read: lets the planner join C before B, e.g.
  to apply a selective filter on C first. Needs the same FK condition. For filters,
  `EXISTS(B ... AND EXISTS(C ...))` can become `EXISTS(B ...) AND EXISTS(C ...)`
  without a FK, since B's key is unique.
* `select_related` of B always keeps the join.

Why not leave it to Postgres: for inner joins it derives `orders.shop_id =
shop_configs.shop_id` but still joins `shops`, since it doesn't trust FKs for join
removal; for LEFT JOINs it derives nothing. Where: `ensure_join` and `exists_via` in
`engine/src/plan.rs` (marked TODO).

## JS / TypeScript

Status: a first binding shipped ([`docs/typescript-api.md`](docs/typescript-api.md),
[`PLAN.md`](PLAN.md)). It mirrors the Python API (class instances, `selectRelated` /
`prefetchRelated`, `AsyncLocalStorage` transactions, awaitable query sets) with types
that follow the query; it already settles camelCase fields and the value mapping
(`bigint`, decimal.js, `Date`). The items below are proposals on top of it. The
Python API is Django / SQLAlchemy-shaped; ported as is (operators → `.eq()`), it would
feel foreign next to Prisma, Drizzle and Kysely. Keep the IR and its semantics shared
and give TS its own frontend. Items marked *(also Python)* are gaps in the shared
query model, not only in the TS surface.

### Query API

* **Result types follow the query.** `include: { posts: true }` returns
  `User & { posts: Post[] }`; `select: { id: true, email: true }` returns exactly
  those keys; `select()` of expressions returns an object typed per key. Prisma's
  `GetPayload`, Kysely's and Drizzle's inferred rows are the bar. This is the main
  gap: `tsc` can't check rows whose relations are filled at runtime.
* **Plain objects, no lazy relations.** Rows cross Next.js server components, tRPC,
  workers and `JSON.stringify`; class instances and `await user.posts` don't. Every
  relation load is explicit (`include` / `with`). Prisma has no lazy loading and
  Drizzle's `with:` is explicit.
* **Two layers over one IR.** An object API for Prisma users
  (`findMany({ where, include, select, orderBy, cursor, take, skip, distinct })`,
  `findUnique`, `findFirstOrThrow`, `count`, `aggregate`, `groupBy`) and a typed
  builder for Drizzle / Kysely users and for what the object API can't say (CTEs,
  windows, `EXISTS`, locks): `db.select({...}).from(User).where(User.views.gt(10))`.
  Both compile to the same IR, so the planner and its tests stay shared.
* **Explicit relation quantifiers** *(also Python)*: `posts: { some: {...} }`,
  `every`, `none`, and `is` / `isNot` for to-one. Django's rule (conditions in one
  `filter()` hold for the same related row) is subtle even for Django users. All of
  them compile to the `EXISTS` / `NOT EXISTS` we already plan; `every` is
  `NOT EXISTS (... AND NOT cond)`.
* **Nested writes** *(also Python)*: `create({ data: { ..., posts: { create: [...],
  connect: [{ id }], connectOrCreate: ... } } })`, and `update` with `set` /
  `disconnect` / `delete` / `upsert` on relations. One transaction, ordered by FK
  dependencies, multi-row `INSERT ... RETURNING` per level.
* **Cursor pagination** *(also Python)*: `cursor: { id }` + `take` (negative to page
  backwards) as a keyset `WHERE (k1, k2) > ($1, $2)` on the `orderBy` columns, not
  `OFFSET`. The batched `iterate()` already does this internally.
* **Typed raw SQL**: an `` sql`...` `` tagged template that parameterises
  interpolations, accepts columns / tables / expressions as fragments, and can be
  used inside the builder or run on its own with a declared row type
  (`` sql<{ id: number }>`...` ``). Today `qs.sql()` only prints. *(also Python: an
  `orm.sql()` fragment.)*
* **Operator names** match the TS world: `equals`, `in`, `notIn`, `lt`, `lte`, `gt`,
  `gte`, `contains`, `startsWith`, `endsWith`, `mode: 'insensitive'`; `has` /
  `hasSome` / `hasEvery` / `isEmpty` for arrays; JSON `path` filters.

### Transactions and connections

* **Explicit `tx` handle first**: `db.transaction(async (tx) => ...)`, with every
  query taking `tx` in place of `db`. A forgotten `tx` with `AsyncLocalStorage`
  silently runs outside the transaction; offer ALS as an opt-in on top.
* **`await using`** (TS 5.2 explicit resource management) for connections, locks and
  manual transactions: `await using tx = await db.begin()` rolls back on scope exit
  unless committed.
* **Isolation level, `readOnly`, `deferrable`, timeouts and retry on serialization
  failure** (`40001`) as options of `transaction()`.
* **`AbortSignal`** on every query and transaction; abort sends a Postgres cancel
  request.
* **Pooler-safe mode**: PgBouncer / Supavisor in transaction mode and serverless
  poolers break named prepared statements. A connect option switches to unnamed
  statements; prepared queries (`.prepare()`) degrade to plain ones there.
* **Dev hot reload**: Next.js / Vite re-evaluate modules and leak pools. Cache the
  client on `globalThis` in dev (document it, or do it in the generated client).

### Runtimes and packaging

* **Prebuilt addons per platform** through `optionalDependencies`
  (`@orm/native-linux-x64-gnu`, `-musl`, `darwin-arm64`, `win32-x64`, ...), no
  postinstall download, no Rust toolchain for users.
* **Node, Bun, Deno** run the napi addon. **Edge runtimes** (Cloudflare Workers,
  Vercel Edge, Deno Deploy) can't load native code: build the engine to WASM with a
  JS socket / HTTP driver (`cloudflare:sockets`, Neon / Supabase HTTP and WebSocket
  drivers). The PLAN already reserves WASM "when required"; edge support is when.
* **ESM first, CJS too**, `exports` map with types, `sideEffects: false`; generated
  code is tree-shakable (one module per model, no giant barrel).
* **Bundler notes**: Next.js `serverExternalPackages`, webpack / esbuild externals
  for the `.node` file; an error that names the fix when the addon is bundled.

### Types and values

* **Generated types stay small.** Prisma's generated types are known to slow `tsc`
  and editors on large schemas. Generate per-model types and let inference do the
  rest (Drizzle's approach); add a `tsc --extendedDiagnostics` benchmark on a
  200-model schema to CI.
* **Value mapping**, decided once and documented: `bigint` columns (`number` with an
  error past 2^53, or `bigint` per column via `@ts.type`), `Decimal` (string or a
  pluggable decimal class, never `number`), timestamps (`Date`, `Temporal` when
  available), `Json` typed per column (`Json<Shape>`), enums as string unions plus a
  `const` object, UUIDs as branded strings.
* **Branded ids** (`UserId`, `PostId`) as an option, so ids of different models
  don't mix.
* **Validation schemas**: generate Zod / Valibot / ArkType schemas for create and
  update inputs from the same schema (checks, lengths and enums included), for
  tRPC / form validation.
* **camelCase fields** mapped from snake_case columns (`@map`), as Prisma does.

### Errors, observability, testing

* **Typed errors with codes**: `UniqueViolation` (with the constraint and columns),
  `ForeignKeyViolation`, `NotFound`, `SerializationFailure`, `QueryCanceled`, all
  subclasses of one `OrmError` with the SQLSTATE; `instanceof` and `code` both work.
* **Logging and tracing hooks**: a `logger` option (SQL, params, duration, rows) and
  OpenTelemetry spans (`db.system`, `db.statement`, `db.operation`) from the engine,
  for Python too.
* **Test helpers**: a per-test transaction that rolls back (Vitest / Jest / `bun
  test` fixtures), and a factory helper typed from the generated inputs.

### Adoption from Prisma

The schema is already a `.prisma` file, so the move from Prisma is mostly runtime:

* **Baseline existing databases**: read `_prisma_migrations`, mark the current state
  as applied, and continue with our migrations.
* **A Prisma-compatible client shape** (`prisma.user.findMany(...)`) for the object
  API, so most call sites move by changing the import. Document what differs
  (no lazy loading in either, our relation filters, raw SQL).
* **Codemod** for `$queryRaw` / `$executeRaw` → `` sql`...` ``, and
  `$transaction([...])` → `db.transaction()`.
* **Benchmarks** against Prisma, Drizzle and Kysely on the same workloads as
  `bench/js` (Drizzle only today), including cold start and `tsc` time.
