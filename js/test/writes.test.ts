/** Bulk writes: insertMany batches, partial-index upserts, getOrInsert, many-to-many
 * links with extra fields, COPY. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { DatabaseError, IntegrityError, connect, getDatabase } from "../src/index.js";
import { Comment, Post, Profile, Tag, User } from "./blog/models.js";
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

// -- upserts on a partial unique index -------------------------------------------------------

async function withAnonIndex(f: () => Promise<void>): Promise<void> {
  const db = getDatabase();
  await db.execute("CREATE UNIQUE INDEX comments_anon_body ON comments (post_id, body) WHERE author_id IS NULL");
  try {
    await f();
  } finally {
    await db.execute("DROP INDEX comments_anon_body");
  }
}

test("onConflict where picks a partial unique index", () =>
  withAnonIndex(async () => {
    const alice = await User.objects.insert({ email: "a@x.io", name: "A" });
    const post = await Post.objects.insert({ author: alice, title: "t", body: "b" });
    const first = await Comment.objects.insert({ post, body: "hi" });
    await assert.rejects(
      Comment.objects.insert({ post, body: "hi" }, { onConflict: [Comment.postId, Comment.body], doNothing: true }),
      /no unique or exclusion constraint/,
    );
    const where = Comment.authorId.isNull();
    assert.equal(await Comment.objects.insert({ post, body: "hi" }, { onConflict: [Comment.postId, Comment.body], where, doNothing: true }), null);
    const row = await Comment.objects.insert({ post, body: "hi" }, { onConflict: [Comment.postId, Comment.body], where, set: { body: "hi again" } });
    assert.equal(row.id, first.id);
    assert.equal(row.body, "hi again");
    await Comment.objects.insertMany([
      { post, author: alice, body: "hi" },
      { post, author: alice, body: "hi" },
    ]);
    assert.equal(await Comment.objects.count(), 3);
  }));

// -- getOrInsert ----------------------------------------------------------------------------------

test("getOrInsert", async () => {
  const [user, created] = await User.objects.getOrInsert({ email: "a@x.io" }, { defaults: { name: "A" } });
  assert.ok(created);
  assert.equal(user.name, "A");
  const [again, created2] = await User.objects.getOrInsert({ email: "a@x.io" }, { defaults: { name: "Other" } });
  assert.ok(!created2);
  assert.equal(again.id, user.id);
  assert.equal(again.name, "A");
  const [profile, made] = await Profile.objects.getOrInsert({ user });
  assert.ok(made);
  assert.equal(profile.userId, user.id);
  const [same, made2] = await Profile.objects.getOrInsert({ user });
  assert.ok(!made2);
  assert.equal(same.id, profile.id);
});

test("getOrInsert is safe under concurrency", async () => {
  const results = await Promise.all(
    Array.from({ length: 20 }, (_, i) => User.objects.getOrInsert({ email: "race@x.io" }, { defaults: { name: `U${i}` } })),
  );
  assert.equal(results.filter(([, created]) => created).length, 1);
  assert.equal(new Set(results.map(([u]) => u.id)).size, 1);
  assert.equal(await User.objects.count(), 1);
});

test("getOrInsert checks the lookup", async () => {
  await assert.rejects(Comment.objects.getOrInsert({ authorId: null }, { defaults: { body: "x", postId: 1n } }), /NULL never conflicts/);
  await assert.rejects(User.objects.getOrInsert({}, { defaults: { name: "A" } }), /unique constraint/);
  await assert.rejects(User.objects.getOrInsert({ name: "A" }, { defaults: { email: "a@x.io" } }), /no unique or exclusion constraint/);
});
