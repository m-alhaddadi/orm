/**
 * End-to-end tour of the TypeScript API, the counterpart of demo.py. Build the `orm`
 * package first (`cd js && npm install && npm run build:native && npm run build`), then
 * from this directory:
 *
 *     npm install
 *     npm run demo -- postgres://postgres:postgres@localhost/orm_test    (or: bun demo.ts ...)
 *
 * Drops and recreates the blog tables in that database.
 */

import { connect, exists, func, outer, param } from "orm";

import { Comment, Post, User } from "./models.js";

const DAY = 86_400_000;

async function main(url: string): Promise<void> {
  const db = await connect(url);
  await db.dropTables();
  await db.createTables();

  const now = new Date();
  const yesterday = new Date(now.getTime() - DAY);

  // Writes are explicit statements: INSERT ... RETURNING gives back the stored rows.
  const alice = await User.objects.insert({ email: "alice@example.com", name: "Alice" });
  let bob = await User.objects.insert({ email: "bob@example.com", name: "Bob" });
  const [old, fresh] = await Post.objects.insertMany([
    { author: alice, title: "Hello", body: "...", createdAt: new Date(now.getTime() - 3 * DAY) },
    { author: bob, title: "Fresh", body: "...", published: true },
  ]);
  await fresh!.comments.insert({ body: "first!", author: alice });
  // Upsert on the unique email.
  bob = await User.objects.insert({ email: "bob@example.com", name: "Robert" }, { onConflict: User.email, doUpdate: true });
  console.log("upserted:", bob);

  // Query sets are lazy: nothing runs until a terminal method (all, first, get, count, ...).
  // Filters follow relations; to-many hops compile to EXISTS (no duplicate rows).
  const q = User.objects.filter(User.posts.createdAt.lt(yesterday));
  console.log(q.sql());
  console.log("posted before yesterday:", (await q.all()).map((u) => u.name));

  // Conditions in one filter() call must match the same post.
  console.log(
    "same post old & published:",
    await User.objects.filter(User.posts.createdAt.lt(yesterday), User.posts.published.eq(true)).count(),
  );

  // Eager loading: JOIN for to-one, one extra IN query for to-many. The row types say
  // what was loaded: c.post.author is a User here, c.author a User or null.
  for (const c of await Comment.objects.selectRelated(Comment.post.author, Comment.author).all()) {
    console.log(`${c.author?.name ?? "?"} on ${c.post.author.name}'s ${JSON.stringify(c.post.title)}: ${c.body}`);
  }
  for (const u of await User.objects.prefetchRelated(User.posts).orderBy(User.name).all()) {
    console.log(u.name, u.posts.cached.map((p) => p.title));
  }

  // select(): typed rows of columns, aggregates and correlated subqueries.
  const stats = await User.objects
    .select({
      name: User.name,
      posts: func.count(User.posts),
      popular: exists(Post.objects.filter(Post.authorId.eq(outer(User.id)), Post.published)),
    })
    .orderBy(User.name)
    .all();
  console.log(stats); // { name: string; posts: bigint; popular: boolean }[]

  // Prepared queries: param() placeholders, values checked against their columns.
  const byAuthor = Post.objects.filter(Post.authorId.eq(param("author"))).orderBy(Post.id).prepare();
  console.log((await byAuthor.all({ author: alice.id })).map((p) => p.title));

  // Set-based UPDATE, and a single-row update that refreshes the instance.
  await Post.objects.filter(Post.authorId.eq(alice.id)).update({ views: Post.views.add(1) });
  await db.transaction(() => old!.update({ title: "Hello, world", views: Post.views.add(10) }));
  console.log(old); // views=11: the database's value, read back with RETURNING

  await db.dropTables();
  await db.close();
}

await main(process.argv[2] ?? "postgres://postgres:postgres@localhost/orm_test");
