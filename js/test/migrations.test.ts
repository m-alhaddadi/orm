/** Schema files, migration generation (no database) and running migrations against a
 * database of their own (migrations create and drop extensions, which are per database). */

import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";

import { DatabaseError, IntegrityError, MigrationError, Migrations, Migrator, Registry, SchemaError, connect, loads, type Database } from "../src/index.js";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { DATABASE_URL } from "./helpers.js";

// `npx orm` as a process of its own: src/cli.ts (Bun) or dist/src/cli.js (Node)
const CLI = [".js", ".ts"].map((ext) => fileURLToPath(new URL(`../src/cli${ext}`, import.meta.url))).find(existsSync)!;

function cli(args: string[], cwd?: string): { code: number; out: string; err: string } {
  const r = spawnSync(process.execPath, [CLI, ...args], { cwd, encoding: "utf8", env: { ...process.env, ORM_DATABASE_URL: "" } });
  return { code: r.status ?? -1, out: r.stdout, err: r.stderr };
}

// the repository root: from test/ (Bun) or dist/test/ (Node)
const ROOT = [join(import.meta.dirname, "..", ".."), join(import.meta.dirname, "..", "..", "..")].find((d) => existsSync(join(d, "examples")))!;

const V1 = `
model Author {
  id    BigInt @id @default(autoincrement())
  email String @unique @db.VarChar(254)
  books Book[]

  @@map("authors")
}

model Book {
  id        BigInt @id @default(autoincrement())
  author_id BigInt
  title     String @db.VarChar(200)
  pages     Int    @default(0)
  author    Author @relation(fields: [author_id], references: [id], onDelete: Cascade)

  @@index([author_id])
  @@map("books")
}
`;

// v1 + an extension type, a renamed column, new columns, indexes, constraints, a trigger
const V2 = String.raw`
model Author {
  id    BigInt @id @default(autoincrement())
  email String @unique @db.Citext
  books Book[]

  @@map("authors")
}

model Book {
  id         BigInt   @id @default(autoincrement())
  author_id  BigInt
  name       String   @renamed_from("title") @db.VarChar(200)
  pages      Int      @default(0) @check("pages >= 0")
  meta       Json?    @comment("free-form attributes")
  updated_at DateTime @default(now())
  author     Author   @relation(fields: [author_id], references: [id], onDelete: Cascade)

  @@index([author_id])
  @@index([author_id, updated_at(sort: Desc)], where: raw("pages > 0"))
  @@index([name(ops: raw("gin_trgm_ops"))], type: Gin)
  @@unique([author_id, name])
  @@map("books")
  @@trigger(touch, before: [update], body: "BEGIN\n    NEW.updated_at := now();\n    RETURN NEW;\nEND;")
}
`;

function models(source: string): Registry {
  const registry = new Registry();
  loads(source, { registry });
  return registry;
}

const tmp = () => mkdtempSync(join(tmpdir(), "orm-migrations-"));

// -- no database ----------------------------------------------------------------------------------

test("loads() builds models from schema text", () => {
  const reg = models(V2);
  const book = reg.get("Book");
  assert.equal(book.table, "books");
  assert.deepEqual([...book.fields.keys()], ["id", "authorId", "name", "pages", "meta", "updatedAt"]);
  assert.ok(book.field("meta").nullable);
  assert.ok(book.field("updatedAt").hasServerValue);
  assert.equal(book.relations.get("author")!.target, "Author");
  // schema objects travel in the IR untouched by TypeScript
  assert.ok((book.ir["triggers"] as { body: string }[])[0]!.body.startsWith("BEGIN\n    NEW.updated_at"));
});

test("schema errors name the line", () => {
  assert.throws(() => loads("model Book {\n  id    BigInt @id\n  title Strin\n}", { registry: new Registry() }), (e: unknown) => e instanceof SchemaError && /<schema>:3:9: Book.title: unknown type Strin/.test(e.message));
});

test("the first migration", () => {
  const migs = new Migrations(tmp(), models(V1));
  const m = migs.make()!;
  assert.equal(m.name, "0001_initial");
  const up = m.upSql;
  assert.ok(up.indexOf('CREATE TABLE "authors"') < up.indexOf('CREATE TABLE "books"'));
  assert.ok(up.includes('CONSTRAINT "books_author_id_fkey" FOREIGN KEY ("author_id") REFERENCES "authors" ("id") ON DELETE CASCADE'));
  assert.ok(m.downSql.includes('DROP TABLE "books";'));
  const snap = JSON.parse(readFileSync(join(m.path, "snapshot.json"), "utf8")) as { tables: { name: string }[] };
  assert.deepEqual(snap.tables.map((t) => t.name), ["authors", "books"]);
  assert.equal(migs.make(), null); // nothing changed
});

