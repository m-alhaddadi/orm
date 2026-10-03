/** Shared setup of the end-to-end tests: Postgres at ORM_TEST_DATABASE_URL. */

import { after, afterEach, before } from "node:test";

import { connect, getDatabase, type Database } from "../src/index.js";
import { Comment, Post, User } from "./blog/models.js";

export const DATABASE_URL = process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";

export const NOW = new Date();
export const YESTERDAY = new Date(NOW.getTime() - 86_400_000);
export const LAST_WEEK = new Date(NOW.getTime() - 7 * 86_400_000);

/** Creates the tables before the file's tests, empties them after each one, drops them
 * at the end. */
export function useDatabase(): void {
  before(async () => {
    const db = await connect(DATABASE_URL, { maxConnections: 4 });
    await db.dropTables();
    await db.createTables();
  });
  afterEach(async () => {
    await getDatabase().execute("TRUNCATE post_tags, tags, profiles, comments, posts, users RESTART IDENTITY CASCADE");
  });
  after(async () => {
    const db = getDatabase();
    await db.dropTables();
    await db.close();
  });
}

/** A second pool: queries `.using()` it run outside the default pool's transaction. */
export async function otherDatabase(): Promise<Database> {
  return connect(DATABASE_URL, { maxConnections: 2, default: false });
}

export async function seed() {
  const alice = await User.objects.insert({ email: "alice@example.com", name: "Alice" });
  const bob = await User.objects.insert({ email: "bob@example.com", name: "Bob" });
  const carol = await User.objects.insert({ email: "carol@example.com", name: "Carol" });
  const [a1, a2, b1] = (await Post.objects.insertMany([
    { author: alice, title: "old draft", body: "...", createdAt: LAST_WEEK, views: 5 },
    { author: alice, title: "new post", body: "...", published: true, views: 50 },
    { author: bob, title: "bob's old", body: "...", createdAt: LAST_WEEK, published: true, views: 100 },
  ])) as [Post, Post, Post];
  await Comment.objects.insertMany([
    { post: a2, author: bob, body: "nice, 100% agree" },
    { post: a2, author: null, body: "anonymous" },
    { post: b1, author: alice, body: "hello" },
  ]);
  return { alice, bob, carol, a1, a2, b1 };
}

export function names(users: readonly { readonly name: string }[]): string[] {
  return users.map((u) => u.name).sort();
}

/** Collects an async iterable. */
export async function collect<T>(it: AsyncIterable<T>): Promise<T[]> {
  const out: T[] = [];
  for await (const x of it) {
    out.push(x);
  }
  return out;
}
