/** Cursor pagination on both supported databases. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, loads, NotLoaded, QueryError, Registry } from "../src/index.js";
import { native } from "../src/native.js";
import { after } from "../src/pagination.js";

const schema = `
datasource db {
  provider = "sqlite"
}
model Owner {
  id BigInt @id @default(autoincrement())
  name String
  items Item[]
  @@map("page07_js_owners")
}
model Item {
  id BigInt @id @default(autoincrement())
  owner_id BigInt
  score Int
  rank Int?
  name String
  code String @unique
  at DateTime
  tags Json?
  owner Owner @relation(fields: [owner_id], references: [id], onDelete: Cascade)
  @@map("page07_js_items")
}
`;
const T0 = Date.UTC(2026, 9, 1);

/** T0 plus `us` microseconds, as the driver reads such a value. */
function at(us: number): Date {
  const d = new Date(T0 + Math.floor(us / 1000));
  if (us % 1000) {
    Object.defineProperty(d, "orm:micros", { value: us % 1000 });
    Object.defineProperty(d, "orm:micros-ms", { value: d.getTime() });
  }
  return d;
}

/** The order values of `cursor`. */
function values(cursor: string): unknown[] {
  return (JSON.parse(Buffer.from(cursor, "base64url").toString("utf8")) as { v: unknown[] }).v;
}

/** `cursor` with its order values replaced by `values`. */
function edited(cursor: string, values: unknown[]): string {
  const data = JSON.parse(Buffer.from(cursor, "base64url").toString("utf8")) as { o: string };
  return Buffer.from(JSON.stringify({ o: data.o, v: values })).toString("base64url");
}

async function open(dialect: "sqlite" | "postgres") {
  const registry = new Registry();
  const { Owner, Item } = loads(schema.replace('"sqlite"', dialect === "postgres" ? '"postgresql"' : '"sqlite"'), { registry }) as any;
  const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
  const db = await connect(url, { registry, default: false });
  await db.dropTables(); await db.createTables();
  const owner = await Owner.objects.using(db).insert({ name: "o" });
  await Item.objects.using(db).insertMany(Array.from({ length: 23 }, (_, i) => ({
    ownerId: owner.pk, score: i % 4, rank: i % 3 === 0 ? null : i % 5, name: `n${i % 3}`, code: `c${String(i).padStart(2, "0")}`, at: at(i * 7),
  })));
  return { db, Item, qs: Item.objects.using(db) };
}

/** Every page forward, then every page back from the end. */
async function walk(qs: any, size: number): Promise<bigint[]> {
  const pages: any[] = [];
  let cursor: string | null = null;
  for (;;) {
    const page: any = await qs.paginate({ first: size, after: cursor });
    pages.push(page);
    assert.ok(pages.length <= 100, "the walk does not end");
    if (!page.hasNext) break;
    cursor = page.nextCursor;
  }
  const forward = pages.flatMap((p) => p.items.map((x: any) => x.pk));
  assert.deepEqual(pages.map((p) => p.hasPrevious), [false, ...pages.slice(1).map(() => true)]);
  let back: bigint[] = [];
  cursor = null;
  for (;;) {
    const page: any = await qs.paginate({ last: size, before: cursor });
    back = [...page.items.map((x: any) => x.pk), ...back];
    if (!page.hasPrevious) break;
    cursor = page.previousCursor;
  }
  assert.deepEqual(back, forward);
  return forward;
}

