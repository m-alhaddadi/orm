/** End-to-end tests against Postgres (TS -> napi -> planner -> tokio-postgres -> Postgres). */

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  DatabaseError,
  DoesNotExist,
  IntegrityError,
  LockNotAvailable,
  MultipleObjectsReturned,
  NotLoaded,
  QueryError,
  TransactionRequired,
  connect,
  excluded,
  func,
  getDatabase,
  not,
  param,
} from "../src/index.js";
import { Comment, Post, User } from "./blog/models.js";
import { DATABASE_URL, LAST_WEEK, YESTERDAY, collect, names, otherDatabase, seed, useDatabase } from "./helpers.js";

useDatabase();

test("insert returns the row with server defaults", async () => {
  const u = await User.objects.insert({ email: "x@example.com", name: "X" });
  assert.equal(typeof u.id, "bigint");
  assert.ok(u.createdAt instanceof Date);
  const p = await Post.objects.insert({ author: u, title: "t", body: "b" });
  assert.equal(p.authorId, u.id);
  assert.equal(p.views, 0);
  assert.equal(p.published, false); // DDL defaults, read back via RETURNING
});

test("await runs a query set once; all() queries afresh", async () => {
  await User.objects.insert({ email: "a@x.io", name: "A" });
  const qs = User.objects.filter(User.name.ne("Z"));
  const first = await qs;
  await User.objects.insert({ email: "b@x.io", name: "B" });
  // the same query set gives its first rows again, without a query; a copy each time
  const again = await qs;
  assert.deepEqual(again.map((u) => u.name), ["A"]);
  assert.notEqual(again, first);
  assert.equal(again[0], first[0]);
  const seen: string[] = [];
  for await (const u of qs) {
    seen.push(u.name);
  }
  assert.deepEqual(seen, ["A"]);
  // builders and all() query again; so does Model.objects, which lives with the model
  assert.equal((await qs.all()).length, 2);
  assert.equal((await qs.orderBy(User.name)).length, 2);
  assert.equal((await User.objects).length, 2);
  await User.objects.insert({ email: "c@x.io", name: "C" });
  assert.equal((await User.objects).length, 3);
  // concurrent awaits share one run
  const fresh = User.objects.filter(User.name.ne("Z"));
  const [a, b] = await Promise.all([fresh, fresh]);
  assert.equal(a.length, 3);
  assert.equal(a[0], b[0]);
  const sel = User.objects.select({ name: User.name }).orderBy(User.name);
  assert.deepEqual(await sel, await sel);
  // a failed run isn't kept: awaiting again retries
  const bad = User.objects.filter(User.id.eq(param("id")));
  await assert.rejects(Promise.resolve(bad as never), QueryError);
  await assert.rejects(Promise.resolve(bad as never), QueryError);
});

test("insertMany with mixed columns", async () => {
  const u = await User.objects.insert({ email: "x@example.com", name: "X" });
  const posts = await Post.objects.insertMany([
    { authorId: u.id, title: "a", body: "b" },
    { authorId: u.id, title: "c", body: "d", views: 7, createdAt: LAST_WEEK },
  ]);
  assert.deepEqual(posts.map((p) => [p.title, p.views]), [["a", 0], ["c", 7]]);
  assert.equal(posts[1]!.createdAt.getTime(), LAST_WEEK.getTime());
  assert.ok(posts[0]!.createdAt > LAST_WEEK);
  assert.deepEqual(await Post.objects.insertMany([]), []);
});

test("insert validation", async () => {
  await assert.rejects(User.objects.insert({ email: "x@example.com" } as never), /User.name is required/);
  await assert.rejects(User.objects.insert({ email: "x@example.com", name: "X", nope: 1 } as never), /no field "nope"/);
  await assert.rejects(Post.objects.insert({ authorId: 1n, title: Post.body, body: "b" } as never), /plain values/);
  await assert.rejects(User.objects.insert({ email: 3, name: "X" } as never), (e: unknown) => e instanceof TypeError && /expected a string/.test(e.message));
  await assert.rejects(Post.objects.insert({ authorId: 1.5, title: "t", body: "b" }), /expected an integer/);
});

