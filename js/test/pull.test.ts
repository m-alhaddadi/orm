/** `pull()`, baseline and drift against a live database. The Postgres tests use a
 * database of their own (`<test database>_pull_js`) and recreate its `public` schema. */

import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";

import { MigrationError, Migrations, Migrator, Registry, connect, loads, pull, type Database } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

// the repository root: from test/ (Bun) or dist/test/ (Node)
const ROOT = [join(import.meta.dirname, "..", ".."), join(import.meta.dirname, "..", "..", "..")].find((d) => existsSync(join(d, "examples")))!;
const FIXTURE = readFileSync(join(ROOT, "tests", "fixtures", "pull_live.sql"), "utf8");

const PULL_DB = new URL(DATABASE_URL).pathname.slice(1) + "_pull_js";
const PULL_URL = Object.assign(new URL(DATABASE_URL), { pathname: "/" + PULL_DB }).toString();

// What the fixture holds that the schema language can't say (see the `gap:` lines).
const EXPECTED = [
  "drop table bundles_tag",
  "drop default of bundles_bundle.id",
  "make bundles_bundle.id an identity column",
  "drop column bundles_slot.during",
];

before(async () => {
  const admin = await connect(DATABASE_URL, { maxConnections: 1, default: false, registry: new Registry() });
  try {
    if (!(await admin.fetchText(`SELECT 1 FROM pg_database WHERE datname = '${PULL_DB}'`)).length) {
      await admin.execute(`CREATE DATABASE "${PULL_DB}"`);
    }
  } finally {
    await admin.close();
  }
});

const opened: Database[] = [];
after(async () => {
  for (const db of opened) await db.close();
});

test("pull, baseline and drift of a Django-style database", async () => {
  const db = await connect(PULL_URL, { maxConnections: 2, default: false, registry: new Registry() });
  opened.push(db);
  await db.execute("DROP SCHEMA public CASCADE; CREATE SCHEMA public;" + FIXTURE);

  const pulled = await pull(db);
  assert.ok(pulled.schema.includes('map: "bundles_bundle_shop_id_8f3e_fk_account_shop_id"'));
  assert.ok(pulled.schema.includes("@@trigger(account_shop_updated_at, after: [update], for_each: statement"));
  assert.ok(pulled.gaps.some((g) => g.startsWith("rule bundles_bundle_soft_delete")));
  assert.deepEqual(pulled.differences.map((s) => s.summary), EXPECTED);

  const dir = mkdtempSync(join(tmpdir(), "orm-pull-"));
  pulled.write(join(dir, "schema.prisma"));
  const migrator = new Migrator(db, new Migrations(join(dir, "migrations"), join(dir, "schema.prisma")));
  assert.equal((await migrator.baseline()).name, "0001_initial");
  assert.deepEqual(await migrator.upgrade(), []);
  const drift = await migrator.drift();
  assert.equal(drift.migration, "0001_initial");
  assert.deepEqual(drift.steps.map((s) => s.summary), EXPECTED);

  await db.execute("ALTER TABLE account_shop ADD COLUMN extra integer");
  assert.ok((await migrator.drift()).steps.some((s) => s.summary === "drop column account_shop.extra"));
  await assert.rejects(migrator.baseline(), MigrationError);
});

test("SQLite pull, baseline and drift", async () => {
  const dir = mkdtempSync(join(tmpdir(), "orm-pull-sqlite-"));
  const source = `
datasource db {
  provider = "sqlite"
}

model Owner {
  id    BigInt @id @default(autoincrement())
  email String @unique
  pets  Pet[]
}

model Pet {
  id       BigInt @id @default(autoincrement())
  owner_id BigInt
  age      Int    @default(0)
  owner    Owner  @relation(fields: [owner_id], references: [id], onDelete: Cascade)

  @@index([owner_id, -age])
  @@check("age >= 0", name: "pet_age")
}
`;
  writeFileSync(join(dir, "schema.prisma"), source);
  const registry = new Registry();
  loads(source, { registry });
  const db = await connect(`sqlite://${join(dir, "live.db")}`, { default: false, registry });
  opened.push(db);
  const made = new Migrations(join(dir, "made"), join(dir, "schema.prisma"));
  made.make();
  await new Migrator(db, made).upgrade();

  const pulled = await pull(db);
  assert.deepEqual(pulled.differences, []);
  assert.ok(pulled.schema.includes('@@check("age >= 0", name: "pet_age")'));
  pulled.write(join(dir, "pulled.prisma"));
  await db.execute("DROP TABLE orm_migrations");
  const migrator = new Migrator(db, new Migrations(join(dir, "migrations"), join(dir, "pulled.prisma")));
  await migrator.baseline();
  assert.deepEqual((await migrator.drift()).steps, []);
  await db.execute("CREATE INDEX extra_idx ON pet (age)");
  assert.deepEqual((await migrator.drift()).steps.map((s) => s.summary), ["drop index extra_idx"]);
});
