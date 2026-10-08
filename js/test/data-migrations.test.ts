/** Data migrations: a `data.ts` with `export async function run(db)` next to `up.sql`. */

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";

import { MigrationError, Migrations, Migrator, Registry, connect, type Database } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const DATA_DB = new URL(DATABASE_URL).pathname.slice(1) + "_data_js";
const DATA_URL = Object.assign(new URL(DATABASE_URL), { pathname: "/" + DATA_DB }).toString();

const V1 = `
model Author {
  id   BigInt @id @default(autoincrement())
  name String
}
`;
const V2 = V1.replace("name String", 'name String\n  slug String @default("")');

const FILL = `
export async function run(db) {
  // the new column is visible only inside the migration's transaction
  await db.execute("UPDATE author SET slug = lower(name)");
  const rows = await db.fetchText("SELECT count(*) FROM author WHERE slug <> ''");
  if (rows[0][0] !== "2") throw new Error("expected 2 rows, got " + rows[0][0]);
}
`;

before(async () => {
  const admin = await connect(DATABASE_URL, { maxConnections: 1, default: false, registry: new Registry() });
  try {
    if (!(await admin.fetchText(`SELECT 1 FROM pg_database WHERE datname = '${DATA_DB}'`)).length) {
      await admin.execute(`CREATE DATABASE "${DATA_DB}"`);
    }
  } finally {
    await admin.close();
  }
});

const opened: Database[] = [];
after(async () => {
  for (const db of opened) await db.close();
});

test("a data step runs in the migration's transaction", async () => {
  const dir = mkdtempSync(join(tmpdir(), "orm-data-"));
  const schema = join(dir, "schema.prisma");
  const migrations = new Migrations(join(dir, "migrations"), schema);
  writeFileSync(schema, V1);
  migrations.make();
  writeFileSync(schema, V2);
  const second = migrations.make("slug")!;
  writeFileSync(join(second.path, "data.ts"), FILL);

  const db = await connect(DATA_URL, { maxConnections: 2, default: false, registry: new Registry() });
  opened.push(db);
  await db.execute("DROP TABLE IF EXISTS author, extra, orm_migrations CASCADE");
  const migrator = new Migrator(db, migrations);
  assert.deepEqual((await migrator.upgrade("1")).map((m) => m.name), ["0001_initial"]);
  await db.execute("INSERT INTO author (name) VALUES ('Ann'), ('Bob')");
  await assert.rejects(db.engine.migrateUp(migrations.directory, null), /0002_slug has a data step \(data\.ts\)/);
  assert.deepEqual((await migrator.upgrade()).map((m) => m.name), ["0002_slug"]);
  assert.deepEqual(await db.fetchText("SELECT slug FROM author ORDER BY id"), [["ann"], ["bob"]]);

  // a failing data step rolls back its SQL and is not recorded
  writeFileSync(schema, V2 + "\nmodel Extra {\n  id BigInt @id\n}\n");
  const third = migrations.make("extra")!;
  writeFileSync(join(third.path, "data.ts"), 'export async function run(db) { throw new Error("data step failed"); }\n');
  await assert.rejects(migrator.upgrade(), /data step failed/);
  assert.deepEqual(await db.fetchText("SELECT to_regclass('extra')"), [[null]]);
  assert.deepEqual((await migrator.status()).map((s) => s.applied), [true, true, false]);

  // the Python migrator's file is refused here
  writeFileSync(join(third.path, "data.py"), "async def run(db):\n    pass\n");
  await assert.rejects(migrator.upgrade(), MigrationError);
});

// `npx orm` as a process of its own: src/cli.ts (Bun) or dist/src/cli.js (Node)
const CLI = [".js", ".ts"].map((ext) => fileURLToPath(new URL(`../src/cli${ext}`, import.meta.url))).find(existsSync)!;

test("npx orm migrate runs data steps", () => {
  const dir = mkdtempSync(join(tmpdir(), "orm-data-cli-"));
  const sqlite = 'datasource db {\n  provider = "sqlite"\n}\n';
  const schema = join(dir, "schema.prisma");
  const migrations = new Migrations(join(dir, "migrations"), schema);
  writeFileSync(schema, sqlite + V1);
  migrations.make();
  writeFileSync(schema, sqlite + V2);
  const second = migrations.make("slug")!;
  writeFileSync(
    join(second.path, "data.ts"),
    "export async function run(db) {\n  await db.execute(\"INSERT INTO author (name, slug) VALUES ('Ann', 'ann')\");\n}\n",
  );
  const env = { ...process.env, ORM_DATABASE_URL: `sqlite://${join(dir, "app.db")}` };
  const run = (...args: string[]) =>
    spawnSync(process.execPath, [CLI, "--schema", "schema.prisma", "--dir", "migrations", ...args], { cwd: dir, env, encoding: "utf8" });
  const done = run("migrate");
  assert.equal(done.status, 0, done.stderr);
  assert.deepEqual(done.stdout.trim().split("\n"), ["Applied 0001_initial", "Applied 0002_slug"]);

  mkdirSync(join(dir, "migrations", "0003_bad"));
  writeFileSync(join(dir, "migrations", "0003_bad", "up.sql"), "SELECT 1;\n");
  writeFileSync(join(dir, "migrations", "0003_bad", "data.ts"), 'export async function run() { throw new Error("bad data"); }\n');
  const failed = run("migrate");
  assert.equal(failed.status, 1);
  assert.ok(failed.stderr.includes("error: bad data"), failed.stderr);
});
