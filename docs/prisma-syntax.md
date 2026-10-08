# Prisma-compatible schema syntax (design)

Status: implemented in `core/src/dsl/`, which reads only `.prisma` files; the `.orm`
syntax is gone. The language reference is [`schema.md`](schema.md). This document
records why the syntax looks the way it does and how the `.orm` syntax maps onto it.

## Decisions

* **The schema is a `.prisma` file.** Models and our constraints, triggers and
  Postgres features live together in it. Prisma's VS Code extension then gives
  highlighting, formatting, completion, go-to-definition and rename for the parts it
  knows. Prisma's language server only loads files whose extension is `.prisma`
  (`@prisma/schema-files-loader`: `extname(path) !== ".prisma"` is skipped), so a
  `.orm` file mapped to the Prisma language gets highlighting but no diagnostics,
  completion or formatting.
* **When Prisma has a syntax for something, we use it** (`@id`, `@default`,
  `@relation`, `@@index(type:, where: raw())`, `@db.*`, `@map`, `@@map`, datasource
  `extensions`). Those lines show no IDE error.
* **What Prisma lacks is a normal attribute or block with our name**, e.g.
  `@check(...)`, `@@trigger(...)`, `function name { ... }`. The IDE marks these lines
  as errors. That is accepted. Comments are never used to carry schema.
* **Our features follow our design, not Prisma's.** Where Prisma has no support, the
  syntax, argument names and semantics are ours and may differ from how Prisma would
  do it. The only constraint is the formatter rules below, so format-on-save never
  breaks a file. This includes extra arguments on Prisma's own attributes (e.g.
  `type: hnsw` or `include:` on `@@index`).
* **We are independent of Prisma at runtime.** Our parser reads the file, our Rust
  migrator diffs and applies it, our code generators emit the models. Nothing calls the
  Prisma CLI, engines or generators. We use what Prisma publishes for free: the
  syntax, the editor extension, and the Apache-2.0 engine source as a reference.
* **No `generator orm { }` block.** Prisma accepts generator blocks without error, but
  `provider = "orm"` makes `prisma generate` look for an `orm` generator program and
  fail. We don't need a marker: the `orm` CLI is given the schema path (flag or
  `[tool.orm] schema` in `pyproject.toml`), and settings stay where they are today.

## Formatter rules (tested)

Tested with `@prisma/prisma-schema-wasm` 8.1.0 (the formatter and linter inside the
Prisma VS Code extension) on the two schemas below:

* Formatting never drops or rewrites a line, including our unknown attributes and
  blocks, and a second format changes nothing.
* Unknown attributes are kept and aligned with the rest.
* Inside a model, the formatter puts Prisma's own `@@` attributes (`@@unique`,
  `@@index`, `@@map`, ...) first and ours after them, each group in its own order. Our
  parser must not depend on `@@` order.
* Inside an unknown top-level block (`function x { }`), the formatter removes
  indentation. Content is kept; SQL doesn't care.

Two rules follow, and the parser enforces them so a format can't break a file:

1. **Each attribute is on one line.** The formatter treats a `@@trigger(...)` split
   across lines as two unknown lines and can move them away from the other `@@`
   attributes. A long trigger body goes in a `function` block instead.
2. **Multi-line text (`"""..."""`) only appears in top-level blocks.**

## Mapping from the `.orm` syntax

`native` means Prisma syntax with no IDE error; `ours` means an IDE error is expected.

### Top level

| `.orm` | `.prisma` | |
|---|---|---|
| (implicit Postgres) | `datasource db { provider = "postgresql" }` | native |
| `extension postgis(schema: "ext", version: "3.4")` | `extensions = [postgis(schema: "ext", version: "3.4")]` in `datasource` | native |
| `extension "uuid-ossp"` | `extensions = [uuid_ossp(map: "uuid-ossp")]` | native |
| `import "extensions/acme.toml"` | `import "extensions/acme.toml"` | ours |
| `function audit_row { returns: trigger ... }` | `function audit_row { returns = trigger ... }` (`key = value`, like `datasource`) | ours |
| `model Post @table("posts")` | `model Post { ... @@map("posts") }` | native |