test("upsert", async () => {
  const u = await User.objects.insert({ email: "a@example.com", name: "A" });
  const same = await User.objects.insert({ email: "a@example.com", name: "A2" }, { onConflict: User.email, doUpdate: true });
  assert.deepEqual([same.id, same.name, same.createdAt.getTime()], [u.id, "A2", u.createdAt.getTime()]);
  const skipped = await User.objects.insert({ email: "a@example.com", name: "A3" }, { onConflict: User.email, doNothing: true });
  assert.equal(skipped, null);
  let rows = await User.objects.insertMany(
    [{ email: "a@example.com", name: "A4" }, { email: "b@example.com", name: "B" }],
    { onConflict: [User.email], doNothing: true },
  );
  assert.deepEqual(rows.map((r) => r.email), ["b@example.com"]);
  rows = await User.objects.insertMany(
    [{ email: "a@example.com", name: "A5" }, { email: "b@example.com", name: "B2" }],
    { onConflict: User.email, doUpdate: [User.name] },
  );
  assert.deepEqual(rows.map((r) => r.name), ["A5", "B2"]);
  assert.equal(await User.objects.count(), 2);
});

test("filter across a to-many relation", async () => {
  await seed();
  // Users with a post created before yesterday: no duplicates, Carol has no posts.
  assert.deepEqual(names(await User.objects.filter(User.posts.createdAt.lt(YESTERDAY)).all()), ["Alice", "Bob"]);
});

test("same row vs independent filters", async () => {
  await seed();
  // One filter() call: the same post must be old AND unpublished.
  const same = await User.objects.filter(User.posts.createdAt.lt(YESTERDAY), User.posts.published.eq(false)).all();
  assert.deepEqual(names(same), ["Alice"]);
  // Two calls: an old post, and a popular post (possibly different ones).
  const indep = await User.objects.filter(User.posts.createdAt.lt(YESTERDAY)).filter(User.posts.views.gt(40)).all();
  assert.deepEqual(names(indep), ["Alice", "Bob"]);
  const both = await User.objects.filter(User.posts.createdAt.lt(YESTERDAY), User.posts.views.gt(40)).all();
  assert.deepEqual(names(both), ["Bob"]);
});

test("exclude and negation", async () => {
  await seed();
  // Users without any unpublished post (Carol has no posts at all).
  assert.deepEqual(names(await User.objects.exclude(User.posts.published.eq(false)).all()), ["Bob", "Carol"]);
  assert.deepEqual(names(await User.objects.filter(not(User.email.eq("bob@example.com"))).all()), ["Alice", "Carol"]);
  assert.deepEqual(names(await User.objects.filter(User.email.eq("bob@example.com").not()).all()), ["Alice", "Carol"]);
});

test("or with a local column", async () => {
  await seed();
  const users = await User.objects.filter(User.name.eq("Carol").or(User.posts.views.gte(100))).all();
  assert.deepEqual(names(users), ["Bob", "Carol"]);
});

test("nested and reverse paths", async () => {
  await seed();
  // A literal '%' is escaped.
  assert.deepEqual(names(await User.objects.filter(User.posts.comments.body.contains("100%")).all()), ["Alice"]);
  const posts = await Post.objects.filter(Post.comments.author.name.eq("Bob")).all();
  assert.deepEqual(posts.map((p) => p.title), ["new post"]);
  assert.equal((await Post.objects.filter(Post.author.email.endsWith("@example.com")).all()).length, 3);
});

test("load a to-one relation", async () => {
  await seed();
  const [first, anon] = await Comment.objects.load(Comment.post.author, Comment.author).orderBy(Comment.id).all();
  assert.equal(first!.post.title, "new post");
  assert.equal(first!.post.author.name, "Alice");
  assert.equal(first!.author?.name, "Bob");
  assert.equal(anon!.author, null); // nullable FK, LEFT JOIN with no match
});

test("an unloaded to-one relation throws", async () => {
  await seed();
  const post = (await Post.objects.first())!;
  assert.throws(() => (post as unknown as { author: unknown }).author, (e: unknown) => e instanceof NotLoaded && /load\(Post\.author\)/.test(e.message));
  const c = await Comment.objects.filter(Comment.authorId.eq(null)).get();
  assert.equal((c as unknown as { author: unknown }).author, null); // a null key needs no loading
});

