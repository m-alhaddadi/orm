/**
 * Static type checks: `npm run typecheck`. Every `@ts-expect-error` line must be a type
 * error (tsc reports a directive with nothing to suppress), and `same<A, B>()` asserts
 * two types are identical. Never run.
 */

import { Decimal, Prefetch, exists, func, outer, param, window, type JsonValue, type Page } from "../../src/index.js";
import { Comment, Post, Priority, Profile, Role, Tag, User } from "../blog/models.js";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
declare function same<A, B>(...check: Equal<A, B> extends true ? [] : [error: "types differ"]): void;
declare const user: User;
declare const post: Post;
declare const profile: Profile;
declare const tag: Tag;

export async function columns() {
  same<typeof user.id, bigint>();
  same<typeof post.createdAt, Date>();
  same<typeof profile.balance, Decimal>();
  same<typeof profile.role, Role>();
  same<typeof profile.links, string[]>();
  same<typeof tag.priority, Priority>();
  same<(typeof Comment)["authorId"], import("../../src/index.js").Column<bigint | null, "Comment">>();
  // @ts-expect-error instances are read-only
  user.name = "x";
  // @ts-expect-error to-one relations exist only on rows of queries that load them
  void post.author;
  // @ts-expect-error prefetched rows exist only after prefetchRelated
  void user.posts.cached;
  const j: JsonValue = { a: [1, "b", null] };
  void j;
}

export async function stringsAndArrays() {
  const rows = await Profile.objects.select({ first: Profile.links.element(1), link: func.unnest(Profile.links) }).all();
  same<(typeof rows)[number], { first: string | null; link: string }>();
  const text = await Post.objects.select({ c: func.concat(Post.title, " ", Post.views), p: Post.title.concat("!"), i: func.strpos(Post.title, "x"), t: func.trim(Post.title) }).all();
  same<(typeof text)[number], { c: string; p: string; i: number; t: string }>();
  const nullable = await Profile.objects.select({ a: Profile.links.element(1).concat("!"), b: func.unnest(Profile.links).concat(Profile.links.element(1)) }).all();
  same<(typeof nullable)[number], { a: string | null; b: string | null }>();
  // @ts-expect-error concatenation takes strings
  Post.views.concat("!");
  // @ts-expect-error element access takes an array column
  Post.title.element(1);
}

export async function caseExpressions() {
  const rows = await Post.objects.select({ h: func.case([Post.views.gt(3), "hot"], { default: "cold" }), t: func.case([Post.published, Post.title]), n: func.case([Post.published, Post.views], [Post.views.gt(1), 2], { default: 0 }) }).all();
  same<(typeof rows)[number], { h: string; t: string | null; n: number }>();
  // @ts-expect-error a branch is a [condition, value] pair
  func.case(Post.published);
}

export async function aggregateFilters() {
  const rows = await Post.objects.select({ c: func.count({ filter: Post.published }), s: func.sum(Post.views, { filter: Post.views.gt(3) }), m: func.max(Post.title, { filter: Post.published }) }).all();
  same<(typeof rows)[number], { c: bigint; s: number | bigint | null; m: string | null }>();
  // @ts-expect-error the filter is a condition
  func.count({ filter: Post.views });
}

export function json(meta: import("../../src/index.js").Column<JsonValue, "Post">) {
  same<ReturnType<typeof meta.get>, import("../../src/index.js").JsonPath<"Post", {}>>();
  same<ReturnType<ReturnType<typeof meta.get>["asText"]>, import("../../src/index.js").Expression<string | null, "Post", {}>>();
  void meta.jsonContains({ a: [1] }).and(meta.hasKey("a"));
  void Post.objects.update({ views: 1 });
  // @ts-expect-error JSON paths need a JSON value
  Post.views.get("a");
}

export async function fullTextSearch() {
  const vector = func.toTsvector("english", Post.title);
  same<typeof vector, import("../../src/index.js").Func<import("../../src/index.js").TsVector, "Post", {}>>();
  const rows = await Post.objects.filter(vector.matches("dog")).select({ r: func.tsRank(vector, func.plaintoTsquery("dog")) }).all();
  same<(typeof rows)[number], { r: number }>();
  // @ts-expect-error matches() needs a tsvector
  Post.title.matches("dog");
}