for (const dialect of ["sqlite", "postgres"] as const) {
  test(`pages follow the order: ${dialect}`, async () => {
    const { db, Item, qs } = await open(dialect);
    try {
      const orders = [
        [],
        ["-score"],
        [Item.score.desc(), Item.name],
        ["name", "-at"],
        [Item.rank.asc({ nulls: "last" }), Item.score],
        [Item.rank.desc({ nulls: "first" })],
        [Item.rank.desc({ nulls: "last" }), "-code"],
      ];
      for (const keys of orders) {
        const ordered = keys.length ? qs.orderBy(...keys) : qs;
        const expected = (await qs.orderBy(...keys, Item.id)).map((x: any) => x.pk);
        for (const size of [1, 5, 23, 40]) assert.deepEqual(await walk(ordered, size), expected, `${String(keys)} by ${size}`);
      }
    } finally {
      await db.dropTables(); await db.close();
    }
  });

  test(`page shape, related rows and new rows: ${dialect}`, async () => {
    const { db, Item, qs } = await open(dialect);
    try {
      const page = await qs.load(Item.owner).load(Item.name).orderBy("-at").paginate({ first: 5 });
      assert.equal(page.items.length, 5);
      assert.ok(page.hasNext && !page.hasPrevious);
      assert.equal(page.items[0].owner.name, "o");
      assert.throws(() => page.items[0].at, NotLoaded);
      const rest = await qs.load(Item.name).orderBy("-at").paginate({ first: 100, after: page.nextCursor });
      assert.ok(rest.items.length === 18 && !rest.hasNext && rest.hasPrevious);
      const empty = await qs.orderBy("-at").paginate({ first: 5, after: rest.nextCursor });
      assert.deepEqual([empty.items, empty.nextCursor, empty.previousCursor], [[], null, null]);
      const first = await qs.orderBy("-score").paginate({ first: 10 });
      await qs.insert({ ownerId: first.items[0].ownerId, score: 99, name: "new", code: "new", at: new Date(T0) });
      const next = await qs.orderBy("-score").paginate({ first: 100, after: first.nextCursor });
      const seen = new Set(first.items.map((x: any) => x.pk));
      assert.ok(next.items.every((x: any) => !seen.has(x.pk)) && next.items.length === 13);
    } finally {
      await db.dropTables(); await db.close();
    }
  });

  test(`rejected orders and cursors: ${dialect}`, async () => {
    const { db, Item, qs } = await open(dialect);
    try {
      await assert.rejects(qs.orderBy(Item.rank).paginate({ first: 2 }), (e: Error) => e instanceof QueryError && /Item\.rank is nullable: order by Item\.rank\.asc\(\{ nulls: "first" \}\)/.test(e.message));
      await assert.rejects(qs.orderBy(Item.owner.name).paginate({ first: 2 }), /orders by columns of Item itself/);
      await assert.rejects(qs.orderBy(Item.score.add(1)).paginate({ first: 2 }), /orders by columns of Item itself/);
      await assert.rejects(qs.slice(0, 5).paginate({ first: 2 }), /sliced/);
      await assert.rejects(qs.paginate({ first: 2, before: "x" } as never), /first with after/);
      await assert.rejects(qs.paginate({} as never), /first or last/);
      await assert.rejects(qs.paginate({ first: 0 }), /at least 1/);
      const cursor = (await qs.orderBy("-score").paginate({ first: 2 })).nextCursor;
      assert.equal(cursor, "eyJvIjoiMDQ5ZjZhMzYwOGNlYzljMiIsInYiOlsiMyIsIjgiXX0", "the same cursor as tests/test_pagination.py");
      await assert.rejects(qs.orderBy("score").paginate({ first: 2, after: cursor }), /another order or model/);
      for (const bad of ["%%%", "e30", "eyJvIjoxLCJ2IjpbXX0"]) {
        await assert.rejects(qs.orderBy("-score").paginate({ first: 2, after: bad }), /invalid cursor/);
      }
    } finally {
      await db.dropTables(); await db.close();
    }
  });

  test(`edited cursors are query errors: ${dialect}`, async () => {
    const { db, Item, qs } = await open(dialect);
    try {
      const byAt = qs.orderBy("-at");
      const atCursor = (await byAt.paginate({ first: 1 })).nextCursor;
      for (const bad of ["2026-10-01T00:00:00", "2026-10-01", 5, "2026-02-30T00:00:00+00:00", "10000-03-15T00:00:00+00:00"]) {
        await assert.rejects(byAt.paginate({ first: 2, after: edited(atCursor, [bad, "1"]) }), (e: Error) => e instanceof QueryError && /invalid cursor/.test(e.message), String(bad));
      }
      const byScore = qs.orderBy("-score");
      const score = (await byScore.paginate({ first: 1 })).nextCursor;
      for (const bad of ["1".repeat(23), "5.5", "2147483648", "-2147483649", "+3", " 3", null, 3]) {
        await assert.rejects(byScore.paginate({ first: 2, after: edited(score, [bad, "1"]) }), (e: Error) => e instanceof QueryError && /invalid cursor/.test(e.message), String(bad));
      }
      await assert.rejects(byScore.paginate({ first: 2, after: edited(score, ["3", "1".repeat(23)]) }), (e: Error) => e instanceof QueryError);
      await assert.rejects(byScore.paginate({ first: 2, after: edited(score, ["3", "9223372036854775808"]) }), /invalid cursor/);
      assert.equal((await byScore.paginate({ first: 2, after: edited(score, ["-2147483648", "1"]) })).items.length, 0);
      const lastNulls = (await qs.orderBy(Item.rank.asc({ nulls: "last" })).paginate({ first: 1 })).nextCursor;
      await assert.rejects(qs.orderBy(Item.rank.asc({ nulls: "first" })).paginate({ first: 2, after: lastNulls }), /another order or model/);
      await assert.rejects(qs.orderBy("tags").paginate({ first: 2 }), /json columns have no cursor value/);
    } finally {
      await db.dropTables(); await db.close();
    }
  });

  test(`microsecond DateTime cursors and reading back: ${dialect}`, async () => {
    const { db, Item, qs } = await open(dialect);
    try {
      const page = await qs.orderBy("-at").paginate({ first: 2 });
      assert.equal(page.nextCursor, "eyJvIjoiNzNhMGQ4YWEwZWEzMWQwOSIsInYiOlsiMjAyNi0xMC0wMVQwMDowMDowMC4wMDAxNDcrMDA6MDAiLCIyMiJdfQ", "the same cursor as tests/test_pagination.py");
      assert.equal((await qs.filter(Item.at.eq(page.items[1].at)).get()).pk, page.items[1].pk);
      const second = (await qs.paginate({ first: 2 })).nextCursor;
      const back = await qs.paginate({ last: 1, before: second });
      assert.deepEqual(back.items.map((x: any) => x.pk), [1n]);
      assert.ok(back.hasNext && !back.hasPrevious);
      assert.deepEqual((await qs.paginate({ first: 1, before: null })).items.map((x: any) => x.pk), [1n]);
      assert.deepEqual((await qs.paginate({ last: 1, after: null })).items.map((x: any) => x.pk), [23n]);
      // More than one millisecond past T0: the hidden part holds only the microseconds below it.
      await qs.filter(Item.id.eq(1n)).update({ at: at(1147) });
      const late = (await qs.orderBy("-at").paginate({ first: 1 })).items[0];
      assert.equal(late.pk, 1n);
      assert.equal(values((await qs.orderBy("-at").paginate({ first: 1 })).nextCursor!)[0], "2026-10-01T00:00:00.001147+00:00");
      // A whole second has no fraction, as Python's isoformat() writes it.
      await qs.filter(Item.id.eq(1n)).update({ at: new Date(T0 + 1000) });
      assert.equal(values((await qs.orderBy("-at").paginate({ first: 1 })).nextCursor!)[0], "2026-10-01T00:00:01+00:00");
      // The microseconds belong to the time they were read with, not to a later setTime().
      const moved = (await qs.get(Item.id.eq(22n))).at as Date;
      moved.setTime(T0 + 5);
      await qs.filter(Item.id.eq(22n)).update({ at: moved });
      const stored = (await qs.get(Item.id.eq(22n))).at as Date;
      assert.equal(stored.getTime(), T0 + 5);
      assert.equal((stored as any)["orm:micros"], undefined);
      const bad = new Date(T0);
      Object.defineProperty(bad, "orm:micros", { value: 1500 });
      Object.defineProperty(bad, "orm:micros-ms", { value: T0 });
      await assert.rejects(qs.filter(Item.at.eq(bad)).fetch(), /invalid orm:micros 1500/);
    } finally {
      await db.dropTables(); await db.close();
    }
  });
}