test("load a to-many relation", async () => {
  await seed();
  const users = await User.objects.load(User.posts).orderBy(User.name).all();
  assert.deepEqual(users.map((u) => u.posts.cached.length), [2, 1, 0]);
  const alice = users[0]!;
  assert.deepEqual((await alice.posts.all()).map((p) => p.title), ["old draft", "new post"]);
  // the reverse relation is filled in
  assert.equal((alice.posts.cached[0] as unknown as { author: unknown }).author, alice);
});

test("related sets: queries and inserts", async () => {
  const { alice } = await seed();
  assert.throws(() => (alice.posts as unknown as { cached: unknown }).cached, NotLoaded);
  assert.equal((await alice.posts.all()).length, 2);
  assert.equal(await alice.posts.filter(Post.published).count(), 1);
  const p = await alice.posts.insert({ title: "via relation", body: "b" });
  assert.equal(p.authorId, alice.id);
  const more = await alice.posts.insertMany([{ title: "x", body: "y" }, { title: "z", body: "w" }]);
  assert.deepEqual(new Set(more.map((q) => q.authorId)), new Set([alice.id]));
  assert.equal(await alice.posts.count(), 5);
});

test("terminal methods", async () => {
  await seed();
  assert.equal(await User.objects.count(), 3);
  assert.equal(await User.objects.filter(User.posts.views.gt(1000)).exists(), false);
  assert.equal((await User.objects.first())!.name, "Alice");
  assert.equal((await User.objects.last())!.name, "Carol");
  assert.equal((await User.objects.orderBy(User.name.desc()).first())!.name, "Carol");
  assert.equal(await User.objects.filter(User.name.eq("Nobody")).first(), null);
  assert.equal((await User.objects.get(User.email.eq("bob@example.com"))).name, "Bob");
  await assert.rejects(User.objects.get(User.email.eq("nobody@example.com")), User.DoesNotExist);
  await assert.rejects(User.objects.get(User.email.eq("nobody@example.com")), DoesNotExist);
  await assert.rejects(User.objects.get(User.name.startsWith("")), MultipleObjectsReturned);
  assert.equal(await User.objects.slice(1).count(), 2);
  assert.deepEqual((await collect(User.objects.orderBy(User.id).slice(1, 3))).map((u) => u.name), ["Bob", "Carol"]);
});

test("update and delete", async () => {
  const { alice, bob, carol, a1, b1 } = await seed();
  assert.equal(await Post.objects.filter(Post.author.name.eq("Alice")).update({ views: Post.views.add(1) }), 2);
  assert.deepEqual((await alice.posts.all()).map((p) => p.views).sort((x, y) => x - y), [6, 51]);

  // Instance update writes only the given fields and refreshes from RETURNING, so a1
  // (still holding views=5 in memory) picks up the 6 from the bulk update above.
  await a1.update({ title: "renamed" });
  assert.deepEqual([a1.title, a1.views], ["renamed", 6]);
  await a1.update({ views: Post.views.mul(10) });
  assert.equal(a1.views, 60);

  await b1.update({ author: alice });
  assert.equal(b1.authorId, alice.id);
  assert.deepEqual(new Set((await alice.posts.all()).map((p) => p.title)), new Set(["renamed", "new post", "bob's old"]));

  // ON DELETE SET NULL keeps Bob's comment, anonymised.
  await bob.delete();
  assert.equal(await Comment.objects.filter(Comment.authorId.isNull()).count(), 2);
  await assert.rejects(bob.update({ name: "ghost" }), User.DoesNotExist);
  assert.equal(await Post.objects.filter(Post.views.gt(90)).delete(), 1);
  // ON DELETE CASCADE removes Alice's remaining posts and their comments.
  await alice.delete();
  assert.equal(await Post.objects.count(), 0);
  assert.equal(await Comment.objects.count(), 0);

  await carol.update({ name: "Caroline" });
  await carol.refresh();
  assert.equal(carol.name, "Caroline");
});

