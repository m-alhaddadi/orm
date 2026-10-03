import assert from "node:assert/strict";
import { readFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { connect, func, IntegrityError, loads, Migrations, Migrator, Prefetch, QueryError, Registry, SchemaError } from "../src/index.js";
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
