/** Decimal, enum and array columns, one-to-one and many-to-many relations (blog
 * example: Profile, Tag, PostTag). */

import assert from "node:assert/strict";
import { test } from "node:test";

import { DatabaseError, Decimal, IntegrityError, NotLoaded, func } from "../src/index.js";
import { Post, PostTag, Priority, Profile, Role, Tag, User } from "./blog/models.js";
import { useDatabase } from "./helpers.js";

useDatabase();

async function users() {
  const alice = await User.objects.insert({ email: "alice@example.com", name: "Alice" });
  const bob = await User.objects.insert({ email: "bob@example.com", name: "Bob" });
  return { alice, bob };
}

// -- decimals -------------------------------------------------------------------------------------

test("decimal values are exact", async () => {
  const { alice, bob } = await users();
  const p = await Profile.objects.insert({ user: alice, balance: new Decimal("10.25") });
  assert.ok(p.balance instanceof Decimal && p.balance.eq("10.25"));
  const q = await Profile.objects.insert({ user: bob });
  assert.ok(q.balance.eq(0));
  // numbers and decimal text are accepted
  await q.update({ balance: "0.10" });
  assert.ok(q.balance.eq("0.1"));
  await q.update({ balance: Profile.balance.add(new Decimal("0.20")) });
  assert.equal(q.balance.toFixed(2), "0.30"); // 0.1 + 0.2, exactly
  await q.update({ balance: 3 });
  assert.equal(await Profile.objects.filter(Profile.balance.gt(new Decimal("5"))).count(), 1);
  assert.equal(await Profile.objects.filter(Profile.balance.in([3, new Decimal("10.25")])).count(), 2);
  const total = await Profile.objects.select({ s: func.sum(Profile.balance) }).scalar();
  assert.ok(total instanceof Decimal && total.eq("13.25"));
  const avg = await Profile.objects.select({ a: func.avg(Profile.balance) }).scalar();
  assert.ok(avg instanceof Decimal && avg.eq("6.625"));
  await assert.rejects(q.update({ balance: "lots" }), (e: unknown) => e instanceof TypeError && /finite decimal/.test(e.message));
  await assert.rejects(q.update({ balance: Number.NaN }), /finite decimal/);
  // values numeric(12, 2) can't hold are the database's error
  await assert.rejects(q.update({ balance: new Decimal("1e20") }), DatabaseError);
});

test("the decimal wire format round-trips", async () => {
  // SUM over no rows is NULL, so COALESCE gives the parameter back: any precision
  for (const text of ["0", "-1", "12345678901234567890.123456789", "0.000001", "-0.5", "100000000", "1E+3"]) {
    const back = await Profile.objects.select({ v: func.coalesce(func.sum(Profile.balance), new Decimal(text)) }).scalar();
    assert.ok(back!.eq(text), `${text} came back as ${String(back)}`);
  }
  const { alice } = await users();
  await Profile.objects.insert({ user: alice });
  for (const text of ["1234567890.12", "-0.01", "0.50", "-99999999.99"]) {
    const [p] = await Profile.objects.update({ balance: new Decimal(text) }, { returning: true });
    assert.equal(p!.balance.toFixed(2), text);
  }
});

// -- enums ----------------------------------------------------------------------------------------

test("a native enum", async () => {
  const { alice, bob } = await users();
  const a = await Profile.objects.insert({ user: alice });
  assert.equal(a.role, Role.member); // the default
  const b = await Profile.objects.insert({ user: bob, role: "admin" });
  assert.equal(b.role, Role.admin);
  assert.deepEqual((await Profile.objects.filter(Profile.role.eq(Role.admin)).all()).map((p) => p.userId), [bob.id]);
  assert.equal(await Profile.objects.filter(Profile.role.in([Role.member, Role.editor])).count(), 1);
  await a.update({ role: Role.editor });
  assert.equal(a.role, "editor");
  assert.deepEqual(await Profile.objects.select({ role: Profile.role }).orderBy(Profile.role).scalars(), ["editor", "admin"]); // the enum's order
  await assert.rejects(a.update({ role: "owner" as Role }), /invalid input value for enum/);
});