test("update and delete returning rows", async () => {
  const { alice } = await seed();
  const posts = await Post.objects.filter(Post.authorId.eq(alice.id)).update({ views: Post.views.add(1) }, { returning: true });
  assert.deepEqual(posts.map((p) => p.views).sort((x, y) => x - y), [6, 51]);
  assert.deepEqual(await Post.objects.filter(Post.views.gt(1000)).update({ views: 0 }, { returning: true }), []);
  assert.deepEqual(await Post.objects.update({}, { returning: true }), []); // nothing to set: no SQL
  const gone = await Post.objects.filter(Post.views.gt(90)).delete({ returning: true });
  assert.deepEqual(gone.map((p) => p.title), ["bob's old"]);
  assert.equal(await Post.objects.count(), 2);
});

test("upsert with expressions", async () => {
  const alice = await User.objects.insert({ email: "a@x.io", name: "Alice" });
  const p = await Post.objects.insert({ author: alice, title: "t", body: "b", views: 3 });
  const again = { id: p.id, author: alice, title: "t2", body: "b", views: 4 };
  const p2 = await Post.objects.insert(again, { onConflict: Post.id, set: { views: Post.views.add(excluded(Post.views)) } });
  assert.deepEqual([p2.id, p2.views, p2.title], [p.id, 7, "t"]); // only the given assignment ran
  const p3 = await Post.objects.insert(again, { onConflict: Post.id, doUpdate: [Post.title], set: { published: true } });
  assert.deepEqual([p3.views, p3.title, p3.published], [7, "t2", true]);
  const out = await Post.objects.insertMany(
    [{ ...again, views: 1 }, { author: alice, title: "new", body: "b", views: 2 }],
    { onConflict: Post.id, set: { views: excluded(Post.views).mul(100) } },
  );
  assert.deepEqual(out.map((o) => o.views).sort((x, y) => x - y), [2, 100]);
  await assert.rejects(Post.objects.update({ views: excluded(Post.views) }), QueryError);
});

test("updateMany", async () => {
  const { alice, a1, a2, b1 } = await seed();
  assert.equal(
    await Post.objects.updateMany([
      { id: a1.id, title: "A1", views: 1 },
      { id: a2.id, title: "A2", views: 2 },
      { id: b1.id, title: "B1", views: 3 },
    ]),
    3,
  );
  assert.deepEqual((await Post.objects.orderBy(Post.title).all()).map((p) => [p.title, p.views]), [["A1", 1], ["A2", 2], ["B1", 3]]);

  // The query set's filters still apply: Bob's post is left alone.
  assert.equal(await Post.objects.filter(Post.authorId.eq(alice.id)).updateMany([{ id: a1.id, views: 10 }, { id: b1.id, views: 30 }]), 1);
  assert.equal(await alice.posts.updateMany([{ id: a2.id, views: 20 }, { id: b1.id, views: 30 }]), 1);
  assert.deepEqual((await Post.objects.all()).map((p) => p.views).sort((x, y) => x - y), [3, 10, 20]);

  // Relations, NULLs, RETURNING, unknown ids.
  const out = await Comment.objects.updateMany((await Comment.objects.all()).map((c) => ({ id: c.id, author: null })), { returning: true });
  assert.equal(out.length, 3);
  assert.ok(out.every((c) => c.authorId === null));
  const moved = await Post.objects.updateMany([{ id: b1.id, author: alice }, { id: 999, author: alice }], { returning: true });
  assert.deepEqual(moved.map((p) => [p.id, p.authorId]), [[b1.id, alice.id]]);
  assert.equal(await Post.objects.updateMany([]), 0);
  await assert.rejects(Post.objects.updateMany([{ id: a1.id, views: 1 }, { id: a1.id, views: 2 }]), /appears twice/);
  await assert.rejects(Post.objects.updateMany([{ id: a1.id, views: 1 }, { id: a2.id, title: "x" }]), /same fields/);
});

test("updateMany batches are one transaction", async () => {
  const { a1, a2, b1 } = await seed();
  const rows = [{ id: a1.id, views: 7 }, { id: a2.id, views: 8 }, { id: b1.id, views: -1 }];
  await assert.rejects(Post.objects.updateMany(rows, { batchSize: 1 }), IntegrityError); // views >= 0 fails in batch 3
  assert.deepEqual((await Post.objects.all()).map((p) => p.views).sort((x, y) => x - y), [5, 50, 100]); // nothing kept
  assert.equal((await Post.objects.updateMany(rows.slice(0, 2), { batchSize: 1, returning: true })).length, 2);
});