export async function filters() {
  await User.objects.filter(User.email.eq("a"), User.posts.views.gt(3)).all();
  await Post.objects.filter(Post.published).all();
  await Comment.objects.filter(Comment.authorId.eq(null)).all();
  await Post.objects.filter(Post.id.eq(1), Post.id.eq(1n), Post.id.in([1, 2n])).all();
  await Profile.objects.filter(Profile.balance.gt("1.5"), Profile.balance.lt(new Decimal(2)), Profile.role.eq("admin")).all();
  // @ts-expect-error a column of another model
  Post.objects.filter(User.email.eq("x"));
  // @ts-expect-error a value of the wrong type
  User.email.eq(3);
  // @ts-expect-error null compares only with nullable columns
  User.email.eq(null);
  // @ts-expect-error no ordering with null
  Comment.authorId.lt(null);
  // @ts-expect-error string matching on a number column
  Post.views.contains("1");
  // @ts-expect-error array operators on a scalar column
  Post.title.has("x");
  // @ts-expect-error not a value of the enum
  Profile.role.eq("owner");
  // @ts-expect-error a decimal column takes numbers, text or Decimals, not booleans
  Profile.balance.eq(true);
  // @ts-expect-error a non-boolean expression is not a condition
  Post.objects.filter(Post.views);
  // @ts-expect-error ordering through a to-many relation would repeat rows
  User.objects.orderBy(User.posts.views);
  Post.objects.orderBy("-createdAt", "id", Post.title.desc());
  Post.objects.select({ t: Post.title }).orderBy("-views");
  same<Awaited<ReturnType<typeof Post.objects.paginate>>, Page<Post>>();
  // @ts-expect-error a forward page reads after a cursor, not before one
  Post.objects.paginate({ first: 20, before: "x" });
  // @ts-expect-error orderBy takes the model's own field names
  Post.objects.orderBy("-nope");
  // @ts-expect-error field names are the TypeScript names, not the column names
  Post.objects.orderBy("created_at");
  // @ts-expect-error a CTE column in a query that doesn't read the CTE
  Post.objects.filter(Post.objects.select({ n: func.count() }).cte("t").c.n.gt(1));
  // @ts-expect-error nowait and skipLocked exclude each other
  Post.objects.lock({ nowait: true, skipLocked: true });
}

export async function loading() {
  const cs = await Comment.objects.selectRelated(Comment.post.author, Comment.author).all();
  same<(typeof cs)[number]["post"]["author"]["name"], string>();
  same<NonNullable<(typeof cs)[number]["author"]>["name"], string>();
  const nullable: (typeof cs)[number]["author"] = null;
  void nullable;
  const us = await User.objects.prefetchRelated(User.posts.comments, User.profile).all();
  same<(typeof us)[number]["posts"]["cached"][number]["comments"]["cached"][number]["body"], string>();
  const p: (typeof us)[number]["profile"] = null;
  void p;
  const top = await User.objects.prefetchRelated(new Prefetch(User.posts, Post.objects.selectRelated(Post.author), { toAttr: "top" })).all();
  same<(typeof top)[number]["top"][number]["author"]["email"], string>();
  // @ts-expect-error selectRelated follows to-one relations only
  User.objects.selectRelated(User.posts);
  // @ts-expect-error a path from another model
  User.objects.selectRelated(Post.author);
  // @ts-expect-error a prefetch query set of the wrong model
  new Prefetch(User.posts, Comment.objects);
}

