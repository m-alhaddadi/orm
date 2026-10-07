/** SQL shape tests: no database needed. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { Func, QueryError, and, exists, excluded, func, or, outer } from "../src/index.js";
import { Comment, Post, Tag, User } from "./blog/models.js";

const Y = new Date("2026-10-01T00:00:00Z");
const USER_COLS = 'SELECT "users"."id", "users"."email", "users"."name", "users"."created_at" FROM "users"';

function where(qs: { sql(): string }): string {
  const sql = qs.sql();
  const i = sql.indexOf(" WHERE ");
  return i < 0 ? "" : sql.slice(i + 7);
}

const count = (s: string, part: string) => s.split(part).length - 1;
const native = () => Post._meta.registry.native();

test("a to-many filter is EXISTS", () => {
  assert.equal(
    User.objects.filter(User.posts.createdAt.lt(Y)).sql(),
    `${USER_COLS} WHERE EXISTS(SELECT 1 FROM "posts" AS "t1" ` +
      `WHERE "t1"."author_id" = "users"."id" AND "t1"."created_at" < '2026-10-01 00:00:00.000000 +00:00')`,
  );
});

test("one filter() call means the same related row; separate calls are independent", () => {
  const same = where(User.objects.filter(User.posts.views.gt(10), User.posts.published.eq(true)));
  assert.equal(count(same, "EXISTS"), 1);
  assert.ok(same.includes('"t1"."views" > 10 AND "t1"."published" = TRUE'));
  assert.equal(count(where(User.objects.filter(User.posts.views.gt(10)).filter(User.posts.published.eq(true))), "EXISTS"), 2);
});

test("or inside one relation shares the subquery; with a local column it keeps rows", () => {
  const w = where(User.objects.filter(User.posts.views.gt(10).or(User.posts.title.eq("x"))));
  assert.equal(count(w, "EXISTS"), 1);
  assert.ok(w.includes(" OR "));
  assert.ok(where(User.objects.filter(or(User.email.eq("a"), User.posts.views.gt(10)))).startsWith('"users"."email" = \'a\' OR EXISTS('));
});

test("exclude is NOT EXISTS; negation stays outside the subquery", () => {
  assert.ok(where(User.objects.exclude(User.posts.published.eq(false))).startsWith("NOT EXISTS("));
  const w = where(User.objects.filter(and(User.posts.views.gt(10), User.posts.published.eq(true).not())));
  assert.equal(count(w, "EXISTS"), 2);
  assert.ok(w.includes("NOT EXISTS"));
});

test("nested relations nest subqueries; LIKE patterns are escaped", () => {
  const w = where(User.objects.filter(User.posts.comments.body.contains("50%")));
  assert.equal(count(w, "EXISTS"), 2);
  assert.ok(w.includes('"t2"."post_id" = "t1"."id"'));
  assert.ok(w.includes("LIKE E'%50\\\\%%'"));
});

test("to-one filters and columns compared across a relation", () => {
  assert.equal(
    where(Post.objects.filter(Post.author.email.eq("a@b.c"))),
    'EXISTS(SELECT 1 FROM "users" AS "t1" WHERE "t1"."id" = "posts"."author_id" AND "t1"."email" = \'a@b.c\')',
  );
  assert.ok(where(Post.objects.filter(Post.comments.createdAt.lt(Post.createdAt))).includes('"t1"."created_at" < "posts"."created_at"'));
});

test("selectRelated and orderBy use LEFT JOINs", () => {
  const sql = Comment.objects.selectRelated(Comment.post.author).orderBy(Comment.post.title.desc()).sql();
  assert.ok(sql.includes('LEFT JOIN "posts" AS "j1" ON "j1"."id" = "comments"."post_id"'));
  assert.ok(sql.includes('LEFT JOIN "users" AS "j2" ON "j2"."id" = "j1"."author_id"'));
  assert.ok(sql.endsWith('ORDER BY "j1"."title" DESC'));
});

test("slicing", () => {
  assert.ok(Post.objects.slice(5, 10).sql().endsWith("LIMIT 5 OFFSET 5"));
  assert.ok(Post.objects.slice(5, 10).slice(1, 3).sql().endsWith("LIMIT 2 OFFSET 6"));
  assert.throws(() => Post.objects.slice(-1), RangeError);
});

test("null comparisons, in() and empty in()", () => {
  assert.equal(where(Comment.objects.filter(Comment.authorId.eq(null))), '"comments"."author_id" IS NULL');
  assert.equal(where(Comment.objects.filter(Comment.authorId.ne(null))), '"comments"."author_id" IS NOT NULL');
  assert.equal(where(User.objects.filter(User.id.in([1, 2]))), '"users"."id" IN (1, 2)');
  assert.equal(where(User.objects.filter(User.id.in([]))), "FALSE");
});

test("rejected queries", () => {
  assert.throws(() => User.objects.orderBy(User.posts.views as never).sql(), (e: unknown) => e instanceof QueryError && /to-one/.test(e.message));
  assert.throws(() => Post.objects.filter(User.email.eq("x") as never).sql(), /belongs to User/);
});

test("locks", () => {
  assert.equal(User.objects.lock().sql(), `${USER_COLS} FOR UPDATE OF "users"`);
  assert.ok(User.objects.lock({ exclusive: false }).sql().endsWith('FOR SHARE OF "users"'));
  assert.ok(User.objects.lock({ nowait: true }).sql().endsWith('FOR UPDATE OF "users" NOWAIT'));
  assert.ok(User.objects.lock({ exclusive: false, skipLocked: true }).sql().endsWith('FOR SHARE OF "users" SKIP LOCKED'));
  // Only the model's rows: rows joined by selectRelated stay unlocked.
  assert.ok(Post.objects.selectRelated(Post.author).filter(Post.views.gt(1)).slice(0, 5).lock().sql().endsWith('LIMIT 5 FOR UPDATE OF "posts"'));
  assert.throws(() => User.objects.lock({ nowait: true, skipLocked: true } as never), TypeError);
});

test("writes validate before any SQL", async () => {
  await assert.rejects(User.objects.lock().update({ name: "x" }), QueryError);
  await assert.rejects(User.objects.lock().delete(), QueryError);
  await assert.rejects(User.objects.update({ nope: 1 } as never), TypeError);
  await assert.rejects(User.objects.slice(0, 3).delete(), QueryError);
  assert.throws(() => excluded(User.posts.views), TypeError);
  await assert.rejects(User.objects.insert({ email: "a", name: "b" }, { onConflict: User.email, doUpdate: [User.name], set: { name: "x" } }), /twice/);
  await assert.rejects(User.objects.insert({ email: "a", name: "b" }, { onConflict: [] }), /onConflict needs/);
});

test("updateMany joins a VALUES list", () => {
  const [sql] = native().updateManySql("Post", ["id", "title", "views"], [[1, "a", 10], [2, "b", 20]], "[]", [], null, []);
  assert.equal(
    sql,
    'UPDATE "posts" SET "title" = "v"."column2", "views" = "v"."column3" ' +
      'FROM (VALUES (1, \'a\', 10), (2, \'b\', 20)) AS "v" WHERE "posts"."id" = "v"."column1"',
  );
});

test("updateMany falls back to CASE", () => {
  const [sql] = native().updateManySql("Post", ["id", "views"], [[1, 10], [2, 20]], "[]", [], null, ["update_from_values"]);
  assert.equal(
    sql,
    'UPDATE "posts" SET "views" = (CASE WHEN ("posts"."id" = 1) THEN 10 WHEN ("posts"."id" = 2) THEN 20 END) WHERE "posts"."id" IN (1, 2)',
  );
});

test("updateMany batches", () => {
  const rows = Array.from({ length: 5 }, (_, i) => [i, "x"]);
  assert.equal(native().updateManySql("Post", ["id", "title"], rows, "[]", [], 2, []).length, 3);
  // Without batchSize, as many rows as Postgres' 65535 parameters allow.
  const many = Array.from({ length: 40_000 }, (_, i) => [i, i]);
  assert.equal(native().updateManySql("Post", ["id", "views"], many, "[]", [], null, []).length, 2);
});

test("updateMany validation", async () => {
  await assert.rejects(Post.objects.updateMany([{ title: "a" } as never]), /has no id/);
  await assert.rejects(Post.objects.updateMany([{ id: 1, title: "a" }, { id: 1, title: "b" }]), /appears twice/);
  await assert.rejects(Post.objects.updateMany([{ id: 1, title: "a" }, { id: 2, views: 3 }]), /same fields/);
  await assert.rejects(Post.objects.updateMany([{ id: 1 }]), /besides id/);
  await assert.rejects(Post.objects.updateMany([{ id: 1, views: Post.views.add(1) as never }]), /plain values/);
  await assert.rejects(Post.objects.updateMany([{ id: 1, nope: 1 } as never]), /no field/);
  await assert.rejects(Post.objects.slice(0, 2).updateMany([{ id: 1, views: 1 }]), QueryError);
  await assert.rejects(Post.objects.updateMany([{ id: 1, views: 1 }], { batchSize: 0 }), RangeError);
});

test("select with groupBy and having", () => {
  const sql = Post.objects
    .filter(Post.published)
    .select({ authorId: Post.authorId, n: func.count(), total: func.sum(Post.views) })
    .groupBy(Post.authorId)
    .having(func.count().gt(2))
    .sql();
  assert.equal(
    sql,
    'SELECT "posts"."author_id", COUNT(*), CAST(SUM("posts"."views") AS BIGINT) FROM "posts" ' +
      'WHERE "posts"."published" = TRUE GROUP BY "posts"."author_id" HAVING (COUNT(*)) > 2',
  );
});

test("an aggregate over a relation is a correlated subquery", () => {
  assert.equal(
    User.objects.select({ id: User.id, posts: func.count(User.posts), top: func.max(User.posts.views) }).sql(),
    'SELECT "users"."id", ' +
      '(SELECT COUNT(*) FROM "posts" AS "a1" WHERE "a1"."author_id" = "users"."id"), ' +
      '(SELECT MAX("a2"."views") FROM "posts" AS "a2" WHERE "a2"."author_id" = "users"."id") FROM "users"',
  );
  assert.equal(
    where(User.objects.filter(func.count(User.posts.comments).gt(3))),
    '(SELECT COUNT(*) FROM "posts" AS "a1" INNER JOIN "comments" AS "a2" ON "a2"."post_id" = "a1"."id" ' +
      'WHERE "a1"."author_id" = "users"."id") > 3',
  );
});

test("selected to-one columns join; to-many ones are rejected", () => {
  assert.equal(
    Post.objects.select({ title: Post.title, author: Post.author.name }).sql(),
    'SELECT "posts"."title", "j1"."name" FROM "posts" LEFT JOIN "users" AS "j1" ON "j1"."id" = "posts"."author_id"',
  );
  assert.throws(() => User.objects.select({ t: User.posts.title as never }).sql(), /aggregate it/);
});

test("subqueries and distinct", () => {
  const inner = User.objects.filter(User.name.eq("A")).select({ id: User.id });
  assert.equal(where(Post.objects.filter(Post.authorId.in(inner))), '"posts"."author_id" IN (SELECT "users"."id" FROM "users" WHERE "users"."name" = \'A\')');
  assert.ok(where(Post.objects.filter(Post.authorId.notIn(inner))).includes("NOT IN (SELECT"));
  assert.throws(() => Post.objects.filter(Post.authorId.in(User.objects.select({ id: User.id, n: User.name }) as never)).sql(), /exactly one column/);
  assert.ok(Post.objects.select({ a: Post.authorId }).distinct().sql().startsWith('SELECT DISTINCT "posts"."author_id"'));
  const sql = Post.objects.select({ t: Post.title }).distinct(Post.authorId).orderBy(Post.authorId, Post.views.desc()).sql();
  assert.ok(sql.startsWith('SELECT DISTINCT ON ("posts"."author_id") "posts"."title" FROM "posts"'));
});

test("select validation", () => {
  assert.throws(() => Post.objects.select({ u: User as never }), /takes Post itself/);
  assert.throws(() => Post.objects.select({ x: 1 as never }), TypeError);
  assert.throws(() => Post.objects.select({}), /at least one column/);
  assert.throws(() => Post.objects.selectRelated(Post.author).select({ id: Post.id }), QueryError);
  assert.throws(() => Post.objects.select({ x: new Func("nope", [Post.id]) }).sql(), /unknown function/);
  assert.throws(() => Post.objects.lock().select({ n: func.count() }).sql(), /lock/);
});

// -- subqueries, windows, CTEs ----------------------------------------------------------------------

test("exists() with outer() correlates", () => {
  assert.equal(
    where(User.objects.filter(exists(Post.objects.filter(Post.authorId.eq(outer(User.id)))))),
    'EXISTS(SELECT 1 FROM "posts" WHERE "posts"."author_id" = "users"."id")',
  );
});

test("a subquery over the same table gets an alias", () => {
  const avg = Post.objects.filter(Post.authorId.eq(outer(Post.authorId))).select({ a: func.avg(Post.views) }).asScalar();
  assert.ok(where(Post.objects.filter(Post.views.gt(avg))).includes('FROM "posts" AS "s1" WHERE "s1"."author_id" = "posts"."author_id"'));
});

test("window function SQL", () => {
  const sql = Post.objects.select({ s: func.sum(Post.views).over({ partitionBy: Post.authorId, orderBy: Post.createdAt.desc(), rows: [-2, 0] }) }).sql();
  assert.ok(
    sql.startsWith(
      'SELECT CAST(SUM("posts"."views") OVER (PARTITION BY "posts"."author_id" ' +
        'ORDER BY "posts"."created_at" DESC ROWS BETWEEN 2 PRECEDING AND CURRENT ROW) AS BIGINT)',
    ),
  );
  assert.ok(Post.objects.select({ q: func.ntile(4).over() }).sql().includes("NTILE(4) OVER ()"));
});

test("CTE SQL", () => {
  const totals = Post.objects.select({ authorId: Post.authorId, n: func.count() }).groupBy(Post.authorId).cte("totals");
  const sql = User.objects.filter(User.id.in(totals.select({ a: totals.c.authorId }).filter(totals.c.n.gt(2)))).sql();
  assert.ok(
    sql.startsWith(
      'WITH "totals" ("authorId", "n") AS (SELECT "posts"."author_id" AS "authorId", COUNT(*) AS "n" ' +
        'FROM "posts" GROUP BY "posts"."author_id") SELECT',
    ),
    sql,
  );
  assert.ok(sql.endsWith('IN (SELECT "totals"."authorId" FROM "totals" WHERE "totals"."n" > 2)'), sql);
});

test("a recursive CTE joins itself", () => {
  const chain = User.objects.filter(User.id.eq(1)).cte("chain", { recursive: (c) => User.objects.filter(User.id.eq(c.c.id.add(1))) });
  const sql = User.objects.from(chain).sql();
  assert.ok(sql.startsWith('WITH RECURSIVE "chain"'));
  assert.ok(sql.includes('FROM "users", "chain" WHERE "users"."id" = "chain"."id" + 1'));
  assert.ok(sql.endsWith('FROM "chain"'));
});

test("a many-to-many filter is one EXISTS through the join table", () => {
  assert.equal(
    where(Post.objects.filter(Post.tags.name.eq("x"))),
    'EXISTS(SELECT 1 FROM "tags" AS "t1" INNER JOIN "post_tags" AS "t2" ON "t2"."tag_id" = "t1"."id" ' +
      'WHERE "t2"."post_id" = "posts"."id" AND "t1"."name" = \'x\')',
  );
  assert.ok(
    Post.objects.select({ n: func.count(Post.tags) }).sql().includes(
      '(SELECT COUNT(*) FROM "tags" AS "a1" INNER JOIN "post_tags" AS "a2" ON "a2"."tag_id" = "a1"."id" WHERE "a2"."post_id" = "posts"."id")',
    ),
  );
  assert.equal(count(Tag.objects.filter(Tag.posts.author.name.eq("A")).sql(), "EXISTS"), 2);
});

test("has-one joins on the other side", () => {
  assert.ok(User.objects.selectRelated(User.profile).sql().includes('LEFT JOIN "profiles" AS "j1" ON "j1"."user_id" = "users"."id"'));
});

test("orderBy takes field names, with - for descending", () => {
  assert.equal(Post.objects.orderBy("-createdAt", "id").sql(), Post.objects.orderBy(Post.createdAt.desc(), Post.id.asc()).sql());
  assert.ok(Post.objects.select({ t: Post.title }).orderBy("-views").sql().endsWith('ORDER BY "posts"."views" DESC'));
  assert.throws(() => Post.objects.orderBy("-nope" as never), /Post has no field "nope"/);
});