test("fallback SQL runs on Postgres", async () => {
  const { a1, b1 } = await seed();
  const db = await connect(DATABASE_URL, { default: false, maxConnections: 2, disable: ["update_from_values", "ilike", "returning"] });
  try {
    assert.equal(await Post.objects.using(db).updateMany([{ id: a1.id, title: "x", views: 1 }, { id: b1.id, title: "y", views: 2 }]), 2);
    const got = await Post.objects.filter(Post.id.in([a1.id, b1.id])).orderBy(Post.id).all();
    assert.deepEqual(got.map((p) => [p.title, p.views]), [["x", 1], ["y", 2]]);
    assert.deepEqual(names(await User.objects.using(db).filter(User.name.icontains("ALI")).all()), ["Alice"]);
    await assert.rejects(Post.objects.using(db).update({ views: 0 }, { returning: true }), /does not support update/);
  } finally {
    await db.close();
  }
});

test("select rows", async () => {
  const { alice, bob, a1, a2, b1 } = await seed();
  const rows = await Post.objects
    .select({ authorId: Post.authorId, posts: func.count(), views: func.sum(Post.views) })
    .groupBy(Post.authorId)
    .orderBy(Post.authorId)
    .all();
  assert.deepEqual(rows, [{ authorId: alice.id, posts: 2n, views: 55n }, { authorId: bob.id, posts: 1n, views: 100n }]);

  const many = await Post.objects.select({ authorId: Post.authorId, n: func.count() }).groupBy(Post.authorId).having(func.count().gt(1)).all();
  assert.deepEqual(many, [{ authorId: alice.id, n: 2n }]);

  assert.equal(await Post.objects.select({ top: func.max(Post.views) }).scalar(), 100);
  assert.equal(await Post.objects.filter(Post.views.gt(1000)).select({ top: func.max(Post.views) }).scalar(), null);
  assert.equal(await Post.objects.filter(Post.views.gt(1000)).select({ id: Post.id }).scalar(), null);
  assert.equal(await Post.objects.select({ n: func.count() }).scalar(), 3n);
  assert.ok(Math.abs((await Post.objects.select({ avg: func.avg(Post.views) }).scalar())! - 155 / 3) < 1e-9);
  assert.deepEqual((await Post.objects.select({ title: Post.title }).scalars()).sort(), ["bob's old", "new post", "old draft"]);
  assert.deepEqual(await Post.objects.select({ t: func.lower(Post.title) }).orderBy(Post.id).first(), { t: "old draft" });
  const one = await Post.objects.filter(Post.id.eq(b1.id)).select({ title: Post.title, author: Post.author.name }).one();
  assert.deepEqual(one, { title: "bob's old", author: "Bob" });
  await assert.rejects(Post.objects.filter(Post.id.eq(-1)).select({ id: Post.id }).one(), DoesNotExist);
  await assert.rejects(Post.objects.select({ id: Post.id }).one(), MultipleObjectsReturned);
  await assert.rejects((Post.objects.select({ id: Post.id, t: Post.title }) as never as { scalars(): Promise<unknown> }).scalars(), QueryError);
  assert.deepEqual((await Post.objects.select({ a: Post.authorId }).distinct().scalars()).sort(), [alice.id, bob.id].sort());
  const top = await Post.objects.select({ authorId: Post.authorId, title: Post.title }).distinct(Post.authorId).orderBy(Post.authorId, Post.views.desc()).all();
  assert.deepEqual(top, [{ authorId: alice.id, title: "new post" }, { authorId: bob.id, title: "bob's old" }]);
  assert.deepEqual(await collect(Post.objects.select({ id: Post.id }).orderBy(Post.id)), [{ id: a1.id }, { id: a2.id }, { id: b1.id }]);
});

