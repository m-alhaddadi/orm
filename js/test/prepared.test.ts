/** Prepared queries (`qs.prepare()` with `param()`) and LIMIT / OFFSET as IR parameters. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { QueryError, TransactionRequired, getDatabase, param } from "../src/index.js";
import { Comment, Post, Profile, User } from "./blog/models.js";
import { useDatabase } from "./helpers.js";

useDatabase();

async function seed() {
  const [alice, bob] = (await User.objects.insertMany([
    { email: "alice@example.com", name: "Alice" },
    { email: "bob@example.com", name: "Bob" },
  ]).returning()) as [User, User];
  const posts = (await Post.objects.insertMany([
    { author: alice, title: "a1", body: "", views: 5 },
    { author: alice, title: "a_2%", body: "", views: 50 },
    { author: alice, title: "a3", body: "", views: 20 },
    { author: bob, title: "b1", body: "", views: 100 },
  ]).returning()) as [Post, Post, Post, Post];
  return { alice, bob, posts };
}

// -- LIMIT / OFFSET as parameters -----------------------------------------------------------------

test("LIMIT and OFFSET are parameters", () => {
  const ir = (q: typeof Post.objects) => {
    const params: unknown[] = [];
    return [JSON.stringify(q.selectIr("select", params)), params] as const;
  };
  const [a, pa] = ir(Post.objects.orderBy(Post.id).slice(10, 20));
  const [b, pb] = ir(Post.objects.orderBy(Post.id).slice(30, 35));
  assert.equal(a, b); // one IR document (and one SQL text) for every page
  assert.deepEqual(pa, [10, 10]);
  assert.deepEqual(pb, [5, 30]);
  assert.ok(Post.objects.slice(5, 15).sql().endsWith("LIMIT 10 OFFSET 5"));
});

// -- prepared queries -------------------------------------------------------------------------------

test("a prepared query's SQL matches the plain query's", () => {
  const plain = Post.objects.filter(Post.authorId.eq(3), Post.title.contains("x_%")).orderBy(Post.id).slice(5, 15);
  const prepared = Post.objects
    .filter(Post.authorId.eq(param("a")), Post.title.contains(param("t")))
    .orderBy(Post.id)
    .limit(param("n"))
    .offset(param("o"))
    .prepare();
  assert.deepEqual([...prepared.params].sort(), ["a", "n", "o", "t"]);
  assert.equal(prepared.sql({ a: 3, t: "x_%", n: 10, o: 5 }), plain.sql());
});

test("prepared reads", async () => {
  const { alice, bob, posts } = await seed();
  const byAuthor = Post.objects.filter(Post.authorId.eq(param("author"))).orderBy(Post.id).prepare();
  assert.deepEqual((await byAuthor.all({ author: alice.id })).map((p) => p.title), ["a1", "a_2%", "a3"]);
  assert.deepEqual((await byAuthor.all({ author: bob.id })).map((p) => p.title), ["b1"]);
  assert.equal(await byAuthor.count({ author: alice.id }), 3);
  assert.equal(await byAuthor.exists({ author: bob.id }), true);
  assert.equal(await byAuthor.exists({ author: -1 }), false);
  assert.equal((await byAuthor.first({ author: alice.id }))!.title, "a1");
  assert.equal(await byAuthor.first({ author: -1 }), null);

  const byId = Post.objects.load(Post.author).filter(Post.id.eq(param("id"))).prepare();
  const p = await byId.get({ id: posts[3].id });
  assert.equal(p.title, "b1");
  assert.equal(p.author.name, "Bob");
  await assert.rejects(byId.get({ id: -1 }), Post.DoesNotExist);
  await assert.rejects(byAuthor.get({ author: alice.id }), Post.MultipleObjectsReturned);
});

test("prepared LIMIT / OFFSET and expressions", async () => {
  await seed();
  const page = Post.objects.filter(Post.views.add(param("bump")).gt(param("min"))).orderBy(Post.id).limit(param("n")).offset(param("skip")).prepare();
  assert.deepEqual((await page.all({ bump: 0, min: 0, n: 2, skip: 0 })).map((p) => p.title), ["a1", "a_2%"]);
  assert.deepEqual((await page.all({ bump: 0, min: 0, n: 2, skip: 2 })).map((p) => p.title), ["a3", "b1"]);
  assert.deepEqual((await page.all({ bump: 10, min: 55, n: 10, skip: 0 })).map((p) => p.title), ["a_2%", "b1"]);
  assert.equal(await page.count({ bump: 0, min: 0, n: 3, skip: 2 }), 2);
  assert.equal((await page.first({ bump: 0, min: 10, n: 5, skip: 1 }))!.title, "a3");
  await assert.rejects(page.all({ bump: 0, min: 0, n: -1, skip: 0 }), (e: unknown) => e instanceof QueryError && /non-negative integer/.test(e.message));
});

test("prepared LIKE patterns are escaped per call", async () => {
  await seed();
  const q = Post.objects.filter(Post.title.contains(param("t"))).orderBy(Post.id).prepare();
  assert.deepEqual((await q.all({ t: "_2%" })).map((p) => p.title), ["a_2%"]);
  assert.deepEqual((await q.all({ t: "1" })).map((p) => p.title), ["a1", "b1"]);
  const starts = Post.objects.filter(Post.title.startsWith(param("t"))).prepare();
  assert.equal(await starts.count({ t: "a" }), 3);
  assert.equal(await starts.count({ t: "a_" }), 1);
});

test("prepared has() and prefetch", async () => {
  const { alice, bob } = await seed();
  await Profile.objects.insertMany([{ user: alice, links: ["x", "y"] }, { user: bob, links: ["y"] }]);
  const withLink = Profile.objects.filter(Profile.links.has(param("link"))).prepare();
  assert.equal(await withLink.count({ link: "x" }), 1);
  assert.equal(await withLink.count({ link: "y" }), 2);

  const top = User.objects
    .orderBy(User.id)
    .load(User.posts.objects.filter(Post.views.gte(param("min"))).orderBy(Post.views.desc()).limit(param("k")))
    .prepare();
  let users = await top.all({ min: 10, k: 1 });
  assert.deepEqual(users.map((u) => u.posts.cached.map((p) => p.title)), [["a_2%"], ["b1"]]);
  users = await top.all({ min: 0, k: 2 });
  assert.deepEqual(users.map((u) => u.posts.cached.map((p) => p.title)), [["a_2%", "a3"], ["b1"]]);
});

test("prepared queries run in the current transaction", async () => {
  await seed();
  const q = Post.objects.filter(Post.views.gt(param("v"))).lock().prepare();
  await assert.rejects(q.all({ v: 0 }), TransactionRequired);
  await getDatabase().transaction(async () => {
    assert.equal((await q.all({ v: 10 })).length, 3);
  });
});

test("prepared value errors", () => {
  const q = Comment.objects.filter(Comment.authorId.eq(param("a"))).prepare();
  assert.throws(() => (q.sql as (v?: object) => string)(), /missing values for a/);
  assert.throws(() => q.sql({ a: 1, b: 2 } as never), /no param b/);
  assert.throws(() => q.sql({ a: null } as never), /can't be null/);
  assert.throws(() => q.sql({ a: "x" } as never), /expected a bigint or an integer number/);
});

test("param() misuse", () => {
  assert.throws(() => (Post.objects.filter(Post.id.eq(param("id"))) as never as { sql(): string }).sql(), (e: unknown) => e instanceof QueryError && /prepare/.test(e.message));
  assert.throws(() => Post.id.in(param("ids") as never), /in\(\)/);
  assert.throws(() => Post.objects.limit(param("n")).slice(0, 5), /sliced/);
  assert.throws(() => param("not a name"), /identifier/);
});
