/** Bulk writes: insertMany batches, partial-index upserts, getOrInsert, many-to-many
 * links with extra fields, COPY. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { DatabaseError, Decimal, IntegrityError, QueryError, connect, excluded, getDatabase, type QueryEvent } from "../src/index.js";
import { Comment, Post, PostTag, Priority, Profile, Tag, User, type TagInsert } from "./blog/models.js";
import { DATABASE_URL, replicaUrl, useDatabase } from "./helpers.js";

useDatabase();

// -- insertMany batches -----------------------------------------------------------------------

test("insertMany splits by the parameter limit", async () => {
  // One field per row: 70 000 rows are 70 000 parameters, more than Postgres's 65 535.
  const tags = await Tag.objects.insertMany(Array.from({ length: 70_000 }, (_, i) => ({ name: `t${i}` }))).returning();
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
  await assert.rejects(User.objects.insertMany(rows).onConflict(User.email, { update: true }), (e) => e instanceof DatabaseError && /second time/.test(e.message));
  const users = await User.objects.insertMany(rows, { batchSize: 1 }).onConflict(User.email, { update: true }).returning();
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
    const users = await User.objects.using(db).insertMany(rows).onConflict(User.email, { update: true }).returning();
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
      Comment.objects.insert({ post, body: "hi" }).onConflict([Comment.postId, Comment.body], { update: false }),
      /no unique or exclusion constraint/,
    );
    const where = Comment.authorId.isNull();
    assert.equal(await Comment.objects.insert({ post, body: "hi" }).onConflict([Comment.postId, Comment.body], { where, update: false }).returning(), null);
    const row = await Comment.objects
      .insert({ post, body: "hi" })
      .onConflict([Comment.postId, Comment.body], { where, update: true, updateValues: { body: "hi again" } })
      .returning();
    assert.equal(row.id, first.id);
    assert.equal(row.body, "hi again");
    await Comment.objects.insertMany([
      { post, author: alice, body: "hi" },
      { post, author: alice, body: "hi" },
    ]);
    assert.equal(await Comment.objects.count(), 3);
  }));

test("onConflict where with a boolean column", async () => {
  // The predicate is SQL text: a parameter would stop matching the index from the sixth
  // run of the prepared statement, when Postgres plans it generically.
  await getDatabase().execute("CREATE UNIQUE INDEX posts_published_title ON posts (author_id, title) WHERE published");
  const db = await connect(DATABASE_URL, { maxConnections: 1, default: false });
  try {
    const posts = Post.objects.using(db);
    const alice = await User.objects.using(db).insert({ email: "a@x.io", name: "A" });
    for (let i = 0; i < 8; i++) {
      await posts
        .insert({ author: alice, title: "t", body: `b${i}`, published: true })
        .onConflict([Post.authorId, Post.title], { where: Post.published.eq(true), update: true, updateFields: [Post.body] });
    }
    assert.deepEqual((await posts).map((p) => p.body), ["b7"]);
  } finally {
    await db.close();
    await getDatabase().execute("DROP INDEX posts_published_title");
  }
});

// -- getOrInsert ----------------------------------------------------------------------------------

// -- insert results and onConflict options ---------------------------------------------------

test("insertMany gives a count; returning gives the rows", async () => {
  assert.equal(await Tag.objects.insertMany([{ name: "a" }, { name: "b" }]), 2);
  assert.equal(await Tag.objects.insertMany([]), 0);
  assert.deepEqual(await Tag.objects.insertMany([]).returning(), []);
  const rows = await Tag.objects.insertMany([{ name: "c" }, { name: "d" }]).returning();
  assert.deepEqual(rows.map((t) => t.name), ["c", "d"]);
  // Each batch adds its count.
  assert.equal(await Tag.objects.insertMany(Array.from({ length: 5 }, (_, i) => ({ name: `e${i}` })), { batchSize: 2 }), 5);
  // Skipped conflicts are not counted and not returned; updated rows are.
  assert.equal(await Tag.objects.insertMany([{ name: "a" }, { name: "f" }, { name: "g" }]).onConflict(Tag.name, { update: false }), 2);
  const tags: TagInsert[] = [{ name: "a", priority: Priority.high }, { name: "h" }, { name: "b", priority: Priority.high }];
  const kept = await Tag.objects.insertMany(tags).onConflict(Tag.name, { update: false }).returning();
  assert.deepEqual(kept.map((t) => t.name), ["h"]);
  const out = await Tag.objects.insertMany(tags).onConflict(Tag.name, { update: true }).returning();
  assert.deepEqual(out.map((t) => [t.name, t.priority]), [["a", Priority.high], ["h", Priority.normal], ["b", Priority.high]]);
  assert.equal(await Tag.objects.insertMany(tags).onConflict(Tag.name, { update: true }), 3);
});

test("a single upsert gives a count", async () => {
  const alice = await User.objects.insert({ email: "a@x.io", name: "A" });
  assert.equal(await User.objects.insert({ email: "a@x.io", name: "B" }).onConflict(User.email, { update: false }), 0);
  assert.equal(await User.objects.insert({ email: "b@x.io", name: "B" }).onConflict(User.email, { update: false }), 1);
  assert.equal(await User.objects.insert({ email: "a@x.io", name: "A2" }).onConflict(User.email, { update: true }), 1);
  assert.equal((await User.objects.get(User.id.eq(alice.id))).name, "A2");
  // A statement runs once, however often it is awaited.
  const once = User.objects.insert({ email: "c@x.io", name: "C" });
  assert.equal((await once).id, (await once).id);
});

test("onConflict updateFields and updateValues", async () => {
  const alice = await User.objects.insert({ email: "a@x.io", name: "A" });
  const p = await Post.objects.insert({ author: alice, title: "t", body: "b", views: 3 });
  const again = { id: p.id, author: alice, title: "t2", body: "b2", views: 4 };
  // Both options: the union; the other fields keep their values.
  let out = await Post.objects
    .insert(again)
    .onConflict(Post.id, { update: true, updateFields: [Post.title], updateValues: { views: Post.views.add(excluded(Post.views)) } })
    .returning();
  assert.deepEqual([out.title, out.views, out.body], ["t2", 7, "b"]);
  // update: true alone overwrites every given field except the conflict columns.
  out = await Post.objects.insert(again).onConflict(Post.id, { update: true }).returning();
  assert.deepEqual([out.title, out.views, out.body], ["t2", 4, "b2"]);
});

test("onConflict rejects bad options", async () => {
  const ins = () => User.objects.insert({ email: "a@x.io", name: "A" });
  await assert.rejects(ins().onConflict(User.email, {} as never), /update: true/);
  await assert.rejects(ins().onConflict(User.email, { update: false, updateFields: [User.name] } as never), /update: false/);
  await assert.rejects(ins().onConflict(User.email, { update: false, updateValues: { name: "x" } } as never), /update: false/);
  await assert.rejects(ins().onConflict(User.email, { update: true, updateFields: [] }), /updates nothing/);
  await assert.rejects(ins().onConflict(User.email, { update: true, updateValues: {} }), /updates nothing/);
  await assert.rejects(ins().onConflict(User.email, { update: true, updateFields: [User.name], updateValues: { name: "x" } }), /name in both/);
  const many = User.objects.insertMany([{ email: "a@x.io", name: "A" }]).onConflict(User.email, { update: false });
  assert.throws(() => many.onConflict(User.email, { update: true }), /already given/);
  // Composite targets: one unique constraint over two columns.
  const alice = await User.objects.insert({ email: "a@x.io", name: "A" });
  const post = await Post.objects.insert({ author: alice, title: "t", body: "b" });
  const tag = await Tag.objects.insert({ name: "t" });
  await PostTag.objects.insert({ post, tag });
  assert.equal(await PostTag.objects.insert({ post, tag }).onConflict([PostTag.postId, PostTag.tagId], { update: false }), 0);
});

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

test("getOrInsert reads the primary", async () => {
  // The replica lags: it has none of the primary's rows.
  const routed = await connect(DATABASE_URL, { replicas: [await replicaUrl("orm_s3_replica_js")], default: false, maxConnections: 2 });
  try {
    const users = User.objects.using(routed);
    const [user, created] = await users.getOrInsert({ email: "a@x.io" }, { defaults: { name: "A" } });
    const [again, createdAgain] = await users.getOrInsert({ email: "a@x.io" });
    assert.ok(created && !createdAgain);
    assert.equal(again.id, user.id);
  } finally {
    await routed.close();
  }
});

test("getOrInsert names a row the filters hide", async () => {
  await User.objects.insert({ email: "a@x.io", name: "A" });
  await assert.rejects(User.objects.filter(User.name.eq("B")).getOrInsert({ email: "a@x.io" }, { defaults: { name: "B" } }), (e) => e instanceof QueryError && /filters hide it/.test(e.message));
  // The limit and offset do not apply to the read.
  const [user, created] = await User.objects.offset(1).limit(1).getOrInsert({ email: "a@x.io" });
  assert.ok(!created);
  assert.equal(user.name, "A");
});

test("getOrInsert on a related set", async () => {
  const alice = await User.objects.insert({ email: "a@x.io", name: "A" });
  const post = await Post.objects.insert({ author: alice, title: "t", body: "b" });
  const tag = await Tag.objects.insert({ name: "a" });
  const [link, created] = await post.postTags.getOrInsert({ tag });
  const [again, createdAgain] = await post.postTags.getOrInsert({ tag });
  assert.ok(created && !createdAgain);
  assert.equal(link.postId, post.id);
  assert.equal(again.id, link.id);
});

// -- many-to-many add() with throughDefaults ------------------------------------------------

test("add with throughDefaults", async () => {
  const alice = await User.objects.insert({ email: "a@x.io", name: "A" });
  const post = await Post.objects.insert({ author: alice, title: "t", body: "b" });
  const [t1, t2, t3] = (await Tag.objects.insertMany([{ name: "a" }, { name: "b" }, { name: "c" }]).returning()) as [Tag, Tag, Tag];
  await post.tags.add(t1, t2, { throughDefaults: { position: 1 } });
  // An existing link keeps its values.
  await post.tags.add(t2, t3, { throughDefaults: { position: 2 } });
  const links = async () =>
    (await PostTag.objects.filter(PostTag.postId.eq(post.id)).orderBy(PostTag.tagId)).map((l) => [l.tagId, l.position]);
  assert.deepEqual(await links(), [
    [t1.id, 1],
    [t2.id, 1],
    [t3.id, 2],
  ]);
  const t4 = await Tag.objects.insert({ name: "d" });
  await post.tags.set([t1, t3, t4], { throughDefaults: { position: 3 } });
  assert.deepEqual(await links(), [
    [t1.id, 1],
    [t3.id, 2],
    [t4.id, 3],
  ]);
  await post.tags.add(t2);
  assert.equal((await PostTag.objects.get(PostTag.tagId.eq(t2.id))).position, null);
});

test("throughDefaults cannot set the link keys", async () => {
  const alice = await User.objects.insert({ email: "a@x.io", name: "A" });
  const post = await Post.objects.insert({ author: alice, title: "t", body: "b" });
  const tag = await Tag.objects.insert({ name: "a" });
  for (const key of ["tagId", "postId", "post"]) {
    await assert.rejects(post.tags.add(tag, { throughDefaults: { [key]: 1 } }), /link's key/);
  }
  assert.equal(await PostTag.objects.count(), 0);
  // An unknown field or option raises also when every link exists.
  await post.tags.add(tag);
  await assert.rejects(post.tags.add(tag, { throughDefaults: { nope: 1 } }), /no field nope/);
  await assert.rejects(post.tags.add(tag, { throughDefault: {} } as never), /throughDefaults only/);
});

test("set keeps the links when it fails", async () => {
  const alice = await User.objects.insert({ email: "a@x.io", name: "A" });
  const post = await Post.objects.insert({ author: alice, title: "t", body: "b" });
  const [t1, t2] = (await Tag.objects.insertMany([{ name: "a" }, { name: "b" }]).returning()) as [Tag, Tag];
  await post.tags.add(t1);
  // Checked before the delete, and a failed insert rolls the delete back.
  for (const bad of [{ tagId: 1 }, { nope: 1 }, { position: "x" }]) {
    await assert.rejects(post.tags.set([t2], { throughDefaults: bad }));
  }
  assert.deepEqual((await PostTag.objects.all()).map((l) => l.tagId), [t1.id]);
});

// -- COPY -----------------------------------------------------------------------------------------

test("insertMany copy loads many rows", async () => {
  const alice = await User.objects.insert({ email: "a@x.io", name: "A" });
  const rows = Array.from({ length: 100_000 }, (_, i) => ({ author: alice, title: `t${i}`, body: "b", views: i }));
  assert.equal(await Post.objects.insertMany(rows, { copy: true }), 100_000);
  assert.equal(await Post.objects.count(), 100_000);
  const last = await Post.objects.get(Post.title.eq("t99999"));
  assert.equal(last.views, 99_999);
  assert.equal(last.published, false);
});

test("insertMany copy column types", async () => {
  const alice = await User.objects.insert({ email: "a@x.io", name: "A" });
  assert.equal(await Profile.objects.insertMany([{ user: alice, role: "admin", balance: new Decimal("12.50"), links: ["x", "y"] }], { copy: true }), 1);
  const p = await Profile.objects.get(Profile.userId.eq(alice.id));
  assert.equal(p.role, "admin");
  assert.equal(p.balance.toString(), "12.5");
  assert.deepEqual(p.links, ["x", "y"]);
});

test("insertMany copy stops at a duplicate key", async () => {
  await Tag.objects.insert({ name: "taken" });
  const rows = [...Array.from({ length: 1000 }, (_, i) => ({ name: `n${i}` })), { name: "taken" }];
  await assert.rejects(Tag.objects.insertMany(rows, { copy: true }), IntegrityError);
  assert.equal(await Tag.objects.count(), 1);
});

test("insertMany copy in a transaction", async () => {
  await assert.rejects(
    getDatabase().transaction(async () => {
      assert.equal(await Tag.objects.insertMany([{ name: "a" }, { name: "b" }], { copy: true }), 2);
      assert.equal(await Tag.objects.count(), 2);
      throw new RangeError("roll back");
    }),
    RangeError,
  );
  assert.equal(await Tag.objects.count(), 0);
});

test("insertMany copy in a tenant block and seen by query hooks", async () => {
  const db = getDatabase();
  const events: QueryEvent[] = [];
  const off = db.onQuery((e) => events.push(e));
  try {
    await db.tenant(7, async () => assert.equal(await Tag.objects.insertMany([{ name: "a" }, { name: "b" }], { copy: true, batchSize: undefined } as never), 2));
  } finally {
    off();
  }
  assert.deepEqual(events.map((e) => [e.sql.split(" ", 1)[0], e.rows]), [["COPY", 2]]);
});

test("insertMany copy rejections", async () => {
  await assert.rejects(Tag.objects.insertMany([{ name: "a" }], { copy: true, batchSize: 2 } as never), /batchSize/);
  await assert.rejects(Tag.objects.insertMany([{ name: "a" }, { name: "b", priority: 1 }], { copy: true }), /some rows only/);
  assert.equal(await Tag.objects.insertMany([], { copy: true }), 0);
});
