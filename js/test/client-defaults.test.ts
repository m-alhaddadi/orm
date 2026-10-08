import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, Registry } from "../src/index.js";

const source = `
enum Status {
  ACTIVE @map("active")
  OLD @map("old")
  @@storage(text)
}
model Item {
  id     String   @id @client_default(uuid7()) @db.Uuid
  token  String   @client_default(uuid())
  status Status   @default(OLD) @client_default(ACTIVE)
  at     DateTime @client_default(now())
  meta   Json     @client_default("{\\"a\\": [1]}")
  n      Int      @client_default(3)
  note   String?  @client_default("note")
  prisma String   @default(uuid())
  @@map("client_default_node_items")
}
`;
type Item = { id: string; token: string; status: string; at: Date; meta: unknown; n: number; note: string | null; prisma: string };
const version = (id: string) => Number(id[14]);

test("client defaults make fields optional but are not database defaults", () => {
  const Item = loads(source, { registry: new Registry() }).Item!;
  const fields = Item._meta.fieldList;
  assert.ok(fields.every((f) => f.hasInsertDefault));
  assert.deepEqual(fields.filter((f) => f.hasServerValue).map((f) => f.name), ["status"]);
});

for (const dialect of ["sqlite", "postgres"]) {
  test(`client defaults fill insert, bulk insert and upsert (${dialect})`, async () => {
    const registry = new Registry();
    const Item = loads((dialect === "sqlite" ? 'datasource db { provider = "sqlite" }\n' : "") + source, { registry }).Item!;
    const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env.ORM_TEST_DATABASE_URL ?? "postgres://postgres:postgres@localhost/orm_test";
    const db = await connect(url, { registry, default: false });
    await db.createTables();
    try {
      const item = await Item.objects.using(db).insert({}) as Item;
      assert.equal(version(item.id), 7);
      assert.equal(version(item.token), 4);
      assert.equal(version(item.prisma), 4);
      assert.equal(item.status, "active");
      assert.ok(Math.abs(item.at.getTime() - Date.now()) < 60_000);
      assert.deepEqual(item.meta, { a: [1] });
      assert.equal(item.n, 3); assert.equal(item.note, "note");
      // An explicit value, also null, wins over both defaults.
      const explicit = await Item.objects.using(db).insert({ note: null, status: "old", n: 9 }) as Item;
      assert.equal(explicit.note, null); assert.equal(explicit.status, "old"); assert.equal(explicit.n, 9);
      const rows = await Item.objects.using(db).insertMany([{ note: "x" }, {}, { note: null }]).returning() as Item[];
      assert.deepEqual(rows.map((r) => r.note), ["x", "note", null]);
      assert.equal(new Set([...rows.map((r) => r.id), item.id, explicit.id]).size, 5);
      const columns = Item as unknown as Record<string, never>;
      const upserted = await Item.objects.using(db).insert({ id: item.id, n: 4 }).onConflict(columns.id!, { update: true, updateFields: [columns.n!, columns.token!] as never }).returning() as Item;
      assert.equal(upserted.id, item.id); assert.equal(upserted.n, 4); assert.notEqual(upserted.token, item.token);
      // Other writers get only the database default.
      await db.execute("INSERT INTO client_default_node_items (id, token, at, meta, n, prisma) VALUES ('0192f7e2-0000-7000-8000-000000000002', 't', '2026-01-01T00:00:00Z', '{}', 1, 'p')");
      const raw = await Item.objects.using(db).filter((columns.token as unknown as { eq(v: string): never }).eq("t")).get() as Item;
      assert.equal(raw.status, "old"); assert.equal(raw.note, null);
    } finally { await db.dropTables(); await db.close(); }
  });
}