async function postgres(source: string) {
  const registry = new Registry();
  const models = loads(source, { registry }) as any;
  const db = await connect(process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test", { registry, default: false });
  await db.dropTables(); await db.createTables();
  return { db, models };
}

test("a char order column: postgres", async () => {
  const { db, models } = await postgres('model Padded {\n  id Int @id\n  k String @db.Char(4)\n  @@map("page07_js_padded")\n}');
  const { Padded } = models;
  const qs = Padded.objects.using(db);
  try {
    await qs.insertMany(["ab", "ab", "ac", "ab", "a"].map((k, i) => ({ id: i + 1, k })));
    assert.equal((await qs.filter(Padded.k.eq((await qs.get(Padded.id.eq(1))).k))).length, 3);
    for (const size of [1, 2]) {
      assert.deepEqual(await walk(qs.orderBy("k"), size), [5, 1, 2, 4, 3]);
      assert.deepEqual(await walk(qs.orderBy("-k"), size), [3, 1, 2, 4, 5]);
    }
  } finally {
    await db.dropTables(); await db.close();
  }
});

test("non-finite float order values: postgres", async () => {
  const { db, models } = await postgres('model Measure {\n  id Int @id\n  f Float\n  d Decimal\n  @@map("page07_js_measures")\n}');
  const { Measure } = models;
  const qs = Measure.objects.using(db);
  try {
    await qs.insertMany([1, Infinity, Infinity, NaN, 2].map((f, i) => ({ id: i + 1, f, d: 1 })));
    assert.deepEqual(await walk(qs.orderBy("f"), 2), [1, 5, 2, 3, 4]);
    const page = await qs.orderBy("f").paginate({ first: 2, after: (await qs.orderBy("f").paginate({ first: 2 })).nextCursor });
    assert.equal(page.nextCursor, "eyJvIjoiYWY4ZDc2YzZmZDZmZTllNyIsInYiOlsiSW5maW5pdHkiLCIzIl19", "the same cursor as tests/test_pagination.py");
  } finally {
    await db.dropTables(); await db.close();
  }
});

test("a deep page has a leading bound", () => {
  const registry = new Registry();
  const { Item } = loads(schema, { registry }) as any;
  const sql = Item.objects.filter(after(Item._meta, [Item.score.desc(), Item.at.desc()], [3, new Date(T0)])).sql();
  assert.match(sql, /WHERE "page07_js_items"\."score" <= \S+ AND \(/);
  const nullable = Item.objects.filter(after(Item._meta, [Item.rank.asc({ nulls: "last" }), Item.id.asc()], [1, 1n])).sql();
  assert.ok(!nullable.includes('"rank" >=') && nullable.includes('"rank" > '), nullable);
});

test("edited cursors of decimal and uuid keys: postgres", async () => {
  const { db, models } = await postgres('model Keyed {\n  id String @id @db.Uuid\n  d Decimal\n  @@map("page07_js_keyed")\n}');
  const { Keyed } = models;
  const qs = Keyed.objects.using(db);
  try {
    await qs.insertMany(["00000000-0000-0000-0000-000000000001", "00000000-0000-0000-0000-000000000002"].map((id, i) => ({ id, d: i })));
    const byD = qs.orderBy("d");
    const cursor = (await byD.paginate({ first: 1 })).nextCursor!;
    for (const bad of [["NaN", values(cursor)[1]], ["0", "not-a-uuid"]]) {
      await assert.rejects(byD.paginate({ first: 1, after: edited(cursor, bad) }), (e: Error) => e instanceof QueryError && /invalid cursor/.test(e.message), String(bad));
    }
    await db.execute("UPDATE page07_js_keyed SET d = 'NaN'");
    await assert.rejects(byD.paginate({ first: 1 }), /can't make a cursor from the decimal NaN/);
  } finally {
    await db.dropTables(); await db.close();
  }
});

test("a date order column past year 9999: postgres", async () => {
  const { db, models } = await postgres('model Dated {\n  id Int @id\n  d DateTime @db.Date\n  @@map("page07_js_dated")\n}');
  const { Dated } = models;
  const qs = Dated.objects.using(db);
  try {
    await qs.insertMany(["9999-12-31", "+010000-03-15", "+010000-03-20"].map((d, i) => ({ id: i + 1, d: new Date(d) })));
    assert.deepEqual(await walk(qs.orderBy("d"), 1), [1, 2, 3]);
    const cursor = (await qs.orderBy("d").paginate({ first: 1 })).nextCursor!;
    for (const bad of ["2024-02-30", "10000-03-15", "2024"]) {
      await assert.rejects(qs.orderBy("d").paginate({ first: 1, after: edited(cursor, [bad, "1"]) }), /invalid cursor/, bad);
    }
  } finally {
    await db.dropTables(); await db.close();
  }
});

const proxies = (JSON.parse(native().nativeArtifact()) as { capabilities?: string[] }).capabilities?.includes("proxy-models") ?? false;

test("a proxy narrowed order column: postgres", async (t) => {
  if (!proxies) {
    t.skip("needs a native build with the proxy-models feature");
    return;
  }
  const { db, models } = await postgres(`
model User {
  id Int @id
  name String?
  code String? @unique
  @@map("page07_js_proxy_users")
}
model Named {
  name String
  code String @unique
  @@proxy.of(User)
}`);
  const { User, Named } = models;
  const qs = Named.objects.using(db);
  try {
    await User.objects.using(db).insertMany([{ id: 1, name: "a", code: "x" }, { id: 2 }, { id: 3, name: "c" }, { id: 4, code: "y" }]);
    await assert.rejects(qs.orderBy("name").paginate({ first: 1 }), /Named\.name is nullable/);
    assert.deepEqual(await walk(qs.orderBy(Named.name.asc({ nulls: "last" })), 1), [1, 3, 2, 4]);
    assert.deepEqual(await walk(qs.orderBy(Named.code.asc({ nulls: "last" })), 1), [1, 4, 2, 3]);
  } finally {
    await db.dropTables(); await db.close();
  }
});
