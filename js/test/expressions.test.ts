/** JSON paths, containment and merge, and full-text search, on schemas loaded at runtime. */

import assert from "node:assert/strict";
import { after, before, test } from "node:test";

import { QueryError, Registry, connect, func, loads, type Database } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const SCHEMA = `
model Doc {
  id    BigInt @id @default(autoincrement())
  title String
  meta  Json
  @@map("expr_docs_js")
}
`;

const registry = new Registry();
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const Doc = loads(SCHEMA, { registry })["Doc"] as any;
let db: Database;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const objects = () => Doc.objects.using(db) as any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const titles = async (q: any) => (await q.orderBy(Doc.id).all()).map((d: { title: string }) => d.title);

before(async () => {
  db = await connect(DATABASE_URL, { default: false, registry, maxConnections: 2 });
  await db.dropTables();
  await db.createTables();
  await objects().insertMany([
    { title: "a", meta: { kind: "post", n: 5, author: { name: "Ann" }, tags: ["x", "y"] } },
    { title: "b", meta: { kind: "page", n: 1, author: { name: "Bob" }, tags: ["y"] } },
    { title: "c", meta: { kind: "post", n: 9 } },
  ]);
});

after(async () => {
  await db.dropTables();
  await db.close();
});

test("JSON paths compare as JSON, asText() as text", async () => {
  assert.deepEqual(await titles(objects().filter(Doc.meta.get("author", "name").eq("Ann"))), ["a"]);
  assert.deepEqual(await titles(objects().filter(Doc.meta.get("n").gt(3))), ["a", "c"]);
  assert.deepEqual(await titles(objects().filter(Doc.meta.get("tags", 0).asText().eq("y"))), ["b"]);
  assert.deepEqual(await titles(objects().filter(Doc.meta.get("author", "name").asText().startsWith("B"))), ["b"]);
  const rows = await objects().orderBy(Doc.id).select({ author: Doc.meta.get("author"), kind: Doc.meta.get("kind").asText() }).all();
  assert.deepEqual(rows, [{ author: { name: "Ann" }, kind: "post" }, { author: { name: "Bob" }, kind: "page" }, { author: null, kind: "post" }]);
});

test("JSON containment and keys", async () => {
  assert.equal(await objects().filter(Doc.meta.jsonContains({ kind: "post" })).count(), 2);
  assert.equal(await objects().filter(Doc.meta.get("tags").jsonContains(["y"])).count(), 2);
  assert.deepEqual(await titles(objects().filter(Doc.meta.jsonContainedBy({ kind: "post", n: 9, x: 1 }))), ["c"]);
  assert.equal(await objects().filter(Doc.meta.hasKey("author")).count(), 2);
  assert.deepEqual(await titles(objects().filter(Doc.meta.hasKey("tags").not())), ["c"]);
});

test("JSON merge in an update", async () => {
  await objects().filter(Doc.title.eq("c")).update({ meta: Doc.meta.jsonMerge({ n: 10, seen: true }) });
  const doc = await objects().get(Doc.title.eq("c"));
  assert.deepEqual(doc.meta, { kind: "post", n: 10, seen: true });
});

test("JSON operators on SQLite are an error", async () => {
  const lite = new Registry();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const Note = loads(`datasource db { provider = "sqlite" }\n${SCHEMA}`, { registry: lite })["Doc"] as any;
  const sqlite = await connect("sqlite://:memory:", { registry: lite, default: false });
  try {
    await sqlite.createTables();
    for (const cond of [Note.meta.get("a").eq(1), Note.meta.jsonContains({ a: 1 }), Note.meta.hasKey("a")]) {
      await assert.rejects(Note.objects.using(sqlite).filter(cond).count(), (e: unknown) => e instanceof QueryError && /sqlite does not support/.test(String(e)));
    }
  } finally {
    await sqlite.close();
  }
});

test("full-text search", async () => {
  const fts = new Registry();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const Article = loads(`model Article {
  id   BigInt @id @default(autoincrement())
  body String
  @@index([sql("to_tsvector('english', body)")], type: Gin, name: "articles_js_body_search")
  @@map("expr_articles_js")
}`, { registry: fts })["Article"] as any;
  const pg = await connect(DATABASE_URL, { default: false, registry: fts, maxConnections: 1 });
  try {
    await pg.dropTables();
    await pg.createTables();
    const articles = Article.objects.using(pg);
    await articles.insertMany([{ body: "The quick brown fox jumps" }, { body: "A lazy dog sleeps" }, { body: "Dogs are running; the fox and the dog" }]);
    const vector = func.toTsvector("english", Article.body);
    assert.deepEqual(await articles.filter(vector.matches("running dogs")).select({ id: Article.id }).scalars(), [3n]);
    const query = func.websearchToTsquery("english", "fox -lazy");
    const found = await articles.filter(vector.matches(query)).orderBy(Article.id).select({ id: Article.id }).scalars();
    assert.deepEqual(found, [1n, 3n]);
    const rank = await articles.filter(Article.id.eq(3)).select({ r: func.tsRank(vector, func.toTsquery("english", "dog")) }).scalar();
    assert.ok(typeof rank === "number" && rank > 0);
  } finally {
    await pg.dropTables();
    await pg.close();
  }
});
