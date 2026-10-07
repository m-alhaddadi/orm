import assert from "node:assert/strict";
import { test } from "node:test";
import { connect, loads, Registry, type Row } from "../src/index.js";

const source = 'datasource db { provider = "sqlite" }\nmodel HookReport {\n id Int @id\n code String @unique\n size Int\n note String?\n}';

async function open() {
  const registry = new Registry();
  const Report = loads(source, { registry }).HookReport as any;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  return { db, Report, objects: Report.objects.using(db) as import("../src/index.js").QuerySet<any> };
}

test("prepareInsert checks before execute and inserts the given values", async () => {
  const { db, objects } = await open();
  try {
    assert.throws(() => objects.prepareInsert({ id: 1, code: "a" }), /size is required/);
    assert.throws(() => objects.prepareInsert({ id: "not an integer", code: "a", size: 1 }));
    const prepared = objects.prepareInsert({ id: 1, code: "a", size: 0 });
    assert.equal(await objects.count(), 0);
    const row = await prepared.execute({ id: 1, code: "a", size: 7 }) as Row;
    assert.deepEqual([row["id"], row["size"]], [1, 7]);
    assert.equal((await objects.prepareInsert({ id: 2, code: "b", size: 2 }).execute() as Row)["code"], "b");
  } finally { await db.close(); }
});

test("prepareUpdate reports a unique row and executes", async () => {
  const { db, Report, objects } = await open();
  try {
    await objects.insertMany([{ id: 1, code: "a", size: 1 }, { id: 2, code: "b", size: 1 }]);
    assert.ok(objects.filter(Report.id.eq(1)).prepareUpdate({ size: 0 }).unique);
    assert.ok(objects.filter(Report.size.eq(1).and(Report.code.eq("b"))).prepareUpdate({ size: 0 }).unique);
    assert.ok(!objects.filter(Report.size.eq(1)).prepareUpdate({ size: 0 }).unique);
    assert.throws(() => objects.filter(Report.id.eq(1)).prepareUpdate({ size: "not an integer" }));
    const prepared = objects.filter(Report.id.eq(1)).prepareUpdate({ size: 0 });
    const rows = await prepared.execute({ size: 5 }, { returning: true }) as Row[];
    assert.deepEqual(rows.map(r => r["size"]), [5]);
    assert.equal(await prepared.execute(), 1);
  } finally { await db.close(); }
});

test("addRowDecoder runs on every materialized instance", async () => {
  const { db, Report, objects } = await open();
  try {
    Report._meta.addRowDecoder((row: Row) => { if (Object.hasOwn(row, "code")) row["code"] = String(row["code"]).toUpperCase(); });
    const inserted = await objects.insert({ id: 1, code: "a", size: 1 }) as Row;
    assert.equal(inserted["code"], "A");
    assert.equal(((await objects.filter(Report.id.eq(1)).get()) as Row)["code"], "A");
    assert.equal(await objects.filter(Report.code.eq("a")).count(), 1);
  } finally { await db.close(); }
});