test("the second migration alters in place", () => {
  const dir = tmp();
  new Migrations(dir, models(V1)).make();
  const migs = new Migrations(dir, models(V2));
  const sql = migs.plan().up.map((s) => s.sql);
  assert.equal(sql[0], 'CREATE EXTENSION IF NOT EXISTS "citext"');
  assert.ok(sql.includes('ALTER TABLE "books" RENAME COLUMN "title" TO "name"'));
  assert.ok(migs.plan().up.some((s) => s.warning?.includes("changes authors.email from varchar(254) to citext")));
  const m = migs.make("catalog changes")!;
  assert.equal(m.name, "0002_catalog_changes");
  assert.equal(migs.make(), null);
  // checksums are the Python package's: sha256 of up.sql
  assert.match(m.checksum, /^[0-9a-f]{64}$/);
});

test("migrations from a schema file", () => {
  const dir = tmp();
  writeFileSync(join(dir, "schema.prisma"), V1);
  const m = new Migrations(join(dir, "migrations"), join(dir, "schema.prisma")).make();
  assert.ok(m !== null);
});

test("the generated models are current", async () => {
  const { native } = await import("../src/native.js");
  const schema = join(ROOT, "examples/blog/schema.prisma");
  // A composition artifact records its lowering passes in the embedded schema;
  // the blog schema uses no extension, so the rest must equal the default output.
  const composition = (JSON.parse(native().profileMetadata()) as { capabilities: Record<string, boolean> }).capabilities.composition;
  const comparable = (module: string): unknown[] => {
    if (!composition) return [module];
    const lines: unknown[] = module.split("\n");
    const at = lines.findIndex((line) => typeof line === "string" && line.startsWith("const SCHEMA: SchemaIR = "));
    const schema = JSON.parse((lines[at] as string).slice("const SCHEMA: SchemaIR = ".length).replace(/;$/, "")) as { behavior?: { declarations?: unknown[] } };
    assert.equal(schema.behavior?.declarations?.length ?? 0, 0);
    delete schema.behavior;
    lines[at] = schema;
    return lines;
  };
  assert.deepEqual(comparable(readFileSync(join(ROOT, "examples/blog/models.ts"), "utf8")), comparable(native().generateTypescript(schema, "orm")), "run `orm generate typescript`");
  assert.deepEqual(comparable(readFileSync(join(ROOT, "js/test/blog/models.ts"), "utf8")), comparable(native().generateTypescript(schema, "../../src/index.js")));
  const out = join(tmp(), "models.ts");
  const sets = ["--query-set", "Post=./queries.js#PostQueries", "--query-set", "Tag=./queries.js#TagQueries"];
  assert.equal(cli(["--schema", schema, "generate", "typescript", "-o", out, "--import", "../../../src/index.js", ...sets]).code, 0);
  assert.deepEqual(comparable(readFileSync(join(ROOT, "js/test/typing/queries/models.ts"), "utf8")), comparable(readFileSync(out, "utf8")), "regenerate js/test/typing/queries/models.ts");
});

test("the CLI", () => {
  const dir = tmp();
  writeFileSync(join(dir, "schema.prisma"), V1);
  const run = (...args: string[]) => cli(args, dir);
  assert.equal(run("check").code, 0);
  assert.equal(run("generate", "-o", "app/models.ts").code, 0);
  assert.ok(readFileSync(join(dir, "app/models.ts"), "utf8").includes("export interface BookSpec"));
  assert.equal(run("generate").code, 0); // TypeScript by default under npx orm
  assert.ok(existsSync(join(dir, "models.ts")));
  writeFileSync(join(dir, "package.json"), JSON.stringify({ orm: { querySets: { Author: "./queries.js#AuthorQueries" } } }));
  assert.equal(run("generate", "-o", "q/models.ts").code, 0);
  assert.ok(readFileSync(join(dir, "q/models.ts"), "utf8").includes("useQuerySet(Author, () => _q0.AuthorQueries);"));
  assert.match(run("generate", "-o", "q/models.ts", "--query-set", "Nope=./x.js#X").err, /no model Nope/);
  assert.equal(run("makemigrations", "--check").code, 1);
  const made = run("makemigrations");
  assert.equal(made.code, 0);
  assert.match(made.out, /^Created /);
  assert.equal(run("makemigrations", "--check").code, 0);
  const sql = run("sqlmigrate", "1");
  assert.equal(sql.code, 0);
  assert.ok(sql.out.includes('CREATE TABLE "books"'));
  assert.equal(run("nope").code, 2);
  assert.match(run("--help").out, /^usage: npx orm /);
  assert.equal(run("migrate").code, 2); // no database URL
  writeFileSync(join(dir, "package.json"), JSON.stringify({ orm: { schema: "db/schema.prisma", migrations: "db/migrations" } }));
  assert.match(run("check").err, /db\/schema.prisma/);
  writeFileSync(join(dir, "package.json"), "{}");
  writeFileSync(join(dir, "schema.prisma"), "model X {");
  const bad = run("check");
  assert.equal(bad.code, 1);
  assert.ok(bad.err.includes("schema.prisma:1:1: model X is not closed"));
});

// -- against a database -----------------------------------------------------------------------------

const url = new URL(DATABASE_URL);
const MIGRATIONS_DB = url.pathname.slice(1) + "_migrations";
const MIGRATIONS_URL = Object.assign(new URL(DATABASE_URL), { pathname: "/" + MIGRATIONS_DB }).toString();

