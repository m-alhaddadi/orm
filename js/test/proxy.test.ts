import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { test } from "node:test";
import { connect, loads, Registry } from "../src/index.js";
import { native } from "../src/native.js";

const source = readFileSync(resolve(process.cwd(), "../tests/fixtures/proxy.prisma"), "utf8").replaceAll("proxy_05_", "proxy_05_node_");
const enabled = (JSON.parse(native().nativeArtifact()).capabilities as string[]).includes("proxy-models");
for (const dialect of ["sqlite", "postgres"]) {
  test(`proxy rows, client defaults, writes and relation targets (${dialect})`, { skip: !enabled }, async () => {
    const registry = new Registry();
    const models = loads((dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + source, { registry });
    assert.throws(() => loads(source.replace("references: [id]", "references: [missing]"), { registry: new Registry() }), /:\d+:\d+: relation \w+\.\w+: \w+ has no field missing after extension lowering/);
    const User = models.User!, Active = models.Active!, Post = models.Post!;
    const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
    const db = await connect(url, { registry, default: false });
    await db.createTables();
    try {
      await User.objects.using(db).insertMany([{ id: 1, name: null, status: "old" }, { id: 2, name: null, status: "old" }, { id: 3, name: "ok", status: "active" }]);
      const rows = await Active.objects.using(db).all() as { name: string | null; status: string }[];
      assert.equal(rows.length, 3);
      assert.equal(rows[0]!.name, null);
      assert.equal(rows[0]!.status, "old");
      assert.equal(await Active.objects.using(db).count(), 3);
      const created = await Active.objects.using(db).insert({ id: 4 }) as { name: string; status: string; settings: unknown; active: boolean; volume: number };
      assert.equal(created.name, "client"); assert.equal(created.status, "active");
      assert.deepEqual(created.settings, { labels: ["proxy", null], limit: 3 });
      assert.equal(created.active, true); assert.equal(created.volume, 7);
      const root = await User.objects.using(db).insert({ id: 5 }) as { name: null; status: string };
      assert.equal(root.name, null); assert.equal(root.status, "old");
      const changed = await Active.objects.using(db).update({ name: null, status: "old" }, { returning: true }) as { name: null; status: string }[];
      assert.ok(changed.every((r) => r.name === null && r.status === "old"));
      await Post.objects.using(db).insert({ id: 1, userId: 1 });
      const posts = await Post.objects.using(db).selectRelated(Post["user"] as never).all() as { user: { name: null; status: string } }[];
      assert.equal(posts[0]!.user.name, null); assert.equal(posts[0]!.user.status, "old");
    } finally { await db.dropTables(); await db.close(); }
  });
}