test("select the model with aggregates over relations", async () => {
  const { alice } = await seed();
  const rows = await User.objects.select({ user: User, posts: func.count(User.posts), views: func.sum(User.posts.views) }).orderBy(User.id).all();
  assert.deepEqual(rows.map((r) => [r.user.name, r.posts, r.views]), [["Alice", 2n, 55n], ["Bob", 1n, 100n], ["Carol", 0n, null]]);
  assert.equal(rows[0]!.user.id, alice.id);
  // Two counts over different relations don't multiply each other (no JOIN fan-out).
  const counts = await User.objects.select({ name: User.name, posts: func.count(User.posts), c: func.count(User.comments) }).orderBy(User.id).all();
  assert.deepEqual(counts, [{ name: "Alice", posts: 2n, c: 1n }, { name: "Bob", posts: 1n, c: 1n }, { name: "Carol", posts: 0n, c: 0n }]);
  assert.equal(await User.objects.filter(User.id.eq(alice.id)).select({ n: func.count(User.posts.comments) }).scalar(), 2n);
  assert.deepEqual(names(await User.objects.filter(func.count(User.posts).gte(1)).all()), ["Alice", "Bob"]);
  assert.deepEqual(names(await User.objects.filter(func.coalesce(func.sum(User.posts.views), 0).gt(60)).all()), ["Bob"]);
});

test("subqueries with in()", async () => {
  await seed();
  const authors = User.objects.filter(User.name.in(["Alice", "Carol"])).select({ id: User.id });
  assert.deepEqual(new Set((await Post.objects.filter(Post.authorId.in(authors)).all()).map((p) => p.title)), new Set(["old draft", "new post"]));
  assert.deepEqual((await Post.objects.filter(Post.authorId.notIn(authors)).all()).map((p) => p.title), ["bob's old"]);
  const popular = Post.objects.filter(Post.views.gte(50)).select({ a: Post.authorId });
  assert.deepEqual(names(await User.objects.filter(User.id.in(popular)).all()), ["Alice", "Bob"]);
  assert.equal(await Post.objects.filter(Post.id.in([])).count(), 0);
  assert.equal(await Post.objects.filter(Post.id.notIn([])).count(), 3);
});

test("batches", async () => {
  const alice = await User.objects.insert({ email: "a@x.io", name: "A" });
  const posts = await Post.objects.insertMany(Array.from({ length: 25 }, (_, i) => ({ author: alice, title: `p${i}`, body: "b" })));
  assert.deepEqual((await collect(Post.objects.batches(10))).map((b) => b.length), [10, 10, 5]);
  const seen = (await collect(Post.objects.filter(Post.views.eq(0)).iterate(7))).map((p) => p.id);
  assert.deepEqual(seen, posts.map((p) => p.id).sort((a, b) => (a < b ? -1 : 1)));
  assert.deepEqual(await collect(Post.objects.filter(Post.id.lt(0)).batches(10)), []);
  const [batch] = await collect(Post.objects.load(Post.author).batches(25));
  assert.equal(batch![0]!.author.id, alice.id);
  await assert.rejects(collect(Post.objects.orderBy(Post.title).batches(10)), QueryError);
  await assert.rejects(collect(Post.objects.slice(0, 5).batches(10)), QueryError);
  await getDatabase().transaction(async () => {
    const it = Post.objects.lock({ skipLocked: true }).batches(4);
    assert.equal((await it.next()).value!.length, 4);
    await it.return();
  });
});

test("row locks", async () => {
  const { alice, a1 } = await seed();
  const db = getDatabase();
  const other = await otherDatabase();
  try {
    await assert.rejects(User.objects.lock().get(User.id.eq(alice.id)), TransactionRequired);
    await assert.rejects(User.objects.lock().count(), QueryError);
    await db.transaction(async () => {
      const locked = await User.objects.lock().get(User.id.eq(alice.id));
      assert.equal(locked.id, alice.id);
      const elsewhere = <T>(fn: () => Promise<T>) => other.transaction(fn);
      await assert.rejects(elsewhere(() => User.objects.using(other).lock({ nowait: true }).get(User.id.eq(alice.id))), LockNotAvailable);
      // exclusive blocks shared too
      await assert.rejects(elsewhere(() => User.objects.using(other).lock({ exclusive: false, nowait: true }).get(User.id.eq(alice.id))), LockNotAvailable);
      const others = await elsewhere(() => User.objects.using(other).orderBy(User.id).lock({ skipLocked: true }).all());
      assert.deepEqual(names(others), ["Bob", "Carol"]);
      // plain reads aren't blocked
      assert.equal((await User.objects.using(other).get(User.id.eq(alice.id))).name, "Alice");
    });
    await db.transaction(async () => {
      await Post.objects.load(Post.author).lock({ exclusive: false }).filter(Post.id.eq(a1.id)).all();
      // shared locks coexist; the joined author row isn't locked at all
      assert.equal((await other.transaction(() => Post.objects.using(other).lock({ exclusive: false, nowait: true }).filter(Post.id.eq(a1.id)).all())).length, 1);
      await other.transaction(() => User.objects.using(other).lock({ nowait: true }).get(User.id.eq(alice.id)));
      await assert.rejects(other.transaction(() => Post.objects.using(other).lock({ nowait: true }).filter(Post.id.eq(a1.id)).all()), LockNotAvailable);
    });
  } finally {
    await other.close();
  }
});

