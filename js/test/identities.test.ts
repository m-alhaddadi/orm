/** Frozen schema identities through native compilation and both database drivers. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { resolve, join } from "node:path";
import { main as cli } from "../src/cli.js";
import { connect, IntegrityError, load, Migrations, Registry, SchemaError } from "../src/index.js";
import { native } from "../src/native.js";

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
    assert.equal(await cli(["--schema", path, "identities"]), 0);
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
      assert.equal(await cli(["--schema", path, "generate", "typescript", "-o", join(dir, "models.ts")]), 0);
      assert.match(readFileSync(join(dir, "models.ts"), "utf8"), /Post: 1/);
      assert.equal(readFileSync(join(dir, "schema.identities.json"), "utf8"), frozen);
    } finally {
      await db.dropTables();
      await db.close();
      rmSync(dir, { recursive: true });
    }
  });
}

test("missing/conflicting manifest fails during schema definition", async () => {
  const dir = mkdtempSync(join(scratch, "invalid-"));
  const path = join(dir, "schema.prisma");
  try {
    writeFileSync(path, "model Post {\nid Int @id\n}\nmodel Tag {\nid Int @id\nkind ContentType\n}");
    assert.throws(() => load(path, { registry: new Registry() }), SchemaError);
    assert.equal(await cli(["--schema", path, "identities"]), 0);
    const file = join(dir, "schema.identities.json");
    const value = JSON.parse(readFileSync(file, "utf8")) as { entries: { id: number }[] };
    value.entries[1]!.id = value.entries[0]!.id;
    writeFileSync(file, JSON.stringify(value));
    assert.throws(() => load(path, { registry: new Registry() }), /duplicate ContentType ID/);
  } finally { rmSync(dir, { recursive: true }); }
});

test("a generic relation schema follows the artifact capability", async () => {
  const dir = mkdtempSync(join(scratch, "generic-"));
  const path = join(dir, "schema.prisma");
  try {
    writeFileSync(path, `datasource db {\nprovider = "sqlite"\n}\nmodel Post {\nid Int @id\n@@map("generic08_node_posts")\n}\nmodel Tag {\nid Int @id\ncontent_type ContentType?\nobject_id Int?\n@@generic.relation("target", type: "content_type", key: "object_id", targets: ["Post"])\n@@map("generic08_node_tags")\n}`);
    assert.equal(await cli(["--schema", path, "identities"]), 0);
    if (!(JSON.parse(native().nativeArtifact()).capabilities as string[]).includes("generic-relations")) {
      assert.throws(() => load(path, { registry: new Registry() }), /rebuild/);
      return;
    }
    const registry = new Registry();
    const models = load(path, { registry });
    const types = registry.getEnum("ContentType");
    const db = await connect("sqlite://:memory:", { registry, default: false });
    try {
      await db.createTables();
      await models["Post"]!.objects.using(db).insert({ id: 7 });
      await models["Tag"]!.objects.using(db).insert({ id: 1, contentType: types["Post"], objectId: 7 });
      const tag = await models["Tag"]!.objects.using(db).get() as unknown as Record<string, unknown>;
      assert.equal(tag["contentType"], types["Post"]);
      assert.equal(tag["objectId"], 7);
    } finally { await db.close(); }
  } finally { rmSync(dir, { recursive: true }); }
});

const genericHeader = `datasource db {\nprovider = "sqlite"\n}\nmodel Post {\nid Int @id\ntags Tag[] @generic.reverse\n@@map("generic_field_node_posts")\n}\nmodel Photo {\nid Int @id\n@@map("generic_field_node_photos")\n}\n`;
const hasGeneric = () => (JSON.parse(native().nativeArtifact()).capabilities as string[]).includes("generic-relations");

test("a Generic field creates its pair, its index and the reverse field", async () => {
  assert.throws(() => native().compileSchema("model Tag {\n id Int @id\n target Generic?\n}", null), /unknown type Generic; a Generic field needs @generic.relation/);
  if (!hasGeneric()) return;
  const dir = mkdtempSync(join(scratch, "generic-field-"));
  const path = join(dir, "schema.prisma");
  try {
    writeFileSync(path, genericHeader + `model Tag {\nid Int @id\ntarget Generic? @generic.relation(targets: ["Post", "Photo"])\nnote String\n@@map("generic_field_node_tags")\n}`);
    assert.equal(await cli(["--schema", path, "identities"]), 0);
    const registry = new Registry();
    load(path, { registry });
    const ir = registry.ir() as unknown as { models: { name: string; fields: { name: string; nullable?: boolean; enum?: string }[]; indexes?: unknown[] }[]; behavior: Record<string, unknown> };
    const tag = ir.models.find((m) => m.name === "Tag")!;
    assert.deepEqual(tag.fields.map((f) => [f.name, f.nullable ?? false, f.enum ?? null]),
      [["id", false, null], ["target_type", true, "ContentType"], ["target_id", true, null], ["note", false, null]]);
    assert.deepEqual(tag.indexes, [{ columns: [{ field: "target_type" }, { field: "target_id" }] }]);
    assert.deepEqual(ir.behavior["generic_reverse"], [{ model: "Post", name: "tags", source: "Tag", relation: "target" }]);
  } finally { rmSync(dir, { recursive: true }); }
});

test("moving to the Generic field form needs no migration", { skip: !hasGeneric() }, async () => {
  const dir = mkdtempSync(join(scratch, "generic-move-"));
  const path = join(dir, "schema.prisma");
  const explicit = `model Tag {\nid Int @id\ntarget_type ContentType?\ntarget_id Int?\nkind ContentType\nobject_id Int\n`
    + `@@generic.relation("target", type: "target_type", key: "target_id", targets: ["Post", "Photo"])\n`
    + `@@generic.relation("owner", type: "kind", key: "object_id", targets: ["Post"])\n@@index([target_type, target_id])\n@@map("generic_field_node_tags")\n}`;
  const field = `model Tag {\nid Int @id\ntarget Generic? @generic.relation(targets: ["Post", "Photo"])\nkind ContentType\nobject_id Int\n`
    + `owner Generic @generic.relation(type: "kind", key: "object_id", targets: ["Post"], index: false)\n@@map("generic_field_node_tags")\n}`;
  try {
    for (const [i, model] of [explicit, field].entries()) {
      writeFileSync(path, genericHeader.replace("tags Tag[] @generic.reverse", `tags Tag[] @generic.reverse(relation: "target")`) + model);
      assert.equal(await cli(["--schema", path, "identities"]), 0);
      const registry = new Registry();
      load(path, { registry });
      assert.equal(new Migrations(join(dir, "migrations"), registry).make("initial") === null, i === 1);
    }
  } finally { rmSync(dir, { recursive: true }); }
});
