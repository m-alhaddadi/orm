/** `qs.explain()` and `db.fetch(sql, ...params)` on Postgres and SQLite. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, DatabaseError, getDatabase, loads, QueryError, Registry, type QueryEvent } from "../src/index.js";
import { Post, User } from "./blog/models.js";
import { useDatabase } from "./helpers.js";

useDatabase();

const source = `datasource db {
  provider = "sqlite"
}
model Person {
  id   Int    @id
  name String @unique
}
`;

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any

test("explain and fetch on SQLite", async () => {
  const registry = new Registry();
  const { Person } = loads(source, { registry }) as Record<string, Any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    await Person.objects.using(db).insertMany([0, 1, 2].map((i) => ({ id: i, name: `p${i}` })));
    assert.match(await Person.objects.using(db).filter(Person.name.eq("p1")).explain(), /SEARCH .*person/);
    await assert.rejects(Person.objects.using(db).explain({ analyze: true }), QueryError);
    const rows = await db.fetch("SELECT id, name, id * 0.5 AS half FROM person WHERE id >= ? ORDER BY id", 1);
    assert.deepEqual(rows, [{ id: 1n, name: "p1", half: 0.5 }, { id: 2n, name: "p2", half: 1 }]);
    const events: QueryEvent[] = [];
    db.onQuery((e) => events.push(e));
    assert.deepEqual(await db.fetch("SELECT count(*) AS n FROM person"), [{ n: 3n }]);
    assert.deepEqual(events.map((e) => [e.sql, e.rows]), [["SELECT count(*) AS n FROM person", 1]]);
  } finally { await db.close(); }
});

test("explain and fetch on Postgres", async () => {
  const db = getDatabase();
  const user = await User.objects.insert({ email: "f@example.com", name: "F" });
  const qs = Post.objects.filter(Post.authorId.eq(user.id)).orderBy(Post.id);
  const plan = await qs.explain();
  assert.match(plan, /Scan/);
  assert.doesNotMatch(plan, /actual time/);
  assert.match(await qs.explain({ analyze: true }), /actual time[\s\S]*Execution Time/);
  await assert.rejects(Post.objects.lock().explain({ analyze: true }), /row locks/);

  assert.deepEqual(await db.fetch("SELECT id, email FROM users WHERE id = $1 AND name = $2", user.id, "F"), [{ id: user.id, email: "f@example.com" }]);
  const [row] = await db.fetch(
    "SELECT $1::jsonb AS j, 2::int2 AS small, 1.5::float4 AS f, ARRAY[1, 2] AS a, NULL::text AS n, true AS b, $2::timestamptz AS t",
    { k: [1] }, new Date("2026-01-02T03:04:05Z"),
  );
  assert.deepEqual(row, { j: { k: [1] }, small: 2, f: 1.5, a: [1, 2], n: null, b: true, t: new Date("2026-01-02T03:04:05Z") });
  await assert.rejects(db.fetch("SELECT interval '1 day' AS i"), (e: unknown) => e instanceof DatabaseError && /interval/.test(e.message));
});
