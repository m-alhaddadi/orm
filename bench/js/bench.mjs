// Phase 0 JS benchmark: Drizzle ORM (node-postgres) vs ormcore napi addon.
//
//   drizzle-pg      Drizzle ORM 0.45 on `pg`, plain JS objects
//   ormcore-async   napi-rs addon -> SeaORM, Promise API
//   ormcore-sync    napi-rs addon -> SeaORM, blocking API
//
// Same ops, sizes, iteration counts and field-touching rules as bench/run_bench.py.
// Runs under Node or Bun:  node bench.mjs [--transport unix|tcp] [--quick]
//                          bun  bench.mjs [--transport unix|tcp] [--quick]

import { createRequire } from "node:module";
import { writeFileSync } from "node:fs";
import os from "node:os";
import pg from "pg";
import { asc, eq } from "drizzle-orm";
import { drizzle } from "drizzle-orm/node-postgres";
import { bigint, bigserial, boolean, integer, pgTable, text, timestamp, varchar } from "drizzle-orm/pg-core";

const require = createRequire(import.meta.url);
const ormcore = require("./ormcore.node");

const args = process.argv.slice(2);
const flag = (name, dflt) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : dflt;
};
const transport = flag("--transport", "unix");
const quick = args.includes("--quick");
const runtime = typeof Bun !== "undefined" ? "bun" : "node";
const out = flag("--out", new URL(`../results-${runtime}-${transport}.json`, import.meta.url).pathname);

const PG = {
  user: process.env.PGUSER ?? "postgres",
  password: process.env.PGPASSWORD ?? "postgres",
  database: process.env.PGDATABASE ?? "ormbench",
  port: Number(process.env.PGPORT ?? 5432),
  host: transport === "tcp" ? (process.env.PG_TCP_HOST ?? "localhost") : (process.env.PG_SOCKET_DIR ?? "/var/run/postgresql"),
};
const sqlxUrl =
  transport === "tcp"
    ? `postgres://${PG.user}:${PG.password}@${PG.host}:${PG.port}/${PG.database}`
    : `postgres://${PG.user}:${PG.password}@${encodeURIComponent(PG.host)}:${PG.port}/${PG.database}`;

const SEED_MAX_ID = 1000;
const SIZES = [1, 50, 1000];
const OPS = ["read", "read_join", "write_bulk", "write_loop"];
const ITERS = {
  read: { 1: 1000, 50: 300, 1000: 50 },
  read_join: { 1: 1000, 50: 300, 1000: 50 },
  write_bulk: { 1: 500, 50: 200, 1000: 30 },
  write_loop: { 1: 500, 50: 20, 1000: 3 },
};
const iterations = (op, n) => (quick ? Math.max(3, Math.floor(ITERS[op][n] / 10)) : ITERS[op][n]);
const warmupFor = (iters) => Math.max(3, Math.floor(iters / 10));

// --- Drizzle schema (same tables Django created) ----------------------------------

const authors = pgTable("blog_author", {
  id: bigserial("id", { mode: "number" }).primaryKey(),
  name: varchar("name", { length: 100 }).notNull(),
  email: varchar("email", { length: 254 }).notNull(),
  createdAt: timestamp("created_at", { withTimezone: true }).notNull(),
});

const posts = pgTable("blog_post", {
  id: bigserial("id", { mode: "number" }).primaryKey(),
  authorId: bigint("author_id", { mode: "number" }).notNull(),
  title: varchar("title", { length: 200 }).notNull(),
  body: text("body").notNull(),
  views: integer("views").notNull(),
  published: boolean("published").notNull(),
  createdAt: timestamp("created_at", { withTimezone: true }).notNull(),
});

// --- helpers ----------------------------------------------------------------------

function makeRows(n) {
  const now = new Date();
  const body = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(4);
  return Array.from({ length: n }, (_, i) => ({
    authorId: (i % 50) + 1,
    title: `Bench post ${i}`,
    body,
    views: i,
    published: i % 2 === 0,
    createdAt: now,
  }));
}

function touchPost(p) {
  return p.id + p.authorId + p.views + p.title.length + p.body.length + (p.published ? 1 : 0) + p.createdAt.getTime();
}
function touchAuthor(a) {
  return a.id + a.name.length + a.email.length + a.createdAt.getTime();
}
function touch(rows, join = false) {
  let s = 0;
  for (const p of rows) {
    s += touchPost(p);
    if (join) s += touchAuthor(p.author);
  }
  return s;
}
function touchDrizzleJoin(rows) {
  let s = 0;
  for (const r of rows) s += touchPost(r.blog_post) + touchAuthor(r.blog_author);
  return s;
}

const summarize = (samplesNs, n) => {
  const s = [...samplesNs].sort((a, b) => a - b);
  const mid = s.length >> 1;
  const median = s.length % 2 ? s[mid] : (s[mid - 1] + s[mid]) / 2;
  const mean = s.reduce((a, b) => a + b, 0) / s.length;
  return {
    n,
    iters: s.length,
    median_us: median / 1e3,
    p95_us: s[Math.min(s.length - 1, Math.floor(s.length * 0.95))] / 1e3,
    mean_us: mean / 1e3,
    us_per_row: median / 1e3 / n,
  };
};