### Fields

Fields are `name Type @attr...` with no colon. `Type?` is nullable, as before.

| `.orm` | `.prisma` | |
|---|---|---|
| `BigInt`, `Int`, `Float` | `BigInt`, `Int`, `Float` | native |
| `Bool` | `Boolean` | native |
| `String`, `String(n)` | `String`, `String @db.VarChar(n)` | native |
| `Text` | `String @db.Text` | native |
| `DateTime`, `Date` | `DateTime`, `DateTime @db.Date` | native |
| `Uuid` | `String @db.Uuid` | native |
| `Json` | `Json` (jsonb) | native |
| `@db_type("numeric(10, 2)")` | `Decimal @db.Decimal(10, 2)` (any `@db.*` Prisma has) | native |
| extension type `citext` | `String @db.Citext` | native |
| extension type `vector(384)`, `geography(Point, 4326)` | `Unsupported("vector(384)")` | native; the string is resolved through the extension catalog |
| `@primary @auto` | `@id @default(autoincrement())` | native; our migrator emits `GENERATED BY DEFAULT AS IDENTITY` |
| `@unique` | `@unique` | native |
| `@index` (one column) | `@@index([col])` on the model | native |
| `@default(0)`, `@default("x")`, `@default(true)` | same | native |
| `@default(now)` | `@default(now())` | native |
| `@default(sql("gen_random_uuid()"))` | `@default(dbgenerated("gen_random_uuid()"))` | native |
| `@default({"a": 1})` | `@default("{\"a\": 1}")` on a `Json` field | native |
| client-made `uuid` | `@client_default(uuid())`, `@client_default(uuid7())` | ours |
| `@column("db_name")` | `@map("db_name")` | native |
| `@check("views >= 0")` | `@check("views >= 0")` | ours |
| `@comment("...")` | `@comment("...")` | ours |
| `@renamed_from("old")` | `@renamed_from("old")` | ours |

### Relations

Prisma requires both sides of a relation to be declared, which the blog example
already does.

| `.orm` | `.prisma` | |
|---|---|---|
| `author: User @relation(via: author_id)` | `author User @relation(fields: [author_id], references: [id])` | native |
| `on_delete: cascade / set_null / set_default / restrict / no_action` | `onDelete: Cascade / SetNull / SetDefault / Restrict / NoAction` (same for `onUpdate`) | native |
| `posts: Post[] @relation(via: Post.author_id)` | `posts Post[]` | native |
| two relations between the same models | `@relation("name", ...)` on both sides | native |
| `deferrable: deferred` | `@relation(..., deferrable: deferred)` | ours |
| foreign-key name | `@relation(..., map: "fk_name")` | native |

### Model attributes

| `.orm` | `.prisma` | |
|---|---|---|
| `@@index([a, b(sort: desc)])` | `@@index([a, b(sort: Desc)])` | native |
| `where: "published"` | `where: raw("published")` | native |
| `type: gin` (gist, brin, hash) | `type: Gin` | native |
| `ops: gin_trgm_ops` | `ops: raw("gin_trgm_ops")` | native |
| `@@unique([a, b])` | `@@unique([a, b])` | native |
| `type: hnsw` / `ivfflat` / `bloom`, `nulls: last`, `include:`, `with:`, `nulls_not_distinct:`, `sql("expr")` keys, `collate:` | same arguments in `@@index` / `@@unique` | ours (Prisma flags the argument) |
| `@@check("...", name: "...")` | same | ours |
| `@@exclude([...], where: "...")` | same | ours |
| `@@trigger(name, before: [...], function: f, args: [...])` | same, on one line | ours |
| `@@comment("...")`, `@@renamed_from("old")` | same | ours |

## The blog example

