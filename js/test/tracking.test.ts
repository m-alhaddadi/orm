import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, Registry, VersionConflict, type Database, type SoftDeletable } from "../src/index.js";
import { native } from "../src/native.js";

const capabilities = JSON.parse(native().nativeArtifact()).capabilities as string[];
const OLD = new Date("2000-01-01T00:00:00Z");
type Models = Record<string, { objects: any; [key: string]: any }>;

async function open(dialect: string, source: string): Promise<[Database, Models]> {
  const registry = new Registry();
  const models = loads((dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + source, { registry }) as unknown as Models;
  const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
  const db = await connect(url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  return [db, models];
}

for (const dialect of ["sqlite", "postgres"]) {
  for (const mode of ["database", "application"]) {
    test(`updated_at is set on ORM updates and by the trigger (${dialect}, ${mode})`, { skip: !capabilities.includes("updated-at") }, async () => {
      const [db, models] = await open(dialect, `
model Note {
  id      Int      @id
  views   Int      @default(0)
  changed DateTime @default(now()) @timestamps.updated_at(mode: "${mode}")
  @@map("tracking_node_notes")
}`);
      const notes = models.Note!.objects.using(db);
      try {
        const note = await notes.insert({ id: 1, changed: OLD });
        await notes.insert({ id: 2, changed: OLD });
        await note.update({ views: 1 });
        assert.ok(note.changed > OLD);
        const [row] = await notes.filter(models.Note!.id.eq(2)).update({ views: 2 }, { returning: true });
        assert.ok(row.changed > OLD);
        await note.update({ changed: OLD });
        assert.equal(note.changed.getTime(), OLD.getTime());
        await db.execute("UPDATE tracking_node_notes SET views = 4 WHERE id = 1");
        await note.refresh();
        assert.equal(note.changed > OLD, mode === "database");
      } finally { await db.dropTables(); await db.close(); }
    });

    test(`soft delete updates instead of deleting (${dialect}, ${mode})`, { skip: !capabilities.includes("soft-delete") }, async () => {
      const [db, models] = await open(dialect, `
model Author {
  id      Int       @id
  deleted DateTime? @soft_delete.deleted_at(mode: "${mode}")
  books   Book[]
  @@map("tracking_node_authors")
}
model Book {
  id        Int       @id
  author_id Int
  author    Author    @relation(fields: [author_id], references: [id], onDelete: Cascade)
  removed   DateTime? @soft_delete.deleted_at(mode: "${mode}")
  @@map("tracking_node_books")
}`);
      const Author = models.Author!, authors = Author.objects.using(db);
      try {
        await authors.insertMany([{ id: 1 }, { id: 2 }, { id: 3 }]);
        await models.Book!.objects.using(db).insertMany([{ id: 1, authorId: 1 }, { id: 2, authorId: 2 }]);
        assert.equal(await authors.filter(Author.id.eq(1)).delete(), 1);
        assert.equal(await authors.filter(Author.id.eq(1)).delete(), 0);
        const [row] = await authors.filter(Author.id.eq(2)).delete({ returning: true });
        assert.ok(row.id === 2 && row.deleted !== null);
        assert.equal(await authors.count(), 3);
        assert.deepEqual((await authors.deletedOnly().orderBy(Author.id.asc()).all()).map((a: { id: number }) => a.id), [1, 2]);
        assert.equal(await authors.allWithDeleted().count(), 3);
        const removed = (await models.Book!.objects.using(db).deletedOnly().all()).length;
        assert.equal(removed, mode === "database" ? 2 : 0);
        assert.equal(await authors.filter(Author.id.eq(1)).undelete(), 1);
        const restored = await authors.get(Author.id.eq(1));
        assert.equal(restored.deleted, null);
        await restored.delete();
        await (restored as SoftDeletable).undelete();
        assert.equal(restored.deleted, null);
        await db.execute("DELETE FROM tracking_node_books");
        await db.execute("DELETE FROM tracking_node_authors WHERE id = 3");
        assert.equal(await authors.filter(Author.id.eq(3)).count(), mode === "database" ? 1 : 0);
        assert.equal(await authors.filter(Author.id.eq(2)).hardDelete(), 1);
        await (restored as SoftDeletable).hardDelete();
        assert.equal(await authors.filter(Author.id.eq(1)).count(), mode === "database" ? 1 : 0);
      } finally { await db.dropTables(); await db.close(); }
    });
  }

  test(`a stale instance write throws VersionConflict (${dialect})`, { skip: !capabilities.includes("optimistic-locking") }, async () => {
    const [db, models] = await open(dialect, `
model Doc {
  id      Int    @id
  title   String
  version Int    @default(0) @locking.version
  @@map("tracking_node_docs")
}`);
    const Doc = models.Doc!, docs = Doc.objects.using(db);
    try {
      const first = await docs.insert({ id: 1, title: "a" });
      const second = await docs.get(Doc.id.eq(1));
      await first.update({ title: "b" });
      assert.equal(first.version, 1);
      await assert.rejects(second.update({ title: "c" }), VersionConflict);
      await assert.rejects(second.delete(), VersionConflict);
      assert.equal(await docs.update({ title: "d" }), 1);
      assert.equal((await docs.get(Doc.id.eq(1))).version, 2);
      await second.refresh();
      await second.update({ title: "e" });
      assert.equal(second.version, 3);
      await second.delete();
      await assert.rejects(first.update({ title: "f" }), Doc.DoesNotExist);
      assert.throws(() => docs.deletedOnly(), /no @soft_delete/);
    } finally { await db.dropTables(); await db.close(); }
  });
}
