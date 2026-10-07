import assert from "node:assert/strict";
import { readFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { connect, func, IntegrityError, loads, Migrations, Migrator, outer, Prefetch, QueryError, Registry, SchemaError } from "../src/index.js";
import { Author, Book, Status, sqliteRegistry } from "./sqlite/models.js";

test("SQLite CRUD, relations, enums, JSON, bulk defaults and CASE updates", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const a = await Author.objects.using(db).insert({ email: "a", name: "Alice" });
    assert.equal(typeof a.id, "bigint");
    assert.equal(a.active, true);
    assert.ok(a.createdAt instanceof Date);
    const books = await Book.objects.using(db).insertMany([
      { authorId: a.id, title: "one" },
      { authorId: a.id, title: "two", pages: 7, metadata: { labels: [1, true] } },
    ]);
    assert.deepEqual(books.map((b) => b.pages), [0, 7]);
    assert.equal(books[0]!.status, Status.draft);
    assert.deepEqual(books[1]!.metadata, { labels: [1, true] });
    const joined = await Book.objects.using(db).selectRelated(Book.author).orderBy(Book.id).all();
    assert.equal(joined[0]!.author.name, "Alice");
    const prefetched = await Author.objects.using(db).prefetchRelated(Author.books).all();
    assert.equal(prefetched[0]!.books.cached.length, 2);
    assert.equal(await Author.objects.using(db).filter(Author.name.icontains("ALI")).count(), 1);
    assert.equal(await Author.objects.using(db).filter(Author.name.contains("ali")).count(), 0);
    assert.equal(await Author.objects.using(db).filter(Author.books.pages.gt(0)).count(), 1);
    assert.equal(await Book.objects.using(db).select({ total: func.sum(Book.pages) }).scalar(), 7n);
    const ranked = await Book.objects.using(db).select({
      title: Book.title,
      rank: func.rowNumber().over({ partitionBy: Book.authorId, orderBy: Book.pages.desc() }),
    }).orderBy(Book.id).all();
    assert.deepEqual(ranked.map((b) => b.rank), [2n, 1n]);
    const totals = Book.objects.using(db).select({ authorId: Book.authorId, pages: func.sum(Book.pages) }).groupBy(Book.authorId).cte("totals");
    assert.equal(await Author.objects.using(db).join(totals, totals.c.authorId.eq(Author.id)).select({ pages: totals.c.pages }).scalar(), 7n);
    const sliced = await Author.objects.using(db).prefetchRelated(new Prefetch(Author.books, Book.objects.orderBy(Book.pages.desc()).slice(1, 2))).all();
    assert.deepEqual(sliced[0]!.books.cached.map((b) => b.title), ["one"]);
    const updated = await Book.objects.using(db).updateMany(books.map((b) => ({ id: b.id, pages: b.pages + 2 })), { returning: true });
    assert.deepEqual(updated.map((b) => b.pages), [2, 9]);
    const same = await Author.objects.using(db).insert({ email: "a", name: "Updated" }, { onConflict: Author.email, doUpdate: true });
    assert.equal(same!.id, a.id);
    await assert.rejects(Book.objects.using(db).insert({ authorId: 999n, title: "orphan" }), IntegrityError);
    await db.dropTables();
  } finally { await db.close(); }
});

test("SQLite nested transactions, rollback, and unsupported locks", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    await db.transaction(async () => {
      await Author.objects.using(db).insert({ email: "kept", name: "Kept" });
      await assert.rejects(db.transaction(async () => {
        await Author.objects.using(db).insert({ email: "rolled", name: "Rolled" });
        throw new Error("rollback inner");
      }), /rollback inner/);
      await assert.rejects(Author.objects.using(db).lock().all(), QueryError);
      await assert.rejects(db.lock("example"), /sqlite does not support advisory locks/);
    });
    assert.equal(await Author.objects.using(db).count(), 1);
    assert.deepEqual(await Promise.all(Array.from({ length: 20 }, () => Author.objects.using(db).count())), Array(20).fill(1));
  } finally { await db.close(); }
});

