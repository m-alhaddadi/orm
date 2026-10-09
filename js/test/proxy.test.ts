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
      const posts = await Post.objects.using(db).load(Post["user"] as never).all() as { user: { name: null; status: string } }[];
      assert.equal(posts[0]!.user.name, null); assert.equal(posts[0]!.user.status, "old");
    } finally { await db.dropTables(); await db.close(); }
  });
}

const fieldsSource = `
model Member {
  id     Int     @id
  name   String
  status String  @default("new")
  legacy String?
  @@map("proxy_fields_node_members")
}
model Current {
  name String @client_default("anon")
  @@proxy.of(Member)
  @@proxy.fields(exclude: ["legacy", "status"])
}
model Named {
  @@proxy.of(Current)
  @@proxy.fields(include: ["id", "name"])
}
`;
for (const dialect of ["sqlite", "postgres"]) {
  test(`omitted proxy fields are not part of the model (${dialect})`, { skip: !enabled }, async () => {
    const registry = new Registry();
    const models = loads((dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + fieldsSource, { registry });
    const Member = models.Member!, Current = models.Current!, Named = models.Named!;
    assert.deepEqual([...Current._meta.fields.keys()], ["id", "name"]);
    assert.deepEqual([...Named._meta.fields.keys()], ["id", "name"]);
    assert.equal((Current as Record<string, unknown>).legacy, undefined);
    assert.notEqual((Member as Record<string, unknown>).legacy, undefined);
    const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
    const db = await connect(url, { registry, default: false });
    await db.createTables();
    try {
      await Member.objects.using(db).insert({ id: 1, name: "ann", status: "old", legacy: "x" });
      const rows = await Current.objects.using(db).all() as object[];
      assert.deepEqual(Object.keys(rows[0]!), ["id", "name"]);
      await assert.rejects(Current.objects.using(db).insert({ id: 2, legacy: "y" } as never), /Current has no field "legacy"/);
      const created = await Current.objects.using(db).insert({ id: 2 }) as { name: string };
      assert.equal(created.name, "anon");
      const stored = await Member.objects.using(db).filter((Member as unknown as { id: { eq(v: number): never } }).id.eq(2)).get() as { status: string; legacy: null };
      assert.equal(stored.status, "new"); assert.equal(stored.legacy, null);
    } finally { await db.dropTables(); await db.close(); }
  });
}

test("proxy field rules fail at definition", { skip: !enabled }, () => {
  const cases: [string, RegExp][] = [
    ['@@proxy.fields(exclude: ["name"])', /Current\.name: a NOT NULL field without a database default cannot be omitted/],
    ['@@proxy.fields(include: ["id"], exclude: ["legacy"])', /include and exclude together/],
    ['@@proxy.nonNull("legacy")', /proxy\.nonNull/],
  ];
  for (const [attribute, message] of cases) {
    assert.throws(() => loads(fieldsSource.replace('@@proxy.fields(exclude: ["legacy", "status"])', attribute), { registry: new Registry() }), message);
  }
});
