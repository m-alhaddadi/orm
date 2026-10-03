/** Value conversion of every column type, on a schema loaded at runtime (`loads()`,
 * untyped) with a registry of its own. */

import assert from "node:assert/strict";
import { after, before, test } from "node:test";

import { Decimal, Registry, connect, loads, type Database, type ModelClass, type ModelSpec } from "../src/index.js";
import { DATABASE_URL } from "./helpers.js";

const SCHEMA = `
model Doc {
  id      String    @id @default(dbgenerated("gen_random_uuid()")) @db.Uuid
  data    Json      @default("{\\"a\\": 1}")
  day     DateTime? @db.Date
  at      DateTime?
  big     BigInt?
  small   Int?
  ratio   Float?
  amount  Decimal?  @db.Decimal(20, 6)
  flag    Boolean?
  ids     BigInt[]   @default([])
  days    DateTime[] @default([]) @db.Date
  @@map("value_docs")
}
`;

const registry = new Registry();
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const Doc = loads(SCHEMA, { registry })["Doc"] as ModelClass<ModelSpec> & Record<string, any>;
let db: Database;

before(async () => {
  db = await connect(DATABASE_URL, { default: false, registry, maxConnections: 2 });
  await db.dropTables();
  await db.createTables();
});

after(async () => {
  await db.dropTables();
  await db.close();
});

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const objects = () => Doc.objects.using(db) as any;

test("every column type round-trips", async () => {
  const at = new Date("2026-01-02T03:04:05.678Z");
  const doc = await objects().insert({
    data: { list: [1, 2.5, "x", null, { deep: true }] },
    day: new Date("2026-03-04T00:00:00Z"),
    at,
    big: 9007199254740993n, // past 2^53: exact as a bigint
    small: -7,
    ratio: 0.25,
    amount: new Decimal("123456789.123456"),
    flag: true,
    ids: [1n, 2, 3n],
    days: [new Date("2026-01-01T12:00:00Z")],
  });
  assert.match(doc.id, /^[0-9a-f-]{36}$/);
  assert.deepEqual(doc.data, { list: [1, 2.5, "x", null, { deep: true }] });
  assert.equal(doc.day.toISOString(), "2026-03-04T00:00:00.000Z");
  assert.equal(doc.at.getTime(), at.getTime());
  assert.equal(doc.big, 9007199254740993n);
  assert.equal(doc.small, -7);
  assert.equal(doc.ratio, 0.25);
  assert.ok(doc.amount.eq("123456789.123456"));
  assert.equal(doc.flag, true);
  assert.deepEqual(doc.ids, [1n, 2n, 3n]);
  assert.equal(doc.days[0].toISOString(), "2026-01-01T00:00:00.000Z"); // the UTC day
  // the defaults and NULLs
  const empty = await objects().insert({});
  assert.deepEqual(empty.data, { a: 1 });
  assert.deepEqual([empty.day, empty.at, empty.big, empty.amount, empty.flag], [null, null, null, null, null]);
  // filters by every type
  const one = (q: unknown) => objects().filter(q).count();
  assert.equal(await one(Doc.id.eq(doc.id)), 1);
  assert.equal(await one(Doc.big.eq(9007199254740993n)), 1);
  assert.equal(await one(Doc.at.eq(at)), 1);
  assert.equal(await one(Doc.day.eq(new Date("2026-03-04T00:00:00Z"))), 1);
  assert.equal(await one(Doc.amount.gt(100)), 1);
  assert.equal(await one(Doc.ids.has(2)), 1);
  assert.equal(await one(Doc.data.eq({ a: 1 })), 1);
});

test("wrong value types are TypeErrors", async () => {
  const bad = async (values: object, re: RegExp) =>
    assert.rejects(objects().insert(values), (e: unknown) => e instanceof TypeError && re.test(e.message));
  await bad({ big: 1.5 }, /expected an integer/);
  await bad({ big: 2 ** 60 }, /expected an integer/); // unsafe as a number
  await bad({ big: "1" }, /expected a bigint or an integer number/);
  await bad({ small: 2 ** 40 }, /int32 range/);
  await bad({ ratio: "1" }, /expected a number/);
  await bad({ flag: 1 }, /expected a boolean/);
  await bad({ at: "2026-01-01" }, /expected a Date/);
  await bad({ at: new Date("nope") }, /invalid Date/);
  await bad({ id: "not-a-uuid" }, /invalid UUID/);
  await bad({ amount: Infinity }, /finite decimal/);
  await bad({ ids: 1n }, /expected an array/);
  await bad({ data: () => 1 }, /JSON value/);
});
