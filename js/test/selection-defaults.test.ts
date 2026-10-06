import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, define, NotLoaded, Registry } from "../src/index.js";
import { native } from "../src/native.js";

const queryDefaults = (JSON.parse(native().nativeArtifact()) as { capabilities?: string[] }).capabilities?.includes("query-defaults") ?? false;

async function open(dialect: "sqlite" | "postgres", defaults: boolean) {
  const registry = new Registry();
  const field = (name: string, type = "string", flags = {}) => ({ name, column: name, type, ...flags });
  // Runtime define intentionally has no generated static model types.
  const models = define({ dialect, models: [
      { name: "SelectedAuthor", table: "selection03_js_authors", fields: [
        field("name"), field("id", "int", { primary_key: true, auto_increment: true }),
        field("visible", "bool", { default: true }), field("bio", "text"), field("note", "string", { nullable: true }),
      ], relations: [{ name: "books", kind: "many", target: "SelectedBook", from: "id", to: "author_id" }] },
      { name: "SelectedBook", table: "selection03_js_books", fields: [field("title"), field("author_id", "int"), field("id", "int", { primary_key: true, auto_increment: true })],
        relations: [{ name: "author", kind: "one", target: "SelectedAuthor", from: "author_id", to: "id", foreign_key: true, on_delete: "cascade" }] },
    ], behavior: defaults ? { schema_contract: 1, query_defaults: [
      { model: "SelectedAuthor", filter: { t: "col", path: [], name: "visible" }, fields: ["name", "note"] },
      { model: "SelectedBook", fields: ["title"], related: [["author"]] },
    ] } : {} } as never, { registry });
  const Author = models["SelectedAuthor"] as any;
  const Book = models["SelectedBook"] as any;
  const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
  const db = await connect(url, { registry, default: false });
  await db.dropTables(); await db.createTables();
  return { db, Author, Book, a: Author.objects.using(db), b: Book.objects.using(db) };
}

for (const dialect of ["sqlite", "postgres"] as const) {
  test(`partial model shapes: ${dialect}`, async () => {
    const { db, Author, Book, a, b } = await open(dialect, false);
    try {
      const author = await a.insert({ name: "ann", bio: "large", note: null });
      await a.insert({ name: "bob", bio: "large" });
      await b.insert({ title: "one", authorId: author.pk });
      const partial = await a.only(Author.name, Author.note).orderBy(Author.id).first();
      assert.deepEqual(partial.toJSON(), { name: "ann", note: null });
      assert.equal(partial.pk, author.pk);
      assert.throws(() => partial.bio, NotLoaded);
      assert.throws(() => a.only(Author.name, Author.name), TypeError);
      await partial.update({ name: "changed" }); assert.deepEqual(partial.toJSON(), { name: "changed", note: null });
      await partial.refresh(Author.bio); assert.deepEqual(partial.toJSON(), { name: "changed", note: null, bio: "large" });
      const names: string[] = [];
      for await (const o of a.only(Author.name).iterate(1)) names.push(o.name);
      assert.deepEqual(names, ["changed", "bob"]);
      assert.equal((await partial.books.using(db).get()).title, "one");
      const full = await a.only().orderBy(Author.id).first();
      assert.equal(Object.getPrototypeOf(full), Author._meta.Row.prototype); assert.equal(full.bio, "large");
      const book = await b.only(Book.title).get(); assert.throws(() => book.author, NotLoaded);
      assert.equal((await b.selectRelated(Book.author).only(Book.title).get()).author.name, "changed");
    } finally { await db.dropTables(); await db.close(); }
  });

  test(`schema query defaults: ${dialect}`, async (t) => {
    if (!queryDefaults) {
      t.skip("needs a native build with the query-defaults feature");
      return;
    }
    const { db, Author, Book, a, b } = await open(dialect, true);
    try {
      await assert.rejects(a.insert({ name: "missing" }), /bio/);
      const visible = await a.insert({ name: "visible", bio: "large", note: null });
      const hidden = await a.insert({ name: "hidden", bio: "large", visible: false });
      await b.insert({ title: "one", authorId: visible.pk });
      await b.insert({ title: "two", authorId: hidden.pk });
      const partial = await a.get();
      assert.deepEqual(partial.toJSON(), { name: "visible", note: null });
      assert.equal(partial.pk, visible.pk);
      for (const name of ["id", "bio", "visible"]) assert.throws(() => partial[name], NotLoaded);
      assert.equal(a.sql().includes('"bio"'), false);
      assert.equal(await a.count(), 1); assert.equal(await a.exists(), true);
      assert.equal(await a.withoutDefaults().count(), 2);
      assert.equal(await a.withoutDefaults().filter(Author.name.eq("visible")).count(), 1);
      assert.equal((await a.only().get()).bio, "large");
      const explicit = await a.only(Author.note, Author.name).get();
      await explicit.refresh(); assert.deepEqual(explicit.toJSON(), { note: null, name: "visible" });
      await explicit.refresh(Author.bio); assert.deepEqual(explicit.toJSON(), { name: "visible", note: null, bio: "large" });
      await partial.update({ name: "changed" }); assert.deepEqual(partial.toJSON(), { name: "changed", note: null });
      const joined = await b.orderBy(Book.id).all();
      assert.equal(joined.length, 2); assert.equal(joined[0].author.name, "changed"); assert.equal(joined[1].author, null);
      assert.deepEqual(joined[0].toJSON(), { title: "one" });
      assert.deepEqual(joined[0].author.toJSON(), { name: "changed", note: null });
      assert.throws(() => joined[0].authorId, NotLoaded);
      await joined[1].update({ authorId: visible.pk });
      assert.throws(() => joined[1].author, NotLoaded);
      await joined[1].update({ authorId: hidden.pk });
      const bypassed = await b.withoutDefaults().selectRelated(Book.author).orderBy(Book.id).all();
      assert.equal(bypassed[1].author.name, "hidden"); assert.equal(bypassed[1].author.visible, false);
      const partialReturn = await a.only(Author.name).update({ name: "again" }, { returning: true });
      assert.deepEqual(partialReturn[0].toJSON(), { name: "again" });
      const cleared = await b.withoutRelated().first(); assert.throws(() => cleared.author, NotLoaded);
      const prefetched = await a.prefetchRelated(Author.books).get(); assert.equal(prefetched.books.cached[0].title, "one");
      const unfiltered = await a.withoutDefaults().prefetchRelated(Author.books).filter(Author.id.eq(visible.pk)).get();
      assert.equal(unfiltered.books.cached[0].authorId, visible.pk);
      assert.equal((await a.inBulk()).get(visible.pk).name, "again");
      const written = await a.only().update({ visible: false }, { returning: true });
      assert.equal(written.length, 1); assert.equal(written[0].visible, false); assert.equal(await a.count(), 0);
      assert.equal(await a.withoutDefaults().updateMany([{ id: hidden.pk, name: "bulk" }]), 1);
      assert.equal(await a.updateMany([{ id: hidden.pk, name: "excluded" }]), 0);
      assert.equal(await a.delete(), 0);
      assert.equal(await a.withoutDefaults().filter(Author.id.eq(hidden.pk)).delete(), 1);
    } finally { await db.dropTables(); await db.close(); }
  });
}
