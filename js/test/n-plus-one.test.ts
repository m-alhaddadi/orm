/** `debug.nPlusOne`: find N+1 query patterns by statement shape. */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { connect, debug, loads, Registry, type Database } from "../src/index.js";

const source = `datasource db {
  provider = "sqlite"
}
model Person {
  id        Int        @id
  name      String
  customers Customer[]
}
model Customer {
  id        Int    @id
  person_id Int
  person    Person @relation(fields: [person_id], references: [id])
}
`;

type Any = any; // eslint-disable-line @typescript-eslint/no-explicit-any
async function shop(): Promise<{ Person: Any; Customer: Any; db: Database }> {
  const registry = new Registry();
  const models = loads(source, { registry }) as Record<string, Any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  await db.createTables();
  const { Person, Customer } = models;
  await Person.objects.using(db).insertMany([...Array(8).keys()].map((i) => ({ id: i, name: `p${i}` })));
  await Customer.objects.using(db).insertMany([...Array(8).keys()].map((i) => ({ id: i, personId: i })));
  return { Person, Customer, db };
}

test("a loader in a loop throws with the shape, the call site and the fix", async () => {
  const { Customer, db } = await shop();
  try {
    const customers = await Customer.objects.using(db).orderBy(Customer.id).all();
    const error = await debug.nPlusOne(async () => {
      for (const c of customers) await c.loadPerson(); // the call site
    }, { threshold: 5, fail: true }).then(() => null, (e: unknown) => e);
    assert.ok(error instanceof debug.NPlusOne);
    const [shape] = error.report.repeated;
    assert.equal(shape!.count, 8);
    assert.match(shape!.sql, /^SELECT .*"person".*\?/);
    assert.doesNotMatch(shape!.sql, /7/);
    const [, file, line] = /^(.*):(\d+)$/.exec(shape!.site)!;
    assert.match(file!, /n-plus-one\.test\.(ts|js)$/);
    assert.match(readFileSync(file!, "utf8").split("\n")[Number(line) - 1]!, /the call site/);
    assert.equal(shape!.fix, "selectRelated(Customer.person)");
    assert.match(error.message, /8 queries with one shape `SELECT [\s\S]*use selectRelated\(Customer.person\)/);
  } finally { await db.close(); }
});

test("a related query in a loop names prefetchRelated and warns without fail", async () => {
  const { Person, db } = await shop();
  const warnings: Error[] = [];
  const listener = (w: Error) => { if (w.name === "NPlusOneWarning") warnings.push(w); };
  process.on("warning", listener);
  try {
    const people = await Person.objects.using(db).orderBy(Person.id).all();
    await debug.nPlusOne(async () => { for (const p of people) await p.customers.using(db).all(); }, { threshold: 3 });
    await new Promise((r) => setImmediate(r));
    assert.equal(warnings.length, 1);
    assert.match(warnings[0]!.message, /use prefetchRelated\(Person.customers\)/);
  } finally { process.off("warning", listener); await db.close(); }
});

test("a many-to-many query in a loop names prefetchRelated", async () => {
  const registry = new Registry();
  const models = loads(source.replace("  customers Customer[]", "  customers Customer[]\n  tags      Tag[]      @relation(through: PersonTag)") + `model Tag {
  id Int @id
}
model PersonTag {
  id        Int    @id
  person_id Int
  tag_id    Int
  person    Person @relation(fields: [person_id], references: [id])
  tag       Tag    @relation(fields: [tag_id], references: [id])
}
`, { registry }) as Record<string, Any>;
  const db = await connect("sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const { Person } = models;
    await Person.objects.using(db).insertMany([...Array(6).keys()].map((i) => ({ id: i, name: `p${i}` })));
    const people = await Person.objects.using(db).orderBy(Person.id).all();
    const error = await debug.nPlusOne(async () => { for (const p of people) await p.tags.using(db).all(); }, { threshold: 3, fail: true })
      .then(() => null, (e: unknown) => e);
    assert.ok(error instanceof debug.NPlusOne);
    assert.equal(error.report.repeated[0]!.fix, "prefetchRelated(Person.tags)");
    // A changed many-to-many query set is not a plain relation load, so it names no fix.
    const filtered = await debug.nPlusOne(async () => { for (const p of people) await p.tags.using(db).filter(models.Tag.id.gte(0)).all(); }, { threshold: 3, fail: true })
      .then(() => null, (e: unknown) => e);
    assert.ok(filtered instanceof debug.NPlusOne);
    assert.equal(filtered.report.repeated[0]!.fix, null);
  } finally { await db.close(); }
});

test("counts by shape, inside the scope only, with started work and inserts", async () => {
  const { Person, db } = await shop();
  try {
    let report!: debug.Report;
    const spy = async (fn: () => Promise<void>, threshold: number) => debug.nPlusOne(async () => { await fn(); }, { threshold, fail: true });
    await spy(async () => {
      for (let i = 0; i < 8; i++) await Person.objects.using(db).get(Person.id.eq(i));
      await Person.objects.using(db).count();
    }, 8);
    await assert.rejects(spy(async () => {
      await Promise.all([0, 1, 2].map((i) => Person.objects.using(db).filter(Person.id.eq(i)).exists()));
      for (let i = 0; i < 3; i++) await Person.objects.using(db).insert({ id: 100 + i, name: "n" });
    }, 2), (e: unknown) => {
      report = (e as debug.NPlusOne).report;
      return e instanceof debug.NPlusOne;
    });
    assert.deepEqual(report.repeated.map((s) => s.count), [3, 3]);
    await Person.objects.using(db).count();
    assert.equal([...report.shapes.values()].reduce((n, s) => n + s.count, 0), 6);
    await assert.rejects(debug.nPlusOne(async () => {}, { threshold: 0 }), RangeError);
  } finally { await db.close(); }
});

test("expectNoNPlusOne is the test helper", async () => {
  const { Person, db } = await shop();
  try {
    await debug.expectNoNPlusOne(async () => { for (let i = 0; i < 5; i++) await Person.objects.using(db).get(Person.id.eq(i)); });
    await assert.rejects(debug.expectNoNPlusOne(async () => {
      for (let i = 0; i < 3; i++) await Person.objects.using(db).get(Person.id.eq(i));
    }, { threshold: 2 }), debug.NPlusOne);
  } finally { await db.close(); }
});

test("the pages of one ORM loop count once, and long SQL is cut", async () => {
  const { Person, Customer, db } = await shop();
  try {
    await debug.nPlusOne(async () => {
      let n = 0;
      for await (const p of Person.objects.using(db).iterate(1)) n += p ? 1 : 0;
      assert.equal(n, 8);
      assert.equal((await Person.objects.using(db).inBulk([...Array(25_000).keys()])).size, 8);
    }, { threshold: 1, fail: true });
    // A loop in user code still counts every query, also inside a batch loop.
    await assert.rejects(debug.nPlusOne(async () => {
      for await (const p of Person.objects.using(db).iterate(2)) await Customer.objects.using(db).filter(Customer.personId.eq(p.id)).exists();
    }, { threshold: 3, fail: true }), debug.NPlusOne);
    const report = new debug.Report(1);
    report.shapes.set("k", { key: "k", sql: "SELECT " + "?, ".repeat(10_000), count: 2, site: "x", fix: null });
    assert.ok(report.message().length < 600 && report.message().includes(" ..."));
    const exact = new debug.Report(1);
    exact.shapes.set("k", { key: "k", sql: "x".repeat(500), count: 2, site: "x", fix: null });
    assert.ok(!exact.message().includes(" ..."));
  } finally { await db.close(); }
});

test("nested scopes count in the outer scope; updateMany counts; a changed related set names no fix", async () => {
  const { Person, Customer, db } = await shop();
  try {
    const people = await Person.objects.using(db).orderBy(Person.id).all();
    const counted: debug.Report[] = [];
    await debug.nPlusOne(async () => {
      // The innermost scope's queries count in every enclosing scope.
      await debug.nPlusOne(() => debug.nPlusOne(async () => {
        for (const p of people.slice(0, 3)) await Person.objects.using(db).filter(Person.id.eq(p.id)).exists();
      }, { threshold: 10 }), { threshold: 2, fail: true }).catch((e: debug.NPlusOne) => { counted.push(e.report); });
      for (const p of people.slice(0, 3)) await Person.objects.using(db).updateMany([{ id: p.id, name: "x" }]);
      for (const p of people.slice(0, 3)) await p.customers.using(db).filter(Customer.id.gte(0)).all();
    }, { threshold: 2, fail: true }).catch((e: debug.NPlusOne) => { counted.push(e.report); });
    const [innerReport, outerReport] = counted;
    assert.deepEqual([...innerReport!.shapes.values()].map((s) => s.count), [3]);
    assert.deepEqual(outerReport!.repeated.map((s) => [s.count, s.fix]), [[3, null], [3, null], [3, null]]);
  } finally { await db.close(); }
});
