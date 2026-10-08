/** Query-set API: query-set classes, prefetch on loaded instances, only() through to-one
 * paths, OR of query sets, single-row Prefetch, column paths and model metadata. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { ManyRelatedSet, NotLoaded, Prefetch, QueryError, QuerySet, RelatedSet, column, describe, prefetch, useQuerySet, type ModelClass, type ModelSpec } from "../src/index.js";
import { Comment, Post, PostTag, Profile, Tag, User, type PostSpec, type TagSpec } from "./blog/models.js";
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
  ]).returning();
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
    const tags = await Tag.objects.insertMany([{ name: "python" }, { name: "rust" }]).returning();
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
  const tags = await Tag.objects.insertMany([{ name: "a" }, { name: "b" }]).returning();
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

// -- model metadata for factories -------------------------------------------------------------

test("describe gives fields, relations and unique keys", () => {
  const post = describe(Post);
  assert.equal(post.name, "Post");
  assert.equal(post.primaryKey, "id");
  const fields = new Map(post.fields.map((f) => [f.name, f]));
  assert.deepEqual(fields.get("title"), {
    name: "title", column: "title", type: "string", nullable: false, array: false, enum: null, maxLength: 200,
    primaryKey: false, unique: false, default: null, insert: true,
  });
  assert.equal(fields.get("authorId")!.column, "author_id");
  assert.equal(fields.get("id")!.default, "database");
  const relations = new Map(post.relations.map((r) => [r.name, r]));
  assert.equal(relations.get("author")!.kind, "belongsTo");
  assert.equal(relations.get("author")!.target, User);
  assert.equal(relations.get("author")!.from, "authorId");
  assert.equal(relations.get("tags")!.through, PostTag);
  assert.deepEqual(describe(PostTag).unique, [["id"], ["postId", "tagId"]]);
  assert.deepEqual(describe(User).unique, [["id"], ["email"]]);
  const profile = new Map(describe(Profile).fields.map((f) => [f.name, f]));
  assert.equal(profile.get("role")!.enum, "Role");
  assert.ok(profile.get("links")!.array);
});

let made = 0;
/** A factory built only on describe() and objects.insert(). */
async function make(model: ModelClass<ModelSpec>, values: Record<string, unknown> = {}): Promise<Record<string, unknown>> {
  const info = describe(model);
  made += 1;
  for (const rel of info.relations) {
    if (rel.kind === "belongsTo" && !rel.nullable && !(rel.name in values) && !(rel.from in values)) {
      values[rel.name] = await make(rel.target);
    }
  }
  const linked = new Set(info.relations.filter((r) => r.name in values).map((r) => r.from));
  for (const f of info.fields) {
    if (f.insert && f.default === null && !f.nullable && !(f.name in values) && !linked.has(f.name)) {
      values[f.name] = f.type === "string" || f.type === "text" ? `${f.name}-${made}` : f.type === "bool" ? false : made;
    }
  }
  return (await model.objects.insert(values as never)) as Record<string, unknown>;
}

test("a factory needs only describe and insert", async () => {
  const post = await make(Post);
  assert.match(post["title"] as string, /^title-/);
  assert.equal(post["views"], 0);
  const author = await User.objects.get(User.id.eq(post["authorId"] as bigint));
  assert.match(author.email, /^email-/);
  const link = await make(PostTag, { post });
  assert.equal(link["postId"], post["id"]);
});

// -- only() through to-one paths --------------------------------------------------------------

test("only() through a to-one path", async () => {
  const { posts: rows } = await seed();
  const qs = Comment.objects.only(Comment.body, Comment.post.title);
  assert.ok(qs.sql().includes('"title"') && !qs.sql().includes('"views"'));
  await Comment.objects.insert({ post: rows[1]!, body: "x" });
  const [c] = (await qs) as unknown as { body: string; post: { title: string; views: number } }[];
  assert.equal(c!.body, "x");
  assert.equal(c!.post.title, "hit");
  assert.throws(() => c!.post.views, NotLoaded);
  const [d] = (await Comment.objects.only(Comment.post.author.name)) as unknown as { body: string; post: { author: { name: string } } }[];
  assert.equal(d!.post.author.name, "Alice");
  assert.throws(() => d!.body, NotLoaded);
  assert.throws(() => User.objects.only(User.posts.title), /to-many relation User.posts/);
});

// -- or(), Prefetch one, column() ---------------------------------------------------------------

test("or() of query sets", async () => {
  await seed();
  const popular = Post.objects.orderBy("id").filter(Post.views.gte(80));
  const drafts = Post.objects.filter(Post.published.eq(false));
  assert.deepEqual((await popular.or(drafts)).map((p) => p.title), ["draft", "hit"]);
  assert.equal((await popular.or(Post.objects)).length, 4);
  assert.throws(() => popular.filter(Post.views.lt(100)).or(drafts), /one filter/);
  assert.throws(() => drafts.or(popular), /sets order/);
  assert.throws(() => popular.slice(0, 1).or(drafts), QueryError);
});

test("Prefetch one stores a row or null", async () => {
  await seed();
  await User.objects.insert({ email: "carol@example.com", name: "Carol" });
  const users = await User.objects.orderBy("id").prefetchRelated(new Prefetch(User.posts, Post.objects.orderBy("-views"), { toAttr: "best", one: true }));
  assert.deepEqual(users.map((u) => u.best?.title ?? null), ["hit", "bob", null]);
  assert.throws(() => new Prefetch(User.posts, Post.objects.slice(0, 2), { toAttr: "x", one: true }), /slice/);
});

test("column() from a dotted path", async () => {
  await seed();
  assert.equal(String(column(Comment, "post.author.name")), String(Comment.post.author.name));
  const names = await User.objects.filter((column(User, "posts.title") as typeof User.posts.title).startsWith("hi")).select({ name: User.name }).scalars();
  assert.deepEqual(names, ["Alice"]);
  assert.throws(() => column(Post, "nope.title"), /Post has no relation "nope"/);
  assert.throws(() => column(Post, "author.nope"), /User has no field "nope"/);
});
