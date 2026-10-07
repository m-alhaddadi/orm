/** Cursor pagination on both supported databases. */

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, loads, NotLoaded, QueryError, Registry } from "../src/index.js";

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
  owner Owner @relation(fields: [owner_id], references: [id], onDelete: Cascade)
  @@map("page07_js_items")
}
`;
const T0 = Date.UTC(2026, 9, 1);

async function open(dialect: "sqlite" | "postgres") {
  const registry = new Registry();
  const { Owner, Item } = loads(schema.replace('"sqlite"', dialect === "postgres" ? '"postgresql"' : '"sqlite"'), { registry }) as any;
  const url = dialect === "sqlite" ? "sqlite://:memory:" : process.env["ORM_TEST_DATABASE_URL"] ?? "postgres://postgres:postgres@localhost/orm_test";
  const db = await connect(url, { registry, default: false });
  await db.dropTables(); await db.createTables();
  const owner = await Owner.objects.using(db).insert({ name: "o" });
  await Item.objects.using(db).insertMany(Array.from({ length: 23 }, (_, i) => ({
    ownerId: owner.pk, score: i % 4, rank: i % 3 === 0 ? null : i % 5, name: `n${i % 3}`, code: `c${String(i).padStart(2, "0")}`, at: new Date(T0 + i * 7),
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
      const page = await qs.selectRelated(Item.owner).only(Item.name).orderBy("-at").paginate({ first: 5 });
      assert.equal(page.items.length, 5);
      assert.ok(page.hasNext && !page.hasPrevious);
      assert.equal(page.items[0].owner.name, "o");
      assert.throws(() => page.items[0].at, NotLoaded);
      const rest = await qs.only(Item.name).orderBy("-at").paginate({ first: 100, after: page.nextCursor });
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
}