test("an int enum", async () => {
  const t = await Tag.objects.insert({ name: "news" });
  assert.equal(t.priority, Priority.normal);
  const hot = await Tag.objects.insert({ name: "hot", priority: Priority.high });
  assert.equal(hot.priority, 3);
  assert.deepEqual((await Tag.objects.filter(Tag.priority.gt(Priority.normal)).all()).map((x) => x.name), ["hot"]);
  assert.equal(await Tag.objects.select({ m: func.max(Tag.priority) }).scalar(), Priority.high);
  await assert.rejects(Tag.objects.insert({ name: "bad", priority: 7 as Priority }), IntegrityError);
});

// -- arrays ---------------------------------------------------------------------------------------

test("array columns", async () => {
  const { alice, bob } = await users();
  const a = await Profile.objects.insert({ user: alice, links: ["https://a.example", "https://b.example"] });
  const b = await Profile.objects.insert({ user: bob });
  assert.deepEqual(a.links, ["https://a.example", "https://b.example"]);
  assert.deepEqual(b.links, []);
  assert.equal(await Profile.objects.filter(Profile.links.has("https://a.example")).count(), 1);
  assert.equal(await Profile.objects.filter(Profile.links.hasAll(["https://a.example", "https://x"])).count(), 0);
  assert.equal(await Profile.objects.filter(Profile.links.hasAny(["https://x", "https://b.example"])).count(), 1);
  assert.equal(await Profile.objects.filter(Profile.links.containedBy(["https://x"])).count(), 1); // the empty one
  assert.equal(await Profile.objects.filter(Profile.links.eq([])).count(), 1);
  await b.update({ links: ["one", null as never] });
  assert.deepEqual(b.links, ["one", null]);
  assert.deepEqual(await Profile.objects.select({ n: func.cardinality(Profile.links) }).orderBy(Profile.id).scalars(), [2, 2]);
  await assert.rejects(b.update({ links: "one" as never }), /expected an array/);
  const rows = await Profile.objects.select({ first: Profile.links.element(1), third: Profile.links.element(3) }).orderBy(Profile.id).all();
  assert.deepEqual(rows, [{ first: "https://a.example", third: null }, { first: "one", third: null }]);
  assert.equal(await Profile.objects.filter(Profile.links.element(2).eq("https://b.example")).count(), 1);
  const all = await Profile.objects.select({ link: func.unnest(Profile.links) }).scalars();
  assert.deepEqual([...all].sort((x, y) => String(x).localeCompare(String(y))), ["https://a.example", "https://b.example", null, "one"]);
});

// -- one-to-one -----------------------------------------------------------------------------------

test("a has-one relation", async () => {
  const { alice } = await users();
  const p = await Profile.objects.insert({ user: alice, role: Role.admin });
  assert.throws(() => (alice as unknown as { profile: unknown }).profile, NotLoaded);
  let us = await User.objects.load(User.profile).orderBy(User.id).all();
  assert.equal(us[0]!.profile!.id, p.id);
  assert.equal(us[1]!.profile, null);
  const pre = await User.objects.load(User.profile.objects.asPrefetch()).orderBy(User.id).all();
  assert.equal(pre[0]!.profile!.id, p.id);
  assert.equal(pre[1]!.profile, null);
  assert.equal((pre[0]!.profile as unknown as { user: unknown }).user, pre[0]); // the back side too
  assert.deepEqual((await User.objects.filter(User.profile.role.eq(Role.admin)).all()).map((u) => u.name), ["Alice"]);
  assert.deepEqual((await User.objects.exclude(User.profile.role.eq(Role.admin)).all()).map((u) => u.name), ["Bob"]);
  const rows = await User.objects.select({ name: User.name, role: User.profile.role }).orderBy(User.id).all();
  assert.deepEqual(rows, [{ name: "Alice", role: "admin" }, { name: "Bob", role: null }]);
  await assert.rejects(Profile.objects.insert({ user: alice }), IntegrityError); // one profile per user
  us = [];
});

// -- many-to-many ---------------------------------------------------------------------------------

async function blog() {
  const { alice, bob } = await users();
  const [p1, p2, p3] = (await Post.objects.insertMany([
    { author: alice, title: "one", body: "." },
    { author: alice, title: "two", body: "." },
    { author: bob, title: "three", body: "." },
  ]).returning()) as [Post, Post, Post];
  const [news, rust, py] = (await Tag.objects.insertMany([{ name: "news" }, { name: "rust" }, { name: "python" }]).returning()) as [Tag, Tag, Tag];
  await p1.tags.add(news, rust);
  await p2.tags.add(rust.id); // keys work too
  await p2.tags.add(rust); // existing links are left alone
  return { p1, p2, p3, news, rust, py };
}