const now = () => process.hrtime.bigint();

async function timeIt(fn, op, n, cleanup) {
  const iters = iterations(op, n);
  const warm = warmupFor(iters);
  const samples = [];
  for (let i = 0; i < warm + iters; i++) {
    const rows = op.startsWith("write") ? makeRows(n) : null;
    const t0 = now();
    await fn(n, rows);
    const dt = Number(now() - t0);
    if (op.startsWith("write")) await cleanup();
    if (i >= warm) samples.push(dt);
  }
  return samples;
}

function timeItSync(fn, op, n, cleanup) {
  const iters = iterations(op, n);
  const warm = warmupFor(iters);
  const samples = [];
  for (let i = 0; i < warm + iters; i++) {
    const rows = op.startsWith("write") ? makeRows(n) : null;
    const t0 = now();
    fn(n, rows);
    const dt = Number(now() - t0);
    if (op.startsWith("write")) cleanup();
    if (i >= warm) samples.push(dt);
  }
  return samples;
}

// --- main -------------------------------------------------------------------------

const pool = new pg.Pool({ ...PG, max: 1 });
const db = drizzle(pool);
const client = await ormcore.connect(sqlxUrl, 1);
const admin = await ormcore.connect(sqlxUrl, 1);

const contenders = {
  "drizzle-pg": {
    read: async (n) => touch(await db.select().from(posts).orderBy(asc(posts.id)).limit(n)),
    read_join: async (n) =>
      touchDrizzleJoin(
        await db.select().from(posts).leftJoin(authors, eq(posts.authorId, authors.id)).orderBy(asc(posts.id)).limit(n),
      ),
    write_bulk: async (n, rows) => {
      await db.insert(posts).values(rows).returning({ id: posts.id });
    },
    write_loop: async (n, rows) => {
      for (const r of rows) await db.insert(posts).values(r).returning({ id: posts.id });
    },
  },
  "ormcore-async": {
    read: async (n) => touch(await client.fetchPosts(n)),
    read_join: async (n) => touch(await client.fetchPostsWithAuthor(n), true),
    write_bulk: async (n, rows) => {
      await client.insertPosts(rows);
    },
    write_loop: async (n, rows) => {
      for (const r of rows) await client.insertPost(r);
    },
  },
  "ormcore-sync": {
    sync: true,
    read: (n) => touch(client.fetchPostsSync(n)),
    read_join: (n) => touch(client.fetchPostsWithAuthorSync(n), true),
    write_bulk: (n, rows) => {
      client.insertPostsSync(rows);
    },
    write_loop: (n, rows) => {
      for (const r of rows) client.insertPostSync(r);
    },
  },
};

const only = flag("--only", null)?.split(",");
const results = [];
for (const [name, impl] of Object.entries(contenders)) {
  if (only && !only.includes(name)) continue;
  for (const op of OPS) {
    await admin.deletePostsAbove(SEED_MAX_ID);
    await pool.query("VACUUM ANALYZE blog_post");
    for (const n of SIZES) {
      const samples = impl.sync
        ? timeItSync(impl[op], op, n, () => admin.deletePostsAboveSync(SEED_MAX_ID))
        : await timeIt(impl[op], op, n, () => admin.deletePostsAbove(SEED_MAX_ID));
      const r = { contender: name, op, ...summarize(samples, n) };
      results.push(r);
      console.log(
        `${name.padEnd(14)} ${op.padEnd(11)} n=${String(n).padEnd(5)} median=${r.median_us.toFixed(1).padStart(10)}us ` +
          `p95=${r.p95_us.toFixed(1).padStart(10)}us  ${r.us_per_row.toFixed(2).padStart(8)}us/row`,
      );
    }
  }
}

// Bridge cost: one call with no DB work.
async function bridge() {
  const N = 20000;
  const measure = async (fn) => {
    for (let i = 0; i < 1000; i++) await fn();
    const s = [];
    for (let i = 0; i < N; i++) {
      const t0 = now();
      await fn();
      s.push(Number(now() - t0));
    }
    return summarize(s, 1).median_us;
  };
  return {
    "plain async function": await measure(async () => {}),
    "napi async fn (Tokio -> Promise)": await measure(() => client.noop()),
  };
}
const bridgeCost = await bridge();
console.log(bridgeCost);

const pgVersion = (await pool.query("SHOW server_version")).rows[0].server_version;
await admin.deletePostsAbove(SEED_MAX_ID);
await pool.end();

const env = {
  runtime: runtime === "bun" ? `bun ${Bun.version}` : `node ${process.versions.node}`,
  drizzle: "0.45",
  postgres: pgVersion,
  cpus: os.cpus().length,
  platform: `${os.platform()} ${os.release()}`,
  transport,
  baseline: "drizzle-pg",
  bridge_us: bridgeCost,
};
writeFileSync(out, JSON.stringify({ env, results }, null, 2));
console.log(`\nwrote ${out}`);
process.exit(0);