before(async () => {
  const admin = await connect(DATABASE_URL, { maxConnections: 1, default: false, registry: new Registry() });
  try {
    if (!(await admin.fetchText(`SELECT 1 FROM pg_database WHERE datname = '${MIGRATIONS_DB}'`)).length) {
      await admin.execute(`CREATE DATABASE "${MIGRATIONS_DB}"`);
    }
  } finally {
    await admin.close();
  }
});

const opened: Database[] = [];
after(async () => {
  for (const db of opened) {
    await db.close();
  }
});

async function database(reg: Registry): Promise<Database> {
  const db = await connect(MIGRATIONS_URL, { maxConnections: 2, default: false, registry: reg });
  opened.push(db);
  await db.execute("DROP TABLE IF EXISTS books, authors, orm_migrations CASCADE; DROP FUNCTION IF EXISTS books_touch() CASCADE");
  return db;
}

async function scalar(db: Database, sql: string): Promise<string | null> {
  return (await db.fetchText(sql))[0]?.[0] ?? null;
}

test("upgrade and downgrade round trip", async () => {
  const dir = tmp();
  const [reg1, reg2] = [models(V1), models(V2)];
  const db = await database(reg2);
  new Migrations(dir, reg1).make();
  const migrator = new Migrator(db, new Migrations(dir, reg2));
  assert.deepEqual((await migrator.upgrade()).map((m) => m.name), ["0001_initial"]);
  await db.execute("INSERT INTO authors (email) VALUES ('Ann@Example.com'); INSERT INTO books (author_id, title) VALUES (1, 'Dune')");

  new Migrations(dir, reg2).make("v2");
  assert.deepEqual((await migrator.status()).map((s) => s.applied), [true, false]);
  await migrator.upgrade();

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const [Author, Book] = [reg2.get("Author").model, reg2.get("Book").model] as any[];
  // the renamed column kept its data; citext compares case-insensitively
  const book = await Book.objects.using(db).get(Book.name.eq("Dune"));
  assert.equal(await Author.objects.using(db).filter(Author.email.eq("ann@example.COM")).count(), 1);
  const before = book.updatedAt;
  await book.update({ pages: 10 }); // the trigger
  assert.ok(book.updatedAt > before);
  await assert.rejects(book.update({ pages: -1 }), IntegrityError);
  await assert.rejects(Author.objects.using(db).insert({ email: "ANN@example.com" }), IntegrityError);
  await assert.rejects(Book.objects.using(db).insert({ authorId: 1, name: "Dune" }), IntegrityError);

  // back to zero and up again: the down migrations are valid DDL too
  assert.deepEqual((await migrator.downgrade({ target: "zero" })).map((m) => m.name), ["0002_v2", "0001_initial"]);
  assert.equal(await scalar(db, "SELECT to_regclass('books')::text"), null);
  assert.equal((await migrator.upgrade()).length, 2);
  assert.deepEqual(await migrator.upgrade(), []);
  assert.deepEqual((await migrator.downgrade()).map((m) => m.name), ["0002_v2"]);
  assert.equal(await scalar(db, "SELECT data_type::text FROM information_schema.columns WHERE table_name = 'books' AND column_name = 'title'"), "character varying");
});

test("an edited applied migration is refused", async () => {
  const dir = tmp();
  const db = await database(models(V1));
  const migs = new Migrations(dir, models(V1));
  const m = migs.make()!;
  const migrator = new Migrator(db, migs);
  await migrator.upgrade();
  writeFileSync(join(m.path, "up.sql"), m.upSql + "\n-- edited\n");
  new Migrations(dir, models(V2)).make();
  await assert.rejects(migrator.upgrade(), (e: unknown) => e instanceof MigrationError && /changed after it was applied/.test(e.message));
});

test("a failed migration rolls back", async () => {
  const dir = tmp();
  const db = await database(models(V1));
  const migs = new Migrations(dir, models(V1));
  const m = migs.make()!;
  writeFileSync(join(m.path, "up.sql"), m.upSql + "\nSELECT no_such_function();\n");
  await assert.rejects(new Migrator(db, migs).upgrade(), DatabaseError);
  assert.equal(await scalar(db, "SELECT to_regclass('authors')::text"), null);
  assert.deepEqual((await new Migrator(db, migs).status()).map((s) => s.applied), [false]);
});

test("the CLI against a database", async () => {
  const dir = tmp();
  writeFileSync(join(dir, "schema.prisma"), V1);
  await database(models(V1));
  const run = (...args: string[]) => cli(["--schema", join(dir, "schema.prisma"), "--dir", join(dir, "migrations"), "--url", MIGRATIONS_URL, ...args]);
  assert.equal(run("makemigrations").code, 0);
  assert.deepEqual(run("migrate"), { code: 0, out: "Applied 0001_initial\n", err: "" });
  assert.match(run("showmigrations").out, /^\[x\] 0001_initial {2}\(/);
  assert.equal(run("migrate").out, "Nothing to apply.\n");
  assert.deepEqual(run("rollback", "--to", "zero"), { code: 0, out: "Reverted 0001_initial\n", err: "" });
});
