/** End-to-end tests: inBulk, exists() / scalar subqueries with outer(), window
 * functions, CTEs, nested and filtered prefetches. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { NotLoaded, Prefetch, QueryError, connect, exists, func, getDatabase, outer, window } from "../src/index.js";
import { DB } from "../src/model.js";
import { Comment, Post, User } from "./blog/models.js";
import { DATABASE_URL, useDatabase } from "./helpers.js";

useDatabase();

const NOW = Date.now();
const daysAgo = (n: number) => new Date(NOW - n * 86_400_000);

async function seed() {
  const [alice, bob, carol] = (await User.objects.insertMany([
    { email: "alice@example.com", name: "Alice" },
    { email: "bob@example.com", name: "Bob" },
    { email: "carol@example.com", name: "Carol" },
  ])) as [User, User, User];
  const posts = (await Post.objects.insertMany([
    { author: alice, title: "a1", body: "", views: 5, createdAt: daysAgo(3) },
    { author: alice, title: "a2", body: "", views: 50, createdAt: daysAgo(2) },
    { author: alice, title: "a3", body: "", views: 20, createdAt: daysAgo(1) },
    { author: bob, title: "b1", body: "", views: 100, createdAt: daysAgo(5) },
  ])) as [Post, Post, Post, Post];
  const [, a2, , b1] = posts;
  await Comment.objects.insertMany([
    { post: a2, author: bob, body: "c1" },
    { post: a2, author: null, body: "c2" },
    { post: b1, author: alice, body: "c3" },
  ]);
  return { alice, bob, carol, posts };
}

const byTitle = <T extends { title: string }>(xs: T[]) => [...xs].sort((a, b) => a.title.localeCompare(b.title));

// -- inBulk -------------------------------------------------------------------------------------

test("inBulk", async () => {
  const { alice, bob, carol } = await seed();
  const got = await User.objects.inBulk([alice.id, carol.id, 999, alice.id]);
  assert.deepEqual([...got.keys()].sort(), [alice.id, carol.id]);
  assert.equal(got.get(alice.id)!.name, "Alice");
  assert.equal((await User.objects.inBulk([])).size, 0);
  const byEmail = await User.objects.inBulk(["bob@example.com"], { field: User.email });
  assert.equal(byEmail.get("bob@example.com")!.id, bob.id);
  assert.deepEqual([...(await User.objects.filter(User.name.ne("Bob")).inBulk()).keys()].sort(), [alice.id, carol.id]);
  await assert.rejects(User.objects.inBulk(["Bob"], { field: User.name }), /unique/);
  await assert.rejects(User.objects.slice(0, 1).inBulk([1n]), /sliced/);
});

// -- exists() and scalar subqueries ---------------------------------------------------------------

test("exists() expressions", async () => {
  await seed();
  const popular = exists(Post.objects.filter(Post.authorId.eq(outer(User.id)), Post.views.gte(50)));
  assert.deepEqual((await User.objects.filter(popular).orderBy(User.id).all()).map((u) => u.name), ["Alice", "Bob"]);
  assert.deepEqual((await User.objects.filter(popular.not()).all()).map((u) => u.name), ["Carol"]);
  // In select(): a boolean per row.
  const rows = await User.objects.select({ name: User.name, popular }).orderBy(User.name).all();
  assert.deepEqual(rows, [{ name: "Alice", popular: true }, { name: "Bob", popular: true }, { name: "Carol", popular: false }]);
  // Uncorrelated.
  assert.equal((await User.objects.filter(exists(Post.objects.filter(Post.views.gt(1000)))).all()).length, 0);
  // A select() with groupBy / having.
  const many = exists(
    Post.objects.filter(Post.authorId.eq(outer(User.id))).select({ a: Post.authorId }).groupBy(Post.authorId).having(func.count().gt(2)),
  );
  assert.deepEqual((await User.objects.filter(many).all()).map((u) => u.name), ["Alice"]);
});

test("scalar subqueries", async () => {
  const { alice } = await seed();
  const latest = Post.objects.filter(Post.authorId.eq(outer(User.id))).orderBy(Post.createdAt.desc()).select({ title: Post.title }).limit(1).asScalar();
  const rows = await User.objects.select({ user: User, latest }).orderBy(User.id).all();
  assert.deepEqual(rows.map((r) => [r.user.name, r.latest]), [["Alice", "a3"], ["Bob", "b1"], ["Carol", null]]);
  assert.equal(rows[0]!.user.id, alice.id);
  // In filters and ordering.
  const best = Post.objects.filter(Post.authorId.eq(outer(User.id))).select({ m: func.max(Post.views) }).asScalar();
  assert.deepEqual((await User.objects.filter(best.gt(60)).all()).map((u) => u.name), ["Bob"]);
  assert.deepEqual((await User.objects.filter(best.isNotNull()).orderBy(best.desc()).all()).map((u) => u.name), ["Bob", "Alice"]);
  // Correlated to the same model: posts above their author's average.
  const avg = Post.objects.filter(Post.authorId.eq(outer(Post.authorId))).select({ a: func.avg(Post.views) }).asScalar();
  assert.deepEqual((await Post.objects.filter(Post.views.gt(avg)).all()).map((p) => p.title), ["a2"]);
});

test("a scalar subquery in an update", async () => {
  await seed();
  const nComments = Comment.objects.filter(Comment.postId.eq(outer(Post.id))).select({ n: func.count() }).asScalar();
  await Post.objects.update({ views: nComments });
  const views = Object.fromEntries((await Post.objects.all()).map((p) => [p.title, p.views]));
  assert.deepEqual(views, { a1: 0, a2: 2, a3: 0, b1: 1 });
});

test("string functions and concatenation", async () => {
  await seed();
  const rows = await Comment.objects.orderBy(Comment.id).select({ body: Comment.body, c: func.concat(Comment.author.name, "!"), p: Comment.author.name.concat("!") }).all();
  assert.deepEqual(rows, [{ body: "c1", c: "Bob!", p: "Bob!" }, { body: "c2", c: "!", p: null }, { body: "c3", c: "Alice!", p: "Alice!" }]);
  const row = await Post.objects.filter(Post.title.eq("b1")).select({
    t: func.trim(func.concat("  ", Post.title, " ")), l: func.ltrim(func.concat("  ", Post.title)), r: func.rtrim(func.concat(Post.title, "  ")),
    x: func.replace(Post.title, "b", "B"), s: func.substr(Post.title, 2), i: func.strpos(Post.title, "1"),
  }).first();
  assert.deepEqual(row, { t: "b1", l: "b1", r: "b1", x: "B1", s: "1", i: 2 });
  assert.deepEqual((await Post.objects.filter(Post.title.concat("!").eq("a2!")).all()).map((p) => p.title), ["a2"]);
});

test("outer() through relation paths", async () => {
  await seed();
  const others = Comment.objects.filter(Comment.postId.eq(outer(Post.id)), Comment.author.name.ne(outer(Post.author.name))).select({ n: func.count() }).asScalar();
  const rows = await Post.objects.orderBy(Post.title).select({ title: Post.title, n: others }).all();
  assert.deepEqual(rows, [{ title: "a1", n: 0n }, { title: "a2", n: 1n }, { title: "a3", n: 0n }, { title: "b1", n: 1n }]);
  const email = User.objects.filter(User.name.eq(outer(Comment.post.author.name))).select({ e: User.email }).asScalar();
  const bodies = await Comment.objects.orderBy(Comment.id).select({ body: Comment.body, email }).all();
  assert.deepEqual(bodies, [{ body: "c1", email: "alice@example.com" }, { body: "c2", email: "alice@example.com" }, { body: "c3", email: "bob@example.com" }]);
});

test("outer() errors", () => {
  assert.throws(() => (User.objects.filter(exists(Post.objects.filter(Post.authorId.eq(User.id as never)))) as never as { sql(): string }).sql(), /use outer\(User.id\)/);
  assert.throws(() => (User.objects.filter(User.id.eq(outer(User.id))) as never as { sql(): string }).sql(), /not a column of an enclosing query/);
  assert.throws(() => (User.objects.filter(exists(Post.objects.filter(Post.views.eq(outer(User.posts.views as never) as never)))) as never as { sql(): string }).sql(), /needs a to-one relation/);
  assert.throws(() => (Post.objects.select({ id: Post.id, t: Post.title }) as never as { asScalar(): unknown }).asScalar(), /one column/);
});

// -- window functions -----------------------------------------------------------------------------

test("window functions", async () => {
  await seed();
  const rank = func.rowNumber().over({ partitionBy: Post.authorId, orderBy: Post.views.desc() });
  assert.deepEqual(await Post.objects.select({ title: Post.title, rank }).orderBy(Post.title).all(), [
    { title: "a1", rank: 3n }, { title: "a2", rank: 1n }, { title: "a3", rank: 2n }, { title: "b1", rank: 1n },
  ]);

  const running = func.sum(Post.views).over({ partitionBy: Post.authorId, orderBy: Post.createdAt, rows: [null, 0] });
  assert.deepEqual(
    (await Post.objects.select({ title: Post.title, total: running }).orderBy(Post.createdAt).all()).map((r) => [r.title, r.total]),
    [["b1", 100n], ["a1", 5n], ["a2", 55n], ["a3", 75n]],
  );

  const prev = func.lag(Post.views, 1, -1).over({ partitionBy: Post.authorId, orderBy: Post.createdAt });
  const next = func.lead(Post.title).over({ orderBy: Post.createdAt });
  const rows = await Post.objects.filter(Post.author.name.eq("Alice")).select({ title: Post.title, prev, next }).orderBy(Post.createdAt).all();
  assert.deepEqual(rows.map((r) => [r.title, r.prev, r.next]), [["a1", -1, "a2"], ["a2", 5, "a3"], ["a3", 50, null]]);

  const many = await Post.objects
    .select({
      title: Post.title,
      rank: func.rank().over({ orderBy: Post.authorId }),
      dense: func.denseRank().over({ orderBy: Post.authorId }),
      half: func.ntile(2).over({ orderBy: Post.id }),
      n: func.count().over(),
      least: func.firstValue(Post.title).over({ partitionBy: Post.authorId, orderBy: Post.views }),
      pct: func.percentRank().over({ orderBy: Post.views }),
    })
    .orderBy(Post.id)
    .all();
  assert.deepEqual(many.map((r) => [r.rank, r.dense, r.half, r.n, r.least]), [
    [1n, 1n, 1, 4n, "a1"], [1n, 1n, 1, 4n, "a1"], [1n, 1n, 2, 4n, "a1"], [4n, 2n, 2, 4n, "b1"],
  ]);
  assert.equal(many[3]!.pct, 1);

  // Ordering by a window function; a frame by range.
  const titles = await Post.objects.orderBy(func.rowNumber().over({ orderBy: Post.views.desc() })).select({ t: Post.title }).scalars();
  assert.deepEqual(titles, ["b1", "a2", "a3", "a1"]);
  const sums = await Post.objects.select({ s: func.sum(Post.views).over({ orderBy: Post.views, range: [-20, 0] }) }).orderBy(Post.views).scalars();
  assert.deepEqual(sums, [5n, 25n, 50n, 100n]);
});

test("window function errors", async () => {
  const rank = func.rowNumber().over({ orderBy: Post.views });
  await assert.rejects(Post.objects.filter(rank.lte(3)).all(), /window functions can only be used in select/);
  await assert.rejects(Post.objects.update({ views: rank as never }), /window functions can only be used in select/);
  assert.throws(() => func.count().over({ rows: [null, 0], range: [null, 0] }), /rows or range/);
  await getDatabase().transaction(async () => {
    await assert.rejects(Post.objects.lock().orderBy(rank).all(), /window functions/);
  });
});

test("named windows", async () => {
  await seed();
  const w = window({ partitionBy: Post.authorId, orderBy: Post.createdAt });
  const q = Post.objects.select({
    title: Post.title,
    total: func.sum(Post.views).over(w),
    avg: func.avg(Post.views).over(w),
    n: func.rowNumber().over(w),
  });
  assert.equal(q.sql().split("WINDOW").length - 1, 1);
  assert.ok(q.sql().includes("OVER w1"));
  const rows = byTitle(await q.all());
  assert.deepEqual(rows.map((r) => [r.title, r.total, r.n]), [["a1", 5n, 1n], ["a2", 55n, 2n], ["a3", 75n, 3n], ["b1", 100n, 1n]]);
  assert.equal(rows[1]!.avg, 27.5);
  // Extending a window with a frame: OVER (w1 ROWS ...).
  const last2 = func.sum(Post.views).over(w, { rows: [-1, 0] });
  assert.ok(Post.objects.select({ title: Post.title, s: last2 }).sql().includes("OVER (w1 ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)"));
  assert.deepEqual(byTitle(await Post.objects.select({ title: Post.title, s: last2 }).all()).map((r) => r.s), [5n, 55n, 70n, 100n]);
  // ... and a window without orderBy with one.
  const part = window({ partitionBy: Post.authorId });
  const ranked = await Post.objects.select({ title: Post.title, r: func.rank().over(part, { orderBy: Post.views.desc() }) }).all();
  assert.deepEqual(byTitle(ranked).map((r) => r.r), [3n, 1n, 2n, 1n]);
  // The same window in a subquery is declared there.
  const cte = Post.objects
    .select({ post: Post, rank: func.rowNumber().over(window({ partitionBy: Post.authorId, orderBy: Post.views.desc() })) })
    .cte("ranked");
  assert.deepEqual((await Post.objects.from(cte).filter(cte.c.rank.eq(1)).orderBy(Post.title).all()).map((p) => p.title), ["a2", "b1"]);
});

test("named window limits", async () => {
  const w = window({ partitionBy: Post.authorId, orderBy: Post.createdAt });
  assert.throws(() => func.sum(Post.views).over(w, { orderBy: Post.id }), /without its own orderBy/);
  assert.throws(() => func.sum(Post.views).over(window({ rows: [null, 0] }), { rows: [null, 0] }), /without its own frame/);
  const other = window({ orderBy: Post.id });
  await assert.rejects(Post.objects.select({ a: func.sum(Post.views).over(w), b: func.sum(Post.views).over(other) }).all(), /only one named window/);
  await assert.rejects(Post.objects.select({ a: func.sum(Post.views).over(w) }).orderBy(Post.id).all(), /order_by/);
  await assert.rejects(Post.objects.select({ a: func.sum(Post.views).over(w) }).limit(3).all(), /slicing/);
});

// -- CTEs -----------------------------------------------------------------------------------------

test("joining a CTE", async () => {
  await seed();
  const totals = Post.objects.select({ authorId: Post.authorId, views: func.sum(Post.views), n: func.count() }).groupBy(Post.authorId).cte("totals");
  const rows = await User.objects.join(totals, totals.c.authorId.eq(User.id)).select({ user: User, views: totals.c.views }).orderBy(totals.c.views.desc()).all();
  assert.deepEqual(rows.map((r) => [r.user.name, r.views]), [["Bob", 100n], ["Alice", 75n]]);
  const outerRows = await User.objects.join(totals, totals.c.authorId.eq(User.id), { outer: true }).select({ name: User.name, n: totals.c.n }).orderBy(User.id).all();
  assert.deepEqual(outerRows, [{ name: "Alice", n: 3n }, { name: "Bob", n: 1n }, { name: "Carol", n: null }]);
  // Filters, instances, count, prefetch.
  const busy = User.objects.join(totals, totals.c.authorId.eq(User.id)).filter(totals.c.n.gt(1));
  assert.deepEqual((await busy.all()).map((u) => u.name), ["Alice"]);
  assert.equal(await busy.count(), 1);
  assert.deepEqual((await busy.prefetchRelated(User.posts).all()).map((u) => u.posts.cached.length), [3]);
  // Two CTEs joined.
  const commenters = Comment.objects.select({ authorId: Comment.authorId, c: func.count() }).groupBy(Comment.authorId).cte("commenters");
  const both = await User.objects
    .join(totals, totals.c.authorId.eq(User.id))
    .join(commenters, commenters.c.authorId.eq(User.id))
    .select({ name: User.name, views: totals.c.views, c: commenters.c.c })
    .orderBy(User.name)
    .all();
  assert.deepEqual(both, [{ name: "Alice", views: 75n, c: 1n }, { name: "Bob", views: 100n, c: 1n }]);
  await assert.rejects(User.objects.join(totals, totals.c.authorId.eq(User.id)).update({ name: "x" }), /can't run on a query set with from\(\)/);
  assert.throws(() => User.objects.join(totals, totals.c.authorId.eq(User.id)).join(totals, totals.c.n.gt(0)), /already read/);
});

test("a recursive CTE with a join", async () => {
  const users = await User.objects.insertMany(Array.from({ length: 5 }, (_, i) => ({ email: `u${i}@x.io`, name: `u${i}` })));
  const first = users[0]!.id;
  const walk = User.objects
    .filter(User.id.eq(first))
    .select({ id: User.id, name: User.name, depth: func.abs(User.id.sub(User.id)) })
    .cte("walk", {
      recursive: (w) =>
        User.objects.join(w as never, User.id.eq(w.c.id.add(1))).filter(w.c.depth.lt(2)).select({ id: User.id, name: User.name, depth: w.c.depth.add(1) }),
    });
  assert.ok(walk.select({ name: walk.c.name }).sql().includes('JOIN "walk" ON'));
  assert.deepEqual(await walk.select({ name: walk.c.name, depth: walk.c.depth }).orderBy(walk.c.depth).all(), [
    { name: "u0", depth: 0n }, { name: "u1", depth: 1n }, { name: "u2", depth: 2n },
  ]);
});

test("reading rows from a CTE", async () => {
  await seed();
  const rank = func.rowNumber().over({ partitionBy: Post.authorId, orderBy: Post.views.desc() });
  const ranked = Post.objects.select({ post: Post, rank }).cte("ranked");
  let top = await Post.objects.from(ranked).filter(ranked.c.rank.lte(2)).orderBy(Post.title).all();
  assert.deepEqual(top.map((p) => p.title), ["a2", "a3", "b1"]);
  // Relation filters, selectRelated and prefetch still work on rows read from a CTE.
  const withAuthor = await Post.objects.from(ranked).filter(ranked.c.rank.eq(1), Post.author.name.eq("Alice")).selectRelated(Post.author).all();
  assert.deepEqual(withAuthor.map((p) => [p.title, p.author.name]), [["a2", "Alice"]]);
  const withComments = await Post.objects.from(ranked).filter(ranked.c.rank.eq(1)).prefetchRelated(Post.comments).orderBy(Post.title).all();
  assert.deepEqual(withComments.map((p) => p.comments.cached.length), [2, 1]);
  assert.equal(await Post.objects.from(ranked).filter(ranked.c.rank.eq(1)).count(), 2);
  assert.equal(await Post.objects.from(ranked).filter(ranked.c.rank.eq(9)).exists(), false);
  const rows = await Post.objects.from(ranked).select({ title: Post.title, rank: ranked.c.rank }).orderBy(Post.title).all();
  assert.deepEqual(rows.map((r) => [r.title, r.rank]), [["a1", 3n], ["a2", 1n], ["a3", 2n], ["b1", 1n]]);
  top = await Post.objects.from(Post.objects.filter(Post.views.gt(30)).cte("big")).orderBy(Post.id).all();
  assert.deepEqual(top.map((p) => p.title), ["a2", "b1"]);
});

test("CTE select() and subqueries", async () => {
  const { alice, bob } = await seed();
  const totals = Post.objects.select({ authorId: Post.authorId, views: func.sum(Post.views), n: func.count() }).groupBy(Post.authorId).cte("totals");
  assert.deepEqual(await totals.select({ a: totals.c.authorId, v: totals.c.views }).filter(totals.c.n.gt(1)).all(), [{ a: alice.id, v: 75n }]);
  assert.deepEqual(await totals.select({ best: func.max(totals.c.views) }).all(), [{ best: 100n }]);
  // A CTE read from a subquery: declared on the statement, once.
  const heavy = User.objects.filter(User.id.in(totals.select({ a: totals.c.authorId }).filter(totals.c.views.gt(80))));
  assert.deepEqual((await heavy.all()).map((u) => u.name), ["Bob"]);
  assert.equal(heavy.sql().split("WITH").length - 1, 1);
  const big = exists(totals.select({ n: totals.c.n }).filter(totals.c.authorId.eq(outer(User.id)), totals.c.views.gt(50)));
  assert.deepEqual((await User.objects.filter(big).orderBy(User.id).all()).map((u) => u.name), ["Alice", "Bob"]);
  // ... in writes too.
  assert.equal(await User.objects.filter(User.id.in(totals.select({ a: totals.c.authorId }))).update({ name: "writer" }), 2);
  assert.equal(await User.objects.filter(User.name.eq("writer")).count(), 2);
  // CTEs reading CTEs, materialized.
  const top = totals.select({ authorId: totals.c.authorId }).filter(totals.c.views.gt(80)).cte("top", { materialized: true });
  assert.ok(User.objects.filter(User.id.in(top.select({ a: top.c.authorId }))).sql().includes("MATERIALIZED"));
  assert.deepEqual((await User.objects.filter(User.id.in(top.select({ a: top.c.authorId }))).all()).map((u) => u.id), [bob.id]);
  assert.equal(await Post.objects.filter(Post.authorId.in(totals.select({ a: totals.c.authorId }))).delete(), 4);
});

test("recursive CTEs", async () => {
  const users = await User.objects.insertMany(Array.from({ length: 5 }, (_, i) => ({ email: `u${i}@x.io`, name: `u${i}` })));
  const first = users[0]!.id;
  const chain = User.objects.filter(User.id.eq(first)).cte("chain", {
    recursive: (c) => User.objects.filter(User.id.eq(c.c.id.add(1)), User.id.lt(first + 3n)),
  });
  assert.deepEqual((await User.objects.from(chain).orderBy(User.id).all()).map((u) => u.name), ["u0", "u1", "u2"]);
  const walk = User.objects.filter(User.id.eq(first)).select({ id: User.id, depth: func.abs(User.id.sub(User.id)) }).cte("walk", {
    recursive: (w) => User.objects.filter(User.id.eq(w.c.id.add(1))).select({ id: User.id, depth: w.c.depth.add(1) }),
  });
  assert.deepEqual(await walk.select({ d: walk.c.depth }).orderBy(walk.c.depth.desc()).limit(1).scalars(), [4n]);
});

test("CTE errors", async () => {
  const totals = Post.objects.select({ authorId: Post.authorId, n: func.count() }).groupBy(Post.authorId).cte("totals");
  assert.throws(() => Post.objects.from(totals as never), /columns of Post/);
  await assert.rejects(Post.objects.filter(totals.c.n.gt(1) as never).all(), /only available in queries reading totals/);
  await assert.rejects(Post.objects.from(Post.objects.cte("p")).delete(), /can't run on a query set with from\(\)/);
  const other = Post.objects.select({ authorId: Post.authorId }).cte("totals");
  assert.throws(
    () => User.objects.filter(User.id.in(totals.select({ a: totals.c.authorId })), User.id.in(other.select({ a: other.c.authorId }))).sql(),
    /two different CTEs/,
  );
  assert.throws(() => Post.objects.select({ post: Post, id: Post.id }).cte("x"), /several columns named id/);
});

// -- prefetch -------------------------------------------------------------------------------------

test("nested prefetch", async () => {
  await seed();
  const [a, b, c] = await User.objects.prefetchRelated(User.posts.comments, User.comments).orderBy(User.id).all();
  assert.deepEqual(a!.posts.cached.map((p) => p.title), ["a1", "a2", "a3"]);
  assert.deepEqual(a!.posts.cached.map((p) => p.comments.cached.map((x) => x.body)), [[], ["c1", "c2"], []]);
  const back = (x: unknown, rel: string) => (x as Record<string, unknown>)[rel];
  assert.equal(back(a!.posts.cached[1]!.comments.cached[0], "post"), a!.posts.cached[1]); // back reference
  assert.equal(back(a!.posts.cached[0], "author"), a);
  assert.deepEqual(b!.comments.cached.map((x) => x.body), ["c1"]);
  assert.deepEqual(c!.posts.cached, []);
});

test("to-one prefetch", async () => {
  await seed();
  const comments = await Comment.objects.prefetchRelated(Comment.post.author, Comment.author).orderBy(Comment.id).all();
  assert.deepEqual(comments.map((x) => [x.post.title, x.post.author.name]), [["a2", "Alice"], ["a2", "Alice"], ["b1", "Bob"]]);
  assert.equal(comments[0]!.post, comments[1]!.post); // one object per related row
  assert.deepEqual(comments.map((x) => x.author?.name ?? null), ["Bob", null, "Alice"]);
});

test("filtered prefetch", async () => {
  await seed();
  let users = await User.objects.prefetchRelated(new Prefetch(User.posts, Post.objects.filter(Post.views.gte(20)).orderBy(Post.views.desc()))).orderBy(User.id).all();
  // Like Django: the filtered rows are what user.posts holds now.
  assert.deepEqual(users[0]!.posts.cached.map((p) => p.title), ["a2", "a3"]);
  assert.deepEqual((await users[0]!.posts.all()).map((p) => p.title), ["a2", "a3"]);
  assert.equal(await users[0]!.posts.count(), 3); // a new query sees every post
  const popular = await User.objects.prefetchRelated(new Prefetch(User.posts, Post.objects.filter(Post.views.gte(20)), { toAttr: "popular" })).orderBy(User.id).all();
  assert.deepEqual(popular[0]!.popular.map((p) => p.title), ["a2", "a3"]);
  assert.deepEqual(popular[2]!.popular, []);
  assert.throws(() => (popular[0]!.posts as unknown as { cached: unknown }).cached, NotLoaded);
  users = [];
});

test("a sliced prefetch is per parent", async () => {
  await seed();
  const top = Post.objects.orderBy(Post.views.desc()).prefetchRelated(Post.comments).limit(2);
  const users = await User.objects.prefetchRelated(new Prefetch(User.posts, top, { toAttr: "top" })).orderBy(User.id).all();
  assert.deepEqual(users.map((u) => u.top.map((p) => p.title)), [["a2", "a3"], ["b1"], []]);
  assert.deepEqual(users[0]!.top.map((p) => p.comments.cached.length), [2, 0]);
  const second = Post.objects.orderBy(Post.createdAt, Post.author.name).slice(1, 2);
  const again = await User.objects.prefetchRelated(new Prefetch(User.posts, second)).orderBy(User.id).all();
  assert.deepEqual(again.map((u) => u.posts.cached.map((p) => p.title)), [["a2"], [], []]);
});

test("prefetch with selectRelated and a nested query set", async () => {
  await seed();
  const users = await User.objects
    .prefetchRelated(
      new Prefetch(User.comments, Comment.objects.selectRelated(Comment.post.author)),
      new Prefetch(User.posts, Post.objects.prefetchRelated(new Prefetch(Post.comments, Comment.objects.filter(Comment.authorId.isNull())))),
    )
    .orderBy(User.id)
    .all();
  assert.deepEqual(users[1]!.comments.cached.map((x) => [x.post.title, x.post.author.name]), [["a2", "Alice"]]);
  assert.deepEqual(users[0]!.posts.cached.map((p) => p.comments.cached.map((x) => x.body)), [[], ["c2"], []]);
});

test("prefetch errors", () => {
  assert.throws(() => User.objects.prefetchRelated(new Prefetch(User.posts, Post.objects), new Prefetch(User.posts, Post.objects.filter(Post.id.gt(0)))), /different query sets/);
  assert.throws(() => new Prefetch(User.posts, Comment.objects as never), /query set of Post/);
  assert.throws(() => User.objects.prefetchRelated(Post.comments as never), /does not start at User/);
  assert.throws(() => User.objects.prefetchRelated(new Prefetch(User.posts, { toAttr: "email" })), /is a field or relation/);
});

test("instances get their database", async () => {
  await seed();
  const db = getDatabase();
  const users = await User.objects.using(db).prefetchRelated(User.posts).all();
  const dbOf = (o: object) => (o as Record<symbol, unknown>)[DB];
  assert.ok(users.every((u) => dbOf(u) === db));
  assert.ok(users.every((u) => u.posts.cached.every((p) => dbOf(p) === db)));
  const rows = await User.objects.using(db).select({ user: User, name: User.name }).all();
  assert.equal(dbOf(rows[0]!.user), db);
  assert.equal(dbOf(await User.objects.using(db).insert({ email: "z@x.io", name: "Z" })), db);
  assert.equal(dbOf((await User.objects.first())!), undefined);
});

test("a prefetch query reading a CTE", async () => {
  await seed();
  const best = Post.objects.select({ id: Post.id }).filter(Post.views.gte(50)).cte("best");
  const users = await User.objects
    .prefetchRelated(new Prefetch(User.posts, Post.objects.filter(Post.id.in(best.select({ id: best.c.id }))), { toAttr: "best" }))
    .orderBy(User.id)
    .all();
  assert.deepEqual(users.map((u) => u.best.map((p) => p.title)), [["a2"], ["b1"], []]);
});

test("a correlated in() subquery", async () => {
  await seed();
  const commented = Comment.objects.filter(Comment.authorId.eq(outer(User.id))).select({ p: Comment.postId });
  const rows = await User.objects.filter(exists(Post.objects.filter(Post.id.in(commented)))).orderBy(User.id).all();
  assert.deepEqual(rows.map((u) => u.name), ["Alice", "Bob"]);
});

// -- prefetch over many parents: keys split across queries -----------------------------------------

test("an invalid max_params option", async () => {
  await assert.rejects(connect(DATABASE_URL, { default: false, disable: ["max_params=abc"] }), QueryError);
});

test("prefetch splits keys", async () => {
  // A pool whose statements take at most 5 parameters, so prefetches split after a few parents.
  const db = await connect(DATABASE_URL, { maxConnections: 2, default: false, disable: ["max_params=5"] });
  try {
    const users = await User.objects.insertMany(Array.from({ length: 12 }, (_, i) => ({ email: `u${i}@x.io`, name: `u${i}` })));
    const posts = await Post.objects.insertMany(users.flatMap((u) => [0, 1, 2].map((j) => ({ author: u, title: `${u.name}-${j}`, body: "", views: j }))));
    await Comment.objects.insertMany(posts.filter((_, i) => i % 2 === 0).map((p) => ({ post: p, body: `on ${p.title}` })));
    const qs = User.objects.prefetchRelated(User.posts.comments).orderBy(User.id);
    const shape = (us: Awaited<ReturnType<typeof qs.all>>) => us.map((u) => u.posts.cached.map((p) => [p.title, p.comments.cached.map((c) => c.body)]));
    assert.deepEqual(shape(await qs.using(db).all()), shape(await qs.all()));
    assert.equal((await qs.using(db).all()).reduce((n, u) => n + u.posts.cached.length, 0), 36);
    // A slice per parent still holds: each parent's rows stay in one query.
    const top = new Prefetch(User.posts, Post.objects.filter(Post.views.gte(0)).orderBy(Post.views.desc()).limit(2), { toAttr: "top" });
    const got = await User.objects.prefetchRelated(top).orderBy(User.id).using(db).all();
    assert.deepEqual(got.map((u) => u.top.map((p) => p.views)), Array.from({ length: 12 }, () => [2, 1]));
    // To-one, with repeated keys.
    const cs = await Comment.objects.prefetchRelated(Comment.post.author).orderBy(Comment.id).using(db).all();
    const nameOf = new Map(users.map((u) => [u.id, u.name]));
    assert.deepEqual(cs.map((c) => c.post.author.name), posts.filter((_, i) => i % 2 === 0).map((p) => nameOf.get(p.authorId)));
  } finally {
    await db.close();
  }
});

test("prefetch beyond the parameter limit", async () => {
  // 70 000 parents: more keys than Postgres takes parameters in one statement.
  const db = getDatabase();
  await db.execute("INSERT INTO users (email, name) SELECT 'u' || g || '@x.io', 'u' FROM generate_series(1, 70000) g");
  await db.execute("INSERT INTO posts (author_id, title, body) SELECT id, 't', '' FROM users WHERE id % 10000 = 0");
  const users = await User.objects.prefetchRelated(User.posts).all();
  assert.equal(users.length, 70000);
  assert.equal(users.reduce((n, u) => n + u.posts.cached.length, 0), 7);
});
