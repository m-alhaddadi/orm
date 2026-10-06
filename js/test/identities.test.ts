/** Frozen schema identities through native compilation and both database drivers. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { resolve, join } from "node:path";
import { execFileSync } from "node:child_process";
import { connect, IntegrityError, load, Registry, SchemaError } from "../src/index.js";

const cli = resolve("../target/debug/orm");
const scratch = resolve("../.test-tmp/node-identities");
mkdirSync(scratch, { recursive: true });

for (const [provider, url] of [
  ["sqlite", "sqlite://:memory:"],
  ["postgresql", process.env["ORM_TEST_DATABASE_URL"] ?? ""],
] as const) {
  test(`ContentType identity roundtrip and frozen generation (${provider})`, { skip: provider === "postgresql" && !url }, async () => {
    const dir = mkdtempSync(join(scratch, "schema-"));
    const path = join(dir, "schema.prisma");
    writeFileSync(path, `datasource db {\nprovider = "${provider}"\n}\nmodel Post {\nid Int @id\n@@map("identity08_node_posts")\n}\nmodel Tag {\nid Int @id\nkind ContentType\nobject_id Int\n@@map("identity08_node_tags")\n}`);
    execFileSync(cli, ["--schema", path, "identities"]);
    const frozen = readFileSync(join(dir, "schema.identities.json"), "utf8");
    const registry = new Registry();
    const models = load(path, { registry });
    const Post = models["Post"]!;
    const Tag = models["Tag"]!;
    const types = registry.getEnum("ContentType");
    assert.equal(types["Post"], 1);
    assert.deepEqual(registry.ir()["identities"], JSON.parse(frozen));
    const db = await connect(url, { registry, default: false });
    try {
      await db.dropTables();
      await db.createTables();
      const post = await Post.objects.using(db).insert({ id: 7 });
      assert.ok(post);
      const tag = await Tag.objects.using(db).insert({ id: 1, kind: types["Post"], objectId: 7 });
      assert.equal((tag as unknown as Record<string, unknown>)["kind"], types["Post"]);
      await assert.rejects(db.execute("INSERT INTO identity08_node_tags (id, kind, object_id) VALUES (2, 99, 7)"), IntegrityError);
      await (post as { delete(): Promise<void> }).delete();
      assert.equal(await Tag.objects.using(db).count(), 1);
      execFileSync(cli, ["--schema", path, "generate", "typescript", "-o", join(dir, "models.ts")]);
      assert.match(readFileSync(join(dir, "models.ts"), "utf8"), /Post: 1/);
      assert.equal(readFileSync(join(dir, "schema.identities.json"), "utf8"), frozen);
    } finally {
      await db.dropTables();
      await db.close();
      rmSync(dir, { recursive: true });
    }
  });
}

test("missing/conflicting manifest fails during schema definition", () => {
  const dir = mkdtempSync(join(scratch, "invalid-"));
  const path = join(dir, "schema.prisma");
  try {
    writeFileSync(path, "model Post {\nid Int @id\n}\nmodel Tag {\nid Int @id\nkind ContentType\n}");
    assert.throws(() => load(path, { registry: new Registry() }), SchemaError);
    execFileSync(cli, ["--schema", path, "identities"]);
    const file = join(dir, "schema.identities.json");
    const value = JSON.parse(readFileSync(file, "utf8")) as { entries: { id: number }[] };
    value.entries[1]!.id = value.entries[0]!.id;
    writeFileSync(file, JSON.stringify(value));
    assert.throws(() => load(path, { registry: new Registry() }), /duplicate ContentType ID/);
  } finally { rmSync(dir, { recursive: true }); }
});