export async function writes() {
  const u = await User.objects.insert({ email: "a", name: "A" });
  same<typeof u, User>();
  const maybe = await User.objects.insert({ email: "a", name: "A" }).onConflict(User.email, { update: false }).returning();
  same<typeof maybe, User | null>();
  const upserted = await User.objects.insert({ email: "a", name: "A" }).onConflict(User.email, { update: true, updateFields: [User.name] }).returning();
  same<typeof upserted, User>();
  const flag = u.name === "A";
  const either = await User.objects.insert({ email: "a", name: "A" }).onConflict(User.email, { update: flag }).returning();
  same<typeof either, User | null>();
  const upsertCount = await User.objects.insert({ email: "a", name: "A" }).onConflict(User.email, { update: true });
  same<typeof upsertCount, number>();
  const inserted = await User.objects.insertMany([{ email: "a", name: "A" }]);
  same<typeof inserted, number>();
  const kept = await User.objects.insertMany([{ email: "a", name: "A" }]).onConflict([User.email], { update: false }).returning();
  same<typeof kept, User[]>();
  // @ts-expect-error update is required
  void User.objects.insert({ email: "a", name: "A" }).onConflict(User.email, {});
  // @ts-expect-error updateValues keys are fields
  void User.objects.insert({ email: "a", name: "A" }).onConflict(User.email, { update: true, updateValues: { nope: 1 } });
  // @ts-expect-error a plain insert gives the instance
  void User.objects.insert({ email: "a", name: "A" }).returning();
  await Post.objects.insert({ author: u, title: "t", body: "b" });
  await Post.objects.insert({ authorId: 1, title: "t", body: "b" });
  await u.posts.insert({ title: "t", body: "b" });
  const n = await Post.objects.update({ views: Post.views.add(1) });
  same<typeof n, number>();
  const rows = await Post.objects.update({ views: 0 }, { returning: true });
  same<typeof rows, Post[]>();
  await post.update({ title: "x", views: Post.views.mul(2) });
  await Post.objects.updateMany([{ id: 1, title: "x" }]);
  await post.tags.add(tag, 3n);
  await post.tags.add(tag, { throughDefaults: { position: 1 } });
  const batched = await User.objects.insertMany([{ email: "a", name: "A" }], { batchSize: 10 }).returning();
  same<typeof batched, User[]>();
  const copied = await User.objects.insertMany([{ email: "a", name: "A" }], { copy: true });
  same<typeof copied, number>();
  const [got, created] = await User.objects.getOrInsert({ email: "a" }, { defaults: { name: "A" } });
  same<typeof got, User>();
  same<typeof created, boolean>();
  await User.objects.insert({ email: "a", name: "A" }).onConflict(User.email, { where: User.name.isNull(), update: false });
  // @ts-expect-error a required field is missing
  User.objects.insert({ email: "a" });
  // @ts-expect-error an unknown field
  User.objects.insert({ email: "a", name: "A", nope: 1 });
  // @ts-expect-error the key or the related row is required
  Post.objects.insert({ title: "t", body: "b" });
  // @ts-expect-error not both
  Post.objects.insert({ author: u, authorId: 1, title: "t", body: "b" });
  // @ts-expect-error the related set fills in the key
  u.posts.insert({ title: "t", body: "b", authorId: 1 });
  // @ts-expect-error an expression over another model
  Post.objects.update({ views: User.id });
  // @ts-expect-error update rows need the primary key
  Post.objects.updateMany([{ title: "x" }]);
  // @ts-expect-error update rows take plain values
  Post.objects.updateMany([{ id: 1, views: Post.views.add(1) }]);
  // @ts-expect-error a post is not a tag
  post.tags.add(post);
}

export async function selects() {
  const rows = await Post.objects
    .select({ id: Post.id, author: Post.author.name, n: func.count(Post.comments), avg: func.avg(Post.views), post: Post })
    .all();
  same<(typeof rows)[number], { id: bigint; author: string; n: bigint; avg: number | null; post: Post }>();
  const via = await Comment.objects.select({ name: Comment.author.name }).all();
  same<(typeof via)[number]["name"], string | null>(); // through a nullable relation
  const top = await Post.objects.select({ m: func.max(Post.views) }).scalar();
  same<typeof top, number | null>();
  const ids = await Post.objects.select({ id: Post.id }).scalars();
  same<typeof ids, bigint[]>();
  const total = await Profile.objects.select({ s: func.sum(Profile.balance) }).scalar();
  same<typeof total, Decimal | null>();
  const w = await Post.objects.select({ r: func.rowNumber().over({ partitionBy: Post.authorId, orderBy: Post.views.desc() }) }).all();
  same<(typeof w)[number]["r"], bigint>();
  const shared = window({ partitionBy: Post.authorId });
  await Post.objects.select({ s: func.sum(Post.views).over(shared) }).all();
  // @ts-expect-error a to-many column outside an aggregate would repeat rows
  User.objects.select({ t: User.posts.title });
  // @ts-expect-error a window function needs over()
  Post.objects.select({ r: func.rowNumber() });
  // @ts-expect-error sum of text
  func.sum(Post.title);
  // @ts-expect-error scalar() of two columns
  Post.objects.select({ a: Post.id, b: Post.title }).scalar();
  // @ts-expect-error another model
  Post.objects.select({ u: User });
}