test("many-to-many links and queries", async () => {
  const { p1, p3, news, rust, py } = await blog();
  assert.equal(await PostTag.objects.count(), 3);
  assert.deepEqual((await p1.tags.orderBy(Tag.name).all()).map((t) => t.name), ["news", "rust"]);
  assert.deepEqual((await rust.posts.orderBy(Post.id).all()).map((p) => p.title), ["one", "two"]);
  assert.equal(await p3.tags.count(), 0);
  // filters follow both hops, one EXISTS per filter() call
  assert.deepEqual((await Post.objects.filter(Post.tags.name.eq("rust")).orderBy(Post.id).all()).map((p) => p.title), ["one", "two"]);
  const both = Post.objects.filter(Post.tags.name.eq("rust")).filter(Post.tags.name.eq("news"));
  assert.deepEqual((await both.all()).map((p) => p.title), ["one"]);
  assert.equal(await Post.objects.filter(Post.tags.name.eq("rust"), Post.tags.name.eq("news")).count(), 0);
  assert.deepEqual((await Post.objects.exclude(Post.tags.name.eq("rust")).all()).map((p) => p.title), ["three"]);
  assert.deepEqual((await User.objects.filter(User.posts.tags.name.eq("news")).all()).map((u) => u.name), ["Alice"]);
  // aggregates over the relation
  assert.deepEqual(await Post.objects.select({ title: Post.title, n: func.count(Post.tags) }).orderBy(Post.id).all(), [
    { title: "one", n: 2n }, { title: "two", n: 1n }, { title: "three", n: 0n },
  ]);
  assert.deepEqual((await Tag.objects.filter(func.count(Tag.posts).eq(0)).all()).map((t) => t.name), ["python"]);

  // unlinking
  assert.equal(await p1.tags.remove(news), 1);
  assert.equal(await p1.tags.remove(news), 0);
  await p1.tags.set([news, py]);
  assert.deepEqual((await p1.tags.all()).map((t) => t.name).sort(), ["news", "python"]);
  assert.equal(await p1.tags.clear(), 2);
  assert.equal(await p1.tags.count(), 0);
  // insert and link
  const t = await p3.tags.insert({ name: "go" });
  assert.deepEqual((await p3.tags.all()).map((x) => x.name), ["go"]);
  assert.equal(t.priority, Priority.normal);
  await assert.rejects(p3.tags.add(p1 as never), /links Tag/);
});

test("many-to-many prefetch", async () => {
  const { py } = await blog();
  let posts = await Post.objects.load(Post.tags).orderBy(Post.id).all();
  assert.deepEqual(posts.map((p) => p.tags.cached.map((t) => t.name)), [["news", "rust"], ["rust"], []]);
  assert.deepEqual((await posts[1]!.tags.all()).map((t) => t.name), ["rust"]); // served from the prefetched rows
  // nested, both directions
  const tags = await Tag.objects.load(Tag.posts.author).orderBy(Tag.id).all();
  assert.deepEqual(tags.map((t) => t.posts.cached.map((p) => [p.title, p.author.name])), [
    [["one", "Alice"]],
    [["one", "Alice"], ["two", "Alice"]],
    [],
  ]);
  // a slice applies per parent
  const top = Post.tags.objects.orderBy(Tag.name.desc()).limit(1).label("firstTag");
  const firsts = await Post.objects.load(top).orderBy(Post.id).all();
  assert.deepEqual(firsts.map((p) => p.firstTag.map((t) => t.name)), [["rust"], ["rust"], []]);
  // filtered
  posts = await Post.objects.load(Post.tags.objects.filter(Tag.name.ne("rust"))).orderBy(Post.id).all();
  assert.deepEqual(posts.map((p) => p.tags.cached.map((t) => t.name)), [["news"], [], []]);
  // links changed: the prefetched rows are dropped
  await posts[0]!.tags.add(py);
  assert.throws(() => posts[0]!.tags.cached, NotLoaded);
});