test("SQLite migrations, target checks and file persistence", async () => {
  const root = [join(import.meta.dirname, "..", ".."), join(import.meta.dirname, "..", "..", "..")].find((d) => {
    try { readFileSync(join(d, "examples/sqlite/schema.prisma")); return true; } catch { return false; }
  })!;
  const source = readFileSync(join(root, "examples/sqlite/schema.prisma"), "utf8");
  const reg = new Registry();
  loads(source, { registry: reg });
  const dir = mkdtempSync(join(tmpdir(), "orm-sqlite-"));
  const migrations = new Migrations(join(dir, "migrations"), reg);
  migrations.make("initial");
  const url = `sqlite://${join(dir, "database.db")}`;
  const db = await connect(url, { registry: reg, default: false });
  try {
    const runner = new Migrator(db, migrations);
    await runner.upgrade();
    await db.execute("INSERT INTO author (email, name) VALUES ('persisted', 'Persisted')");
    assert.equal((await runner.status())[0]!.applied, true);
    await db.close();
    const reopened = await connect(url, { registry: reg, default: false });
    try {
      assert.equal((await reopened.fetchText("SELECT name FROM author"))[0]![0], "Persisted");
      await new Migrator(reopened, migrations).downgrade({ target: "zero" });
    } finally { await reopened.close(); }
    await assert.rejects(connect("sqlite://:memory:", { registry: new Registry(), default: false }), SchemaError);
    assert.throws(() => loads(source.replace("provider = \"sqlite\"", "provider = \"postgresql\""), { registry: reg }), /same database/);
  } finally {
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
});

test("SQLite rebuilds after a rename ignore the kept rename hints", async () => {
  const root = [join(import.meta.dirname, "..", ".."), join(import.meta.dirname, "..", "..", "..")].find((d) => {
    try { readFileSync(join(d, "examples/sqlite/schema.prisma")); return true; } catch { return false; }
  })!;
  const source = readFileSync(join(root, "examples/sqlite/schema.prisma"), "utf8");
  const renamed = source.replace("  books      Book[]", "  books      Book[]\n  @@map(\"writer\")\n  @@renamed_from(\"author\")")
    .replace("  title     String", "  name      String @renamed_from(\"title\")").replace("@@index([title]", "@@index([name]");
  const regs = [source, renamed, renamed.replace("  pages     Int", "  added     Int @default(42)\n  pages     Int")].map((text) => {
    const reg = new Registry();
    loads(text, { registry: reg });
    return reg;
  });
  const dir = mkdtempSync(join(tmpdir(), "orm-sqlite-"));
  ["initial", "rename", "later"].forEach((name, i) => new Migrations(dir, regs[i]!).make(name));
  const db = await connect("sqlite://:memory:", { registry: regs[0]!, default: false });
  try {
    const runner = new Migrator(db, new Migrations(dir, regs[0]!));
    await runner.upgrade("1");
    await db.execute("INSERT INTO author (id, email, name) VALUES (100, 'gone', 'gone'); DELETE FROM author; INSERT INTO author (email, name) VALUES ('kept', 'kept')");
    await db.execute("INSERT INTO book (author_id, title) VALUES (101, 'kept')");
    await runner.upgrade("2");
    await runner.upgrade();
    assert.deepEqual(await db.fetchText("SELECT w.email, b.name, b.added FROM writer w JOIN book b ON b.author_id = w.id"), [["kept", "kept", "42"]]);
    assert.deepEqual(await db.fetchText("PRAGMA foreign_key_check"), []);
    await db.execute("INSERT INTO writer (email, name) VALUES ('next', 'next')");
    assert.equal((await db.fetchText("SELECT max(id) FROM writer"))[0]![0], "102");
    await runner.downgrade();
    assert.deepEqual(await db.fetchText("SELECT w.email, b.name FROM writer w JOIN book b ON b.author_id = w.id"), [["kept", "kept"]]);
  } finally {
    await db.close();
    rmSync(dir, { recursive: true, force: true });
  }
});

test("SQLite outer() through a relation path", async () => {
  const db = await connect("sqlite://:memory:", { registry: sqliteRegistry, default: false });
  try {
    await db.createTables();
    const alice = await Author.objects.using(db).insert({ email: "a@example.com", name: "Alice" });
    const bob = await Author.objects.using(db).insert({ email: "b@example.com", name: "Bob" });
    await Book.objects.using(db).insertMany([{ authorId: alice.id, title: "one" }, { authorId: bob.id, title: "two" }]);
    const name = Author.objects.filter(Author.email.eq(outer(Book.author.email))).select({ n: Author.name }).asScalar();
    assert.deepEqual(await Book.objects.using(db).orderBy(Book.id).select({ title: Book.title, name }).all(), [{ title: "one", name: "Alice" }, { title: "two", name: "Bob" }]);
  } finally { await db.close(); }
});

test("SQLite string functions and concatenation", async () => {
  const registry = new Registry();
  const Note = loads(`
    datasource db { provider = "sqlite" }
    model Note {
      id BigInt @id @default(autoincrement())
      title String
      tag String?
    }`, { registry })["Note"] as any;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await Note.objects.using(db).insertMany([{ title: "  a-b  ", tag: "x" }, { title: "c", tag: null }]);
    const rows = await Note.objects.using(db).orderBy(Note.id).select({
      c: func.concat(Note.title, Note.tag), p: Note.title.concat(Note.tag), t: func.trim(Note.title), l: func.ltrim(Note.title),
      r: func.rtrim(Note.title), x: func.replace(Note.title, "-", "+"), s: func.substr(func.trim(Note.title), 2, 1), i: func.strpos(Note.title, "b"),
    }).all();
    assert.deepEqual(rows, [
      { c: "  a-b  x", p: "  a-b  x", t: "a-b", l: "a-b  ", r: "  a-b", x: "  a+b  ", s: "-", i: 5 },
      { c: "c", p: null, t: "c", l: "c", r: "c", x: "c", s: "", i: 0 },
    ]);
    assert.equal(await Note.objects.using(db).filter(Note.title.concat("!").eq("c!")).count(), 1);
  } finally { await db.close(); }
});

test("SQLite UUID, date, timestamp and inline trigger conversions", async () => {
  const registry = new Registry();
  const Event = loads(`
    datasource db { provider = "sqlite" }
    model Event {
      id String @id @db.Uuid
      day DateTime @db.Date
      at DateTime
      name String
      @@trigger(uppercase, after: [insert], body: "UPDATE event SET name = upper(NEW.name) WHERE id = NEW.id")
    }
  `, { registry })["Event"]!;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const id = "c689c080-a6d0-4d3e-9305-7fe896158ac9";
    const day = new Date("2026-03-04T00:00:00Z");
    const at = new Date("2026-03-04T12:34:56.123+03:30");
    await Event.objects.using(db).insert({ id, day, at, name: "hello" });
    const loaded = await Event.objects.using(db).get() as { id: string; day: Date; at: Date; name: string };
    assert.equal(loaded.id, id);
    assert.equal((loaded.day as Date).toISOString(), day.toISOString());
    assert.equal((loaded.at as Date).toISOString(), at.toISOString());
    assert.equal(loaded.name, "HELLO");
  } finally { await db.close(); }
});