export async function subqueries() {
  const popular = exists(Post.objects.filter(Post.authorId.eq(outer(User.id)), Post.views.gte(50)));
  await User.objects.filter(popular).all();
  const latest = Post.objects.filter(Post.authorId.eq(outer(User.id))).select({ t: Post.title }).limit(1).asScalar();
  const rows = await User.objects.select({ user: User, latest }).all();
  same<(typeof rows)[number]["latest"], string | null>();
  // two levels down: the nearest User query
  const commented = Comment.objects.filter(Comment.authorId.eq(outer(User.id))).select({ p: Comment.postId });
  await User.objects.filter(exists(Post.objects.filter(Post.id.in(commented)))).all();
  const authorName = Post.objects.filter(Post.author.name.eq(outer(Comment.post.author.name))).select({ t: Post.title }).limit(1).asScalar();
  await Comment.objects.select({ t: authorName }).all();
  // @ts-expect-error outer() through a to-many relation
  outer(User.posts.views);
  // @ts-expect-error an outer() reference with no enclosing query
  await User.objects.filter(User.id.eq(outer(User.id))).all();
  // a Post query reading a User reference runs only inside a User query
  const nested = Post.objects.filter(popular);
  // @ts-expect-error exists() over a User reference in a top-level Post query
  await nested.all();
  // @ts-expect-error in() takes a one-column select
  Post.id.in(User.objects.select({ a: User.id, b: User.name }));
}

export async function awaiting() {
  const posts = await Post.objects.filter(Post.published);
  same<typeof posts, Post[]>();
  const cs = await Comment.objects.selectRelated(Comment.post.author);
  same<(typeof cs)[number]["post"]["author"]["name"], string>();
  const rows = await Post.objects.select({ id: Post.id, n: func.count(Post.comments) });
  same<typeof rows, { id: bigint; n: bigint }[]>();
  const mine = await user.posts.filter(Post.views.gt(1));
  same<typeof mine, Post[]>();
  for await (const p of Post.objects.orderBy(Post.id)) {
    same<typeof p, Post>();
  }
  // returned from an async function, a query set resolves to its rows
  const later = async () => Post.objects.filter(Post.published);
  same<Awaited<ReturnType<typeof later>>, Post[]>();
  // @ts-expect-error a query with param() placeholders runs through prepare()
  await Post.objects.filter(Post.id.eq(param("id")));
  // @ts-expect-error a query reading outer() columns runs only inside another query
  await Post.objects.filter(Post.authorId.eq(outer(User.id)));
  // @ts-expect-error the same for select()
  await Post.objects.filter(Post.authorId.eq(outer(User.id))).select({ id: Post.id });
}

export async function ctes() {
  const totals = Post.objects.select({ authorId: Post.authorId, n: func.count() }).groupBy(Post.authorId).cte("totals");
  const rows = await User.objects.join(totals, totals.c.authorId.eq(User.id)).select({ name: User.name, n: totals.c.n }).all();
  same<(typeof rows)[number], { name: string; n: bigint }>();
  const ranked = Post.objects.select({ post: Post, rank: func.rowNumber().over({ orderBy: Post.views }) }).cte("ranked");
  await Post.objects.from(ranked).filter(ranked.c.rank.lte(3)).all();
  const chain = User.objects.filter(User.id.eq(1)).cte("chain", { recursive: (c) => User.objects.filter(User.id.eq(c.c.id.add(1))) });
  await User.objects.from(chain).all();
  // @ts-expect-error a CTE without the model's columns
  Post.objects.from(totals);
  // @ts-expect-error a CTE of another model
  User.objects.from(ranked);
}

export async function prepared() {
  const q = Post.objects.filter(Post.authorId.eq(param("author")), Post.title.contains(param("t"))).limit(param("n")).prepare();
  const posts = await q.all({ author: 1n, t: "x", n: 10 });
  same<typeof posts, Post[]>();
  const plain = Post.objects.prepare();
  await plain.all();
  // @ts-expect-error a value is missing
  await q.all({ author: 1n, t: "x" });
  // @ts-expect-error a value of the wrong type
  await q.all({ author: "1", t: "x", n: 1 });
  // @ts-expect-error a query with param() placeholders runs through prepare()
  await Post.objects.filter(Post.id.eq(param("id"))).all();
}