test("refresh with a row lock", async () => {
  const { alice, bob } = await seed();
  const db = getDatabase();
  const other = await otherDatabase();
  try {
    await assert.rejects(alice.refresh({ nowait: true } as never), /lock: true/);
    await assert.rejects(alice.refresh({ lock: false, exclusive: false } as never), /lock: true/);
    await assert.rejects(alice.refresh({ lock: true }), TransactionRequired);
    await assert.rejects(alice.refresh({ lock: true, nowait: true, skipLocked: true } as never), TypeError);
    assert.equal(await alice.refresh(), true);
    await db.transaction(async () => {
      assert.equal(await alice.refresh(User.name, { lock: true }), true);
      // a partial refresh locks the whole row
      await assert.rejects(other.transaction(() => User.objects.using(other).lock({ nowait: true }).get(User.id.eq(alice.id))), LockNotAvailable);
    });
    await db.transaction(async () => {
      assert.equal(await bob.refresh({ lock: true, exclusive: false }), true);
      await other.transaction(() => User.objects.using(other).lock({ exclusive: false, nowait: true }).get(User.id.eq(bob.id)));
    });
    const copy = await User.objects.using(other).get(User.id.eq(alice.id));
    await db.transaction(async () => {
      await User.objects.lock().get(User.id.eq(alice.id));
      await User.objects.filter(User.id.eq(alice.id)).update({ name: "Al" });
      await assert.rejects(other.transaction(() => copy.refresh({ lock: true, nowait: true })), LockNotAvailable);
      assert.equal(await other.transaction(() => copy.refresh({ lock: true, skipLocked: true })), false);
      assert.equal(copy.name, "Alice"); // unchanged
    });
    assert.equal(await other.transaction(() => copy.refresh({ lock: true, skipLocked: true })), true);
    assert.equal(copy.name, "Al");
    await User.objects.filter(User.id.eq(bob.id)).delete();
    await db.transaction(async () => {
      assert.equal(await bob.refresh({ lock: true, skipLocked: true }), false);
      await assert.rejects(bob.refresh({ lock: true }), User.DoesNotExist);
    });
    await assert.rejects(bob.refresh(), User.DoesNotExist);
  } finally {
    await other.close();
  }
});

test("relation equals instance", async () => {
  const { alice, bob, carol, a1, a2, b1 } = await seed();
  const qs = Post.objects.filter(Post.author.eq(alice)).orderBy(Post.id);
  assert.equal(qs.sql(), Post.objects.filter(Post.authorId.eq(alice.id)).orderBy(Post.id).sql());
  assert.ok(!qs.sql().includes(" JOIN "));
  assert.deepEqual((await qs.all()).map((p) => p.id), [a1.id, a2.id]);
  assert.deepEqual((await Post.objects.filter(Post.author.ne(alice)).all()).map((p) => p.id), [b1.id]);
  assert.equal(await Post.objects.filter(Post.author.eq(carol)).count(), 0);
  assert.match(Post.objects.filter(Post.author.eq(null)).sql(), /IS NULL/);
  assert.match(Post.objects.filter(Post.author.ne(null)).sql(), /IS NOT NULL/);
  const nested = Comment.objects.filter(Comment.post.author.eq(bob)).orderBy(Comment.id);
  assert.ok(!nested.sql().includes('"users"')); // reaches posts, not users
  const expected = await Comment.objects.filter(Comment.postId.eq(b1.id)).orderBy(Comment.id).all();
  assert.deepEqual((await nested.all()).map((c) => c.id), expected.map((c) => c.id));
  assert.throws(() => (User.posts as unknown as { eq(v: unknown): unknown }).eq(a1), /not a belongsTo relation/);
  assert.throws(() => Post.author.eq(a1 as never), /compares with a User instance/);
  assert.throws(() => Post.author.eq({ id: 1n } as never), /compares with a User instance/);
});

