/** Query-set API: query-set classes, prefetch on loaded instances, only() through to-one
 * paths, OR of query sets, single-row Prefetch, column paths and model metadata. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { ManyRelatedSet, Prefetch, QuerySet, RelatedSet, prefetch, useQuerySet } from "../src/index.js";
import { Comment, Post, Tag, User, type PostSpec, type TagSpec } from "./blog/models.js";
import { useDatabase } from "./helpers.js";

useDatabase();

class PostQueries extends QuerySet<PostSpec> {
  published(): this {
    return this.filter(Post.published.eq(true)) as this;
  }

  popular(views = 10): this {
    return this.filter(Post.views.gte(views)) as this;
  }
}

class TagQueries extends QuerySet<TagSpec> {
  named(prefix: string): this {
    return this.filter(Tag.name.startsWith(prefix)) as this;
  }
}

const posts = () => Post.objects as unknown as PostQueries;

async function seed() {
  const alice = await User.objects.insert({ email: "alice@example.com", name: "Alice" });
  const bob = await User.objects.insert({ email: "bob@example.com", name: "Bob" });
  const rows = await Post.objects.insertMany([
    { author: alice, title: "draft", body: "...", views: 50 },
    { author: alice, title: "hit", body: "...", published: true, views: 90 },
    { author: alice, title: "quiet", body: "...", published: true, views: 1 },
    { author: bob, title: "bob", body: "...", published: true, views: 70 },
  ]);
  return { alice, bob, posts: rows };
}

// -- query-set classes ------------------------------------------------------------------------

test("query-set classes", async (t) => {
  useQuerySet(Post, () => PostQueries);
  useQuerySet(Tag, TagQueries);
  t.after(() => {
    useQuerySet(Post, QuerySet);
    useQuerySet(Tag, QuerySet);
  });

  await t.test("custom methods chain with builder methods", async () => {
    const { alice } = await seed();
    const qs = (posts().published().filter(Post.authorId.eq(alice.id)) as unknown as PostQueries).popular(10);
    assert.ok(qs instanceof PostQueries);
    assert.deepEqual((await qs).map((p) => p.title), ["hit"]);
    assert.deepEqual((await (posts().orderBy("views") as unknown as PostQueries).popular(60).published()).map((p) => p.title), ["bob", "hit"]);
    assert.equal(await posts().published().count(), 3);
    assert.equal(Post.objects, Post.objects);
  });

  await t.test("relation sets have the custom methods", async () => {
    const { alice, posts: rows } = await seed();
    const set = alice.posts as unknown as PostQueries;
    assert.ok(alice.posts instanceof RelatedSet && alice.posts instanceof PostQueries);
    assert.deepEqual((await (set.published().orderBy("views") as unknown as PostQueries)).map((p) => p.title), ["quiet", "hit"]);
    assert.deepEqual((await (alice.posts.filter(Post.views.gt(5)) as unknown as PostQueries).published()).map((p) => p.title), ["hit"]);
    const tags = await Tag.objects.insertMany([{ name: "python" }, { name: "rust" }]);
    await rows[1]!.tags.add(...tags);
    assert.ok(rows[1]!.tags instanceof ManyRelatedSet);
    assert.deepEqual((await (rows[1]!.tags as unknown as TagQueries).named("py")).map((t) => t.name), ["python"]);
  });

  await t.test("a Prefetch query set uses them", async () => {
    await seed();
    const users = await User.objects.orderBy("name").prefetchRelated(new Prefetch(User.posts, posts().published() as never));
    assert.deepEqual(users.map((u) => (u.posts as unknown as { cached: { title: string }[] }).cached.map((p) => p.title)), [["hit", "quiet"], ["bob"]]);
  });
});

test("a query-set class is checked", () => {
  assert.throws(() => useQuerySet(Post, Object as never), /extends QuerySet/);
  useQuerySet(Post, (() => Map) as never);
  try {
    assert.throws(() => Post.objects, /extends QuerySet/);
  } finally {
    useQuerySet(Post, QuerySet);
  }
  assert.equal(Post.objects.constructor, QuerySet);
});

// -- prefetch on loaded instances -------------------------------------------------------------

test("prefetch onto loaded instances", async () => {
  const { posts: rows } = await seed();
  await Comment.objects.insertMany([{ post: rows[1]!, body: "a" }, { post: rows[1]!, body: "b" }]);
  const users = await User.objects.orderBy("id");
  await prefetch(users, User.posts.comments, new Prefetch(User.posts, Post.objects.filter(Post.published.eq(true)).slice(0, 1), { toAttr: "top" }));
  type Loaded = { posts: { cached: { title: string; comments: { cached: { body: string }[] } }[] }; top: { title: string }[] };
  const loaded = users as unknown as Loaded[];
  assert.deepEqual(loaded[0]!.posts.cached.map((p) => p.title), ["draft", "hit", "quiet"]);
  assert.deepEqual(loaded[0]!.posts.cached[1]!.comments.cached.map((c) => c.body), ["a", "b"]);
  assert.deepEqual(loaded.map((u) => u.top.map((p) => p.title)), [["hit"], ["bob"]]);
});

test("prefetch reads keys from the instances, to-one and many-to-many", async () => {
  const { posts: rows } = await seed();
  await Comment.objects.insertMany([{ post: rows[0]!, body: "a" }, { post: rows[3]!, body: "b" }]);
  const comments = await Comment.objects.orderBy("id");
  await Comment.objects.delete(); // the instances' own rows are not read again
  await prefetch(comments, Comment.post.author);
  const loaded = comments as unknown as { post: { title: string; author: { name: string } } }[];
  assert.deepEqual(loaded.map((c) => [c.post.title, c.post.author.name]), [["draft", "Alice"], ["bob", "Bob"]]);
  const tags = await Tag.objects.insertMany([{ name: "a" }, { name: "b" }]);
  await rows[0]!.tags.add(...tags);
  const posts = await Post.objects.orderBy("id");
  await prefetch(posts, Post.tags);
  assert.deepEqual((posts[0]!.tags as unknown as { cached: { name: string }[] }).cached.map((t) => t.name), ["a", "b"]);
});

test("prefetch checks its input", async () => {
  const { alice, posts: rows } = await seed();
  await prefetch([], User.posts);
  await assert.rejects(prefetch([alice, rows[0]!], User.posts), /one model/);
  await assert.rejects(prefetch([alice], Post.author as never), /does not start at User|User has no relation/);
});
