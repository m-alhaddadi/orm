/** `db.onQuery` hooks, `orm/otel` spans, and the N+1 finder on the same events. */
import assert from "node:assert/strict";
import { AsyncLocalStorage } from "node:async_hooks";
import { test } from "node:test";
import { connect, debug, IntegrityError, loads, param, Registry, type Database, type QueryEvent } from "../src/index.js";
import { instrument, type Span, type Tracer } from "../src/otel.js";

const source = `datasource db {
  provider = "sqlite"
}
model Person {
  id        Int        @id
  name      String     @unique
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
  await Person.objects.using(db).insertMany([0, 1, 2].map((i) => ({ id: i, name: `p${i}` })));
  await Customer.objects.using(db).insertMany([0, 1, 2, 3].map((i) => ({ id: i, personId: i % 2 })));
  return { Person, Customer, db };
}

test("a hook gets each statement with its shape, time and rows", async () => {
  const { Person, Customer, db } = await shop();
  try {
    const events: QueryEvent[] = [];
    const off = db.onQuery((e) => events.push(e));
    const people = await Person.objects.using(db).prefetchRelated(Person.customers).orderBy(Person.id).all();
    assert.equal(people.length, 3);
    const [select, prefetch] = events;
    assert.match(select!.sql, /^SELECT .*"person"/);
    assert.equal(select!.rows, 3);
    assert.match(prefetch!.sql, /"customer".*\?/);
    assert.equal(prefetch!.rows, 4);
    assert.equal(select!.error, null);
    assert.ok(select!.duration >= 0 && select!.duration < 5000 && select!.start <= prefetch!.start);
    assert.ok(Math.abs(select!.start - Date.now()) < 60_000);

    events.length = 0;
    assert.equal(await Customer.objects.using(db).updateMany([0, 1, 2].map((i) => ({ id: i, personId: 2 })), { batchSize: 1 }), 3);
    assert.deepEqual(events.map((e) => e.rows), [1, 1, 1]);

    events.length = 0;
    assert.equal(await db.execute("DELETE FROM customer WHERE id = 3"), 1);
    assert.deepEqual(events.map((e) => [e.sql, e.rows]), [["DELETE FROM customer WHERE id = 3", 1]]);

    off();
    events.length = 0;
    await Person.objects.using(db).count();
    assert.deepEqual(events, []);
  } finally { await db.close(); }
});

test("a failed statement gives an event with the error", async () => {
  const { Person, db } = await shop();
  try {
    const events: QueryEvent[] = [];
    db.onQuery((e) => events.push(e));
    await assert.rejects(Person.objects.using(db).insert({ id: 9, name: "p0" }), IntegrityError);
    assert.equal(events.length, 1);
    assert.match(events[0]!.sql, /^INSERT INTO/);
    assert.equal(events[0]!.rows, 0);
    assert.match(events[0]!.error!, /UNIQUE/);
  } finally { await db.close(); }
});

test("hooks run in the caller's context and see prepared queries", async () => {
  const { Person, db } = await shop();
  try {
    const request = new AsyncLocalStorage<string>();
    const seen: [string | undefined, number][] = [];
    db.onQuery((e) => seen.push([request.getStore(), e.rows]));
    const byName = Person.objects.using(db).filter(Person.name.eq(param("name"))).prepare();
    await request.run("r1", async () => assert.equal((await byName.get({ name: "p1" })).id, 1));
    await request.run("r2", () => db.transaction(() => Person.objects.using(db).filter(Person.id.eq(1)).update({ name: "x" })));
    assert.deepEqual(seen, [["r1", 1], ["r2", 1]]);
  } finally { await db.close(); }
});

test("an error a hook throws rejects the call", async () => {
  const { Person, db } = await shop();
  try {
    db.onQuery(() => { throw new Error("exporter down"); });
    await assert.rejects(Person.objects.using(db).count(), /exporter down/);
  } finally { await db.close(); }
});

class FakeSpan implements Span {
  status: { code: number; message?: string } | null = null;
  endTime: number | undefined;
  constructor(readonly name: string, readonly options: Parameters<Tracer["startSpan"]>[1]) {}
  setStatus(status: { code: number; message?: string }): void { this.status = status; }
  end(endTime?: number): void { this.endTime = endTime; }
}

class FakeTracer implements Tracer {
  readonly spans: FakeSpan[] = [];
  startSpan(name: string, options?: Parameters<Tracer["startSpan"]>[1]): FakeSpan {
    this.spans.push(new FakeSpan(name, options));
    return this.spans.at(-1)!;
  }
}

test("otel gives a client span per statement", async () => {
  const { Person, db } = await shop();
  try {
    const tracer = new FakeTracer();
    const stop = await instrument(db, { tracer });
    await Person.objects.using(db).filter(Person.id.eq(1)).all();
    await assert.rejects(Person.objects.using(db).insert({ id: 9, name: "p0" }), IntegrityError);
    const [ok, failed] = tracer.spans;
    assert.equal(ok!.name, "SELECT");
    assert.equal(ok!.options!.kind, 2); // SpanKind.CLIENT
    assert.equal(ok!.status, null);
    assert.equal(ok!.options!.attributes!["db.system.name"], "sqlite");
    assert.equal(ok!.options!.attributes!["db.response.returned_rows"], 1);
    assert.match(String(ok!.options!.attributes!["db.query.text"]), /^SELECT .*\?/);
    assert.ok(ok!.endTime! >= ok!.options!.startTime!);
    assert.equal(failed!.name, "INSERT");
    assert.equal(failed!.status!.code, 2); // SpanStatusCode.ERROR
    assert.match(failed!.status!.message!, /UNIQUE/);
    stop();
    await Person.objects.using(db).count();
    assert.equal(tracer.spans.length, 2);
  } finally { await db.close(); }
});

test("the N+1 finder counts the traced SQL", async () => {
  const { Person, db } = await shop();
  try {
    const report = await debug.nPlusOne(async () => {
      for (const i of [0, 1, 2]) await Person.objects.using(db).insert({ id: 10 + i, name: `n${i}` });
    }, { threshold: 2 }).then(() => null, (e: unknown) => e);
    assert.equal(report, null); // fail is off: a warning, no error
    const error = await debug.nPlusOne(async () => {
      for (const i of [0, 1, 2]) await Person.objects.using(db).insert({ id: 20 + i, name: `m${i}` });
    }, { threshold: 2, fail: true }).then(() => null, (e: unknown) => e);
    assert.ok(error instanceof debug.NPlusOne);
    const [shape] = error.report.repeated;
    assert.equal(shape!.count, 3);
    assert.match(shape!.sql, /^INSERT INTO "person".*\?/);
  } finally { await db.close(); }
});