[`examples/blog/schema.prisma`](../examples/blog/schema.prisma), as the formatter
leaves it. The IDE errors are the `@@check` line, the `through:` arguments of the
many-to-many relations (with Prisma's complaint that they have no opposite side), and
the `@value` / `@@storage` lines of the `Priority` enum. It doesn't list
`extensions = [pg_trgm]`: the trigram index pulls the extension in, and listing it
would add an explicit pin to the IR.

```prisma
datasource db {
  provider = "postgresql"
}

model User {
  id         BigInt    @id @default(autoincrement())
  email      String    @unique @db.VarChar(254)
  name       String    @db.VarChar(100)
  created_at DateTime  @default(now())
  posts      Post[]
  comments   Comment[]
  profile    Profile?

  @@map("users")
}

// One-to-one: the key is on Profile (unique), User.profile is the other side.
model Profile {
  id         BigInt   @id @default(autoincrement())
  user_id    BigInt   @unique
  role       Role     @default(member)
  balance    Decimal  @default(0) @db.Decimal(12, 2)
  links      String[] @default([])
  user       User     @relation(fields: [user_id], references: [id], onDelete: Cascade)

  @@map("profiles")
}

// A Postgres enum type (the default storage).
enum Role {
  member
  editor
  admin
}

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
  tags       Tag[]     @relation(through: PostTag)
  post_tags  PostTag[]

  @@index([author_id])
  @@index([author_id, created_at(sort: Desc)], where: raw("published"))
  @@index([title(ops: raw("gin_trgm_ops"))], type: Gin) // pulls in the pg_trgm extension
  @@map("posts")
  @@check("views >= 0", name: "posts_views_not_negative")
}

model Comment {
  id         BigInt   @id @default(autoincrement())
  post_id    BigInt
  author_id  BigInt?
  body       String   @db.Text
  created_at DateTime @default(now())
  post       Post     @relation(fields: [post_id], references: [id], onDelete: Cascade)
  author     User?    @relation(fields: [author_id], references: [id], onDelete: SetNull)

  @@index([post_id])
  @@index([author_id])
  @@map("comments")
}

// Many-to-many: Post.tags and Tag.posts go through the PostTag join model.
model Tag {
  id        BigInt    @id @default(autoincrement())
  name      String    @unique @db.VarChar(50)
  priority  Priority  @default(normal)
  posts     Post[]    @relation(through: PostTag)
  post_tags PostTag[]

  @@map("tags")
}

model PostTag {
  id      BigInt @id @default(autoincrement())
  post_id BigInt
  tag_id  BigInt
  position Int?  // set by post.tags.add(..., through_defaults={"position": n})
  post    Post   @relation(fields: [post_id], references: [id], onDelete: Cascade)
  tag     Tag    @relation(fields: [tag_id], references: [id], onDelete: Cascade)

  @@unique([post_id, tag_id])
  @@index([tag_id])
  @@map("post_tags")
}

// An enum stored as integers, limited to these values by a CHECK constraint.
enum Priority {
  low    @value(1)
  normal @value(2)
  high   @value(3)

  @@storage(int)
}
```

## Every feature

```prisma
import "extensions/acme.toml"

datasource db {
  provider   = "postgresql"
  extensions = [postgis(schema: "ext", version: "3.4"), uuid_ossp(map: "uuid-ossp"), citext, btree_gist]
}

function audit_row {
returns  = trigger
language = plpgsql
body     = """
BEGIN
INSERT INTO audit_log (tbl) VALUES (TG_TABLE_NAME);
RETURN NULL;
END;
"""
}

model Account {
  id         String                                 @id @default(dbgenerated("gen_random_uuid()")) @db.Uuid
  email      String                                 @unique @db.Citext
  price      Decimal                                @db.Decimal(10, 2)
  embedding  Unsupported("vector(384)")?
  location   Unsupported("geography(Point, 4326)")?
  born_on    DateTime?                              @db.Date
  meta       Json                                   @default("{\"a\": 1}")
  nick       String                                 @map("nickname") @renamed_from("handle") @comment("shown publicly")
  views      Int                                    @default(0) @check("views >= 0")
  updated_at DateTime                               @default(now())

  @@unique([email, nick], nulls_not_distinct: true, deferrable: deferred)
  @@index([email(sort: Desc)], nulls: last, include: [nick], name: "x")
  @@index([sql("lower(email)", collate: "C")], unique: true)
  @@index([embedding(ops: vector_cosine_ops)], type: hnsw, with: { m: 16, ef_construction: 64 })
  @@map("accounts")
  @@check("char_length(nick) > 0")
  @@exclude([id(op: "="), sql("tstzrange(updated_at, updated_at)", op: "&&")], where: "views > 0")
  @@trigger(touch, before: [update], update_of: [nick], when: "OLD.nick <> NEW.nick", body: "BEGIN NEW.updated_at := now(); RETURN NEW; END;")
  @@trigger(audit, after: [insert, update, delete], for_each: statement, function: audit_row, args: ["accounts"])
  @@comment("customer accounts")
  @@renamed_from("customers")
}
```

This is the formatter's output, so it is stable under format-on-save. IDE errors:
`import`, the lines of `function audit_row`, and our attributes and arguments in
`Account`. Everything else is native Prisma.

## Choices made in the parser

Where the mapping above leaves something open:

* `@@index([col])` with a single plain key is the column's own index, as `@index` was,
  so the IR doesn't change.
* `where:` takes `raw("...")` or a plain string, in `@@index` and `@@exclude` alike;
  `ops:` takes `raw("...")` or a bare name. `name:` and Prisma's `map:` both set the
  database name of an index or unique constraint.
* Extension types are written `Unsupported("...")` (or `String @db.Citext`), never as
  bare type names, so Prisma's editor reads every field type.
* `Decimal` is `numeric`; values travel in Postgres' binary `numeric` format and become
  Python `Decimal`s, so no precision is lost.
* `enum` blocks are Prisma's, and by default a Postgres enum type as in Prisma.
  `@@storage(text)` / `@@storage(int)` (ours) store them in a text or integer column
  with a `CHECK` instead; integer values are given with `@value(n)` (ours), labels with
  Prisma's `@map("...")`.
* `Type[]` on a scalar type is an array column (Prisma's scalar lists), `text[]` for
  `String[]`. Unlike Prisma, `Type[]?` (a nullable array) is allowed.
* A one-to-one back relation is `Profile?` without `fields:`, as in Prisma; the key on
  the other side must be unique.
* Many-to-many relations go through an explicit join model:
  `tags Tag[] @relation(through: PostTag)` (ours), with
  `through_fields: [post, tag]` when the join model has several relations to a side.
  Prisma's implicit many-to-many (`Tag[]` on both sides, no join model) isn't
  supported: name the join model.
* Prisma's other `@db.*` types set the column's SQL type, like `@db_type` did.
* A to-many relation finds its key through the to-one relation on the other model;
  composite keys aren't supported.
* `datasource` is optional and its `url` is ignored (the URL comes from
  `ORM_DATABASE_URL`). `generator` blocks are read and ignored, so a file can also
  drive Prisma Client.
* Comments are `//` only, as in Prisma.
* Prisma Client makes `uuid()` and `cuid()`, not the database. So an imported
  `@default(uuid())` (also `uuid(4)`) becomes `@client_default(uuid())`, and
  `@default(uuid(7))` becomes `@client_default(uuid7())`: no DDL default, and the ORM
  fills the value on insert. `cuid()`, `nanoid()` and `ulid()` fail with a hint to
  use `uuid()` or `uuid7()`.

## Migrations

The migrator stays ours, in Rust (`core/src/migrate/`), and reads only our IR, so it
handles triggers, checks, exclusions and functions like any other object. Prisma's
engine code is a reference, not a dependency:

* Live-database introspection (`orm pull`, drift detection) is ours too, in
  `engine/src/introspect.rs`: it reads `pg_catalog`, including `pg_trigger` and
  `pg_proc`. `schema-engine/sql-schema-describer/src/postgres.rs` in
  `prisma/prisma-engines` (Apache-2.0) shows the catalog queries, but the crate itself
  depends on Prisma's `psl` and `quaint`, does not read triggers, and keeps only the
  names of check constraints.
* Prisma's differ compares Prisma datamodels, so it can't diff our objects.

The migration file layout, the `orm_migrations` table and down migrations stay as
described in [`schema.md`](schema.md#migrations). A database migrated with Prisma's
CLI and one migrated with ours are not interchangeable: Prisma doesn't know about our
triggers and checks and would report drift.