test("advisory locks", async () => {
  const db = getDatabase();
  const other = await otherDatabase();
  try {
    await assert.rejects(db.lock("import"), TransactionRequired);
    await db.transaction(async () => {
      assert.equal(await db.lock("import"), true);
      assert.equal(await other.transaction(() => other.lock("import", { nowait: true })), false);
      assert.equal(await other.transaction(() => other.lock("other", { nowait: true })), true);
      assert.equal(await other.transaction(() => other.lock(42, { exclusive: false, nowait: true })), true);
    });
    assert.equal(await other.transaction(() => other.lock("import", { nowait: true })), true); // released at commit
  } finally {
    await other.close();
  }
});

test("string lock keys hash like the Python package's", async () => {
  // int.from_bytes(blake2b(key.encode(), digest_size=8).digest(), "big", signed=True)
  const { blake2b } = await import("../src/blake2b.js");
  const key = (s: string) => BigInt.asIntN(64, BigInt("0x" + Buffer.from(blake2b(new TextEncoder().encode(s), 8)).toString("hex")));
  assert.equal(key("import:42"), -4107035088217233148n);
  assert.equal(key(""), -1970711489451281740n);
  assert.equal(key("a".repeat(300)), -8414559580627654238n);
});

test("integrity errors", async () => {
  const a = await User.objects.insert({ email: "dup@example.com", name: "A" });
  await assert.rejects(User.objects.insert({ email: "dup@example.com", name: "B" }), (e: unknown) => {
    assert.ok(e instanceof IntegrityError);
    assert.equal(e.sqlstate, "23505");
    assert.equal(e.constraint, "users_email_key");
    assert.equal(e.detail, "Key (email)=(dup@example.com) already exists.");
    assert.ok(e.message.startsWith("ERROR (23505)"));
    return true;
  });
  await assert.rejects(Post.objects.insert({ authorId: a.id + 1000n, title: "t", body: "b" }), (e: unknown) => {
    assert.ok(e instanceof IntegrityError);
    assert.deepEqual([e.sqlstate, e.constraint], ["23503", "posts_author_id_fkey"]);
    return true;
  });
  await assert.rejects(getDatabase().execute("SELECT 1/0"), (e: unknown) => {
    assert.ok(e instanceof DatabaseError && !(e instanceof IntegrityError));
    assert.deepEqual([e.sqlstate, e.constraint, e.detail], ["22012", null, null]);
    return true;
  });
  assert.equal(new DatabaseError("from user code").sqlstate, null);
});

test("transactions", async () => {
  const db = getDatabase();
  await db.transaction(() => User.objects.insert({ email: "in-tx@example.com", name: "T" }));
  assert.equal(await User.objects.count(), 1);

  await assert.rejects(
    db.transaction(async () => {
      await User.objects.insert({ email: "rolled-back@example.com", name: "R" });
      assert.equal(await User.objects.count(), 2); // visible inside the transaction
      throw new RangeError("boom");
    }),
    RangeError,
  );
  assert.equal(await User.objects.count(), 1);

  const out = await db.transaction(async () => {
    await User.objects.insert({ email: "outer@example.com", name: "O" });
    await assert.rejects(db.transaction(() => User.objects.insert({ email: "outer@example.com", name: "dup" })), IntegrityError); // savepoint
    await User.objects.insert({ email: "after@example.com", name: "A" });
    return "done";
  });
  assert.equal(out, "done");
  assert.equal(await User.objects.count(), 3);
});

test("concurrent queries", async () => {
  await seed();
  const counts = await Promise.all(Array.from({ length: 20 }, (_, i) => User.objects.filter(User.posts.views.gt(i)).count()));
  assert.equal(counts[0], 2);
  assert.equal(counts[19], 2);
});
