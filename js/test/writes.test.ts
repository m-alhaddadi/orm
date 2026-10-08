/** Bulk writes: insertMany batches, partial-index upserts, getOrInsert, many-to-many
 * links with extra fields, COPY. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { DatabaseError, IntegrityError, connect } from "../src/index.js";
import { Tag, User } from "./blog/models.js";
import { DATABASE_URL, useDatabase } from "./helpers.js";

useDatabase();

// -- insertMany batches -----------------------------------------------------------------------

test("insertMany splits by the parameter limit", async () => {
  // One field per row: 70 000 rows are 70 000 parameters, more than Postgres's 65 535.
  const tags = await Tag.objects.insertMany(Array.from({ length: 70_000 }, (_, i) => ({ name: `t${i}` })));
  assert.equal(tags.length, 70_000);
  assert.deepEqual(tags.slice(0, 2).map((t) => t.name), ["t0", "t1"]);
  assert.equal(tags.at(-1)!.name, "t69999");
  assert.equal(await Tag.objects.count(), 70_000);
});

test("insertMany batchSize", async () => {
  // One statement can't update a row twice; one row per statement can.
  const rows = [
    { email: "a@x.io", name: "A1" },
    { email: "a@x.io", name: "A2" },
  ];
  await assert.rejects(User.objects.insertMany(rows, { onConflict: User.email }), (e) => e instanceof DatabaseError && /second time/.test(e.message));
  const users = await User.objects.insertMany(rows, { onConflict: User.email, batchSize: 1 });
  assert.deepEqual(users.map((u) => u.name), ["A1", "A2"]);
  assert.equal((await User.objects.get(User.email.eq("a@x.io"))).name, "A2");
});

test("insertMany batches run in one transaction", async () => {
  await Tag.objects.insert({ name: "taken" });
  const rows = [...Array.from({ length: 5 }, (_, i) => ({ name: `n${i}` })), { name: "taken" }];
  await assert.rejects(Tag.objects.insertMany(rows, { batchSize: 2 }), IntegrityError);
  assert.equal(await Tag.objects.count(), 1);
});

test("insertMany batches by max_params", async () => {
  const db = await connect(DATABASE_URL, { maxConnections: 2, default: false, disable: ["max_params=5"] });
  try {
    // Two parameters per row: two rows per statement, so the duplicate email appears
    // once in each statement.
    const rows = [
      { email: "dup@x.io", name: "U0" },
      { email: "b@x.io", name: "B" },
      { email: "dup@x.io", name: "U1" },
      { email: "c@x.io", name: "C" },
    ];
    const users = await User.objects.using(db).insertMany(rows, { onConflict: User.email });
    assert.deepEqual(users.map((u) => u.name), ["U0", "B", "U1", "C"]);
  } finally {
    await db.close();
  }
});

test("insertMany batchSize is checked", async () => {
  await assert.rejects(User.objects.insertMany([], { batchSize: 0 }), TypeError);
});
