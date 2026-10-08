import assert from 'node:assert/strict';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { connect, define, getDatabase, loads, param, Registry, scope, type Database } from '../src/index.js';
import { native } from '../src/native.js';
import { User } from './blog/models.js';
import { useDatabase, otherDatabase } from './helpers.js';

useDatabase();

const SQLITE = 'datasource db { provider = "sqlite" }\nmodel Item {\n  id Int @id @default(autoincrement())\n}\n';

async function sqliteDb() {
  const registry = new Registry();
  loads(SQLITE, { registry });
  return connect('sqlite://:memory:', { registry, default: false });
}

test('onCommit runs after the outer commit, and a rolled-back savepoint drops its callbacks', async () => {
  const db = getDatabase(), other = await otherDatabase();
  const seen: unknown[] = [];
  try {
    await db.transaction(async () => {
      await User.objects.insert({ email: 'ann@example.com', name: 'Ann' });
      await db.onCommit(async () => { seen.push(await User.objects.using(other).filter(User.name.eq('Ann')).count()); });
      await db.transaction(async () => db.onCommit(() => seen.push('released')));
      await assert.rejects(db.transaction(async () => {
        await db.onCommit(() => seen.push('rolled back'));
        throw new Error('undo');
      }), /undo/);
      assert.deepEqual(seen, []);
    });
    assert.deepEqual(seen, [1, 'released']);
  } finally {
    await other.close();
  }
});

async function twoSqliteFiles() {
  const registry = new Registry();
  const { Item } = loads(SQLITE, { registry }) as any;
  const dir = mkdtempSync(join(tmpdir(), 'orm-s6-'));
  const dbs = [await connect(`sqlite://${join(dir, 'a.db')}`, { registry, default: false }), await connect(`sqlite://${join(dir, 'b.db')}`, { registry, default: false })];
  for (const db of dbs) await db.createTables();
  return { Item, a: dbs[0]!, b: dbs[1]! };
}

test('transactions and onCommit are kept per database', async () => {
  const { Item, a, b } = await twoSqliteFiles();
  const seen: string[] = [];
  try {
    await assert.rejects(a.transaction(async () => {
      await b.transaction(async () => {
        // A's transaction is still open inside B's.
        assert.notEqual(a.tx(), null);
        await Item.objects.using(a).insert({});
        await a.onCommit(() => seen.push('a'));
      });
      throw new Error('undo');
    }), /undo/);
    assert.equal(seen.length, 0);
    assert.equal(await Item.objects.using(a).count(), 0);
    await a.transaction(async () => {
      await b.transaction(async () => {
        await a.onCommit(() => seen.push('a'));
        await b.onCommit(() => seen.push('b'));
      });
      assert.deepEqual(seen, ['b']);
    });
    assert.deepEqual(seen, ['b', 'a']);
  } finally {
    await a.close();
    await b.close();
  }
});

test('onCommit after the transaction ended throws', async () => {
  const { a, b } = await twoSqliteFiles();
  let end!: () => void;
  const ended = new Promise<void>((resolve) => { end = resolve; });
  let late!: Promise<void>;
  try {
    await a.transaction(async () => {
      late = (async () => { await ended; await a.onCommit(() => undefined); })();
    });
    end();
    await assert.rejects(late, /has ended/);
  } finally {
    await a.close();
    await b.close();
  }
});

test('onCommit is dropped on rollback and runs at once outside a transaction', async () => {
  const db = getDatabase(), other = await otherDatabase();
  const seen: string[] = [];
  try {
    await assert.rejects(db.transaction(async () => {
      await db.onCommit(() => seen.push('dropped'));
      throw new Error('undo');
    }), /undo/);
    await db.onCommit(() => seen.push('now'));
    await db.transaction(async () => {
      await other.onCommit(() => seen.push('other'));
      assert.deepEqual(seen, ['now', 'other']);
    });
  } finally {
    await other.close();
  }
});

test('an onCommit error reaches the caller after the commit', async () => {
  const db = getDatabase();
  await assert.rejects(db.transaction(async () => {
    await User.objects.insert({ email: 'b@example.com', name: 'B' });
    await db.onCommit(() => { throw new Error('boom'); });
  }), /boom/);
  assert.equal(await User.objects.count(), 1);
});

test('onCommit on SQLite', async () => {
  const db = await sqliteDb();
  const seen: number[] = [];
  try {
    await db.transaction(async () => {
      await db.onCommit(() => seen.push(1));
      assert.deepEqual(seen, []);
    });
    assert.deepEqual(seen, [1]);
  } finally {
    await db.close();
  }
});

test('a session lock holds outside a transaction and times out', async () => {
  const db = getDatabase(), other = await otherDatabase();
  try {
    const got = await db.lock('import', { session: true, timeout: 5 }, async () => {
      assert.equal(db.tx(), null);
      const started = Date.now();
      await assert.rejects(other.lock('import', { session: true, timeout: 0.2 }, async () => assert.fail('held')),
        { name: 'LockNotAvailable', message: /after 0.2s/ });
      const waited = Date.now() - started;
      assert.ok(waited > 150 && waited < 3000, `waited ${waited} ms`);
      await assert.rejects(other.lock('import', { session: true, nowait: true }, async () => {}), { name: 'LockNotAvailable' });
      await other.transaction(async () => assert.equal(await other.lock('import', { nowait: true }), false));
      return 42;
    });
    assert.equal(got, 42);
    await other.lock('import', { session: true, nowait: true }, async () => {});
  } finally {
    await other.close();
  }
});

test('a session lock is released on error, and shared locks share', async () => {
  const db = getDatabase(), other = await otherDatabase();
  try {
    await assert.rejects(db.lock(7, { session: true }, async () => { throw new Error('undo'); }), /undo/);
    await other.lock(7, { session: true, nowait: true }, async () => {});
    await db.lock(8n, { session: true, exclusive: false }, async () => {
      await other.lock(8, { session: true, exclusive: false, nowait: true }, async () => {
        await assert.rejects(other.lock(8, { session: true, timeout: 0 }, async () => {}), { name: 'LockNotAvailable' });
      });
    });
  } finally {
    await other.close();
  }
});

test('a session lock keeps the error of its function when the unlock fails, and checks its options', async () => {
  const db = getDatabase();
  const kill = "SELECT pg_terminate_backend(pid) FROM pg_locks WHERE locktype = 'advisory' AND objid = 4243 AND granted";
  await assert.rejects(db.lock(4243, { session: true }, async () => { await db.execute(kill); throw new Error('mine'); }), /mine/);
  assert.equal(await db.lock(4243, { session: true }, async () => { await db.execute(kill); return 1; }), 1);
  assert.equal(await db.lock(5, { session: true, timeout: Infinity }, async () => 2), 2);
  for (const timeout of [-1, 3e6, NaN]) {
    await assert.rejects(db.lock(5, { session: true, timeout }, async () => 3), /lock timeout/);
  }
  await db.transaction(async () => {
    await assert.rejects((db.lock as any)(5, {}, async () => 4), /session: true/);
    await assert.rejects(db.lock(5, { timeout: 1 } as never), /session: true/);
  });
  await assert.rejects(db.lock(2 ** 53, { session: true }, async () => 5), /pass it as a bigint/);
  // The transaction form is the last overload, so helper types see it.
  const args: Parameters<Database['lock']> = [1, { exclusive: true }];
  assert.equal(args.length, 2);
});

test('a session lock needs Postgres', async () => {
  const db = await sqliteDb();
  try {
    await assert.rejects(db.lock(1, { session: true }, async () => {}), { name: 'QueryError', message: 'sqlite does not support advisory locks' });
  } finally {
    await db.close();
  }
});

/** Another database with the same tables, standing in for a replica: replica `n` holds `n`
 * users. Its name comes from the test database, so parallel runs do not share it. */
async function replicaUrl(n = 1): Promise<string> {
  const db = getDatabase();
  const name = `${db.url.split('/').pop()}_s6_replica_js${n}`;
  const url = db.url.replace(/\/[^/]*$/, `/${name}`);
  if ((await db.fetchText(`SELECT 1 FROM pg_database WHERE datname = '${name}'`)).length === 0) {
    await db.execute(`CREATE DATABASE "${name}"`);
  }
  const replica = await connect(url, { default: false, maxConnections: 1 });
  try {
    await replica.dropTables();
    await replica.createTables();
    const names = n === 1 ? ['Replica'] : Array.from({ length: n }, (_, i) => `R${n}-${i}`);
    await User.objects.using(replica).insertMany(names.map((u) => ({ email: `${u}@example.com`, name: u })));
  } finally {
    await replica.close();
  }
  return url;
}

async function backends(url: string): Promise<number> {
  const rows = await getDatabase().fetchText(`SELECT count(*) FROM pg_stat_activity WHERE datname = '${url.split('/').pop()}'`);
  return Number(rows[0]![0]);
}

test('replicas answer reads outside a transaction; writes and transactions use the primary', async () => {
  const db = getDatabase(), url = await replicaUrl(), url2 = await replicaUrl(2);
  const routed = await connect(db.url, { replicas: [url, url2], default: false, maxConnections: 2 });
  try {
    await User.objects.using(routed).insert({ email: 'p@example.com', name: 'Primary' });
    const users = User.objects.using(routed);
    // Reads take the replicas in turn.
    const reads = [];
    for (let i = 0; i < 2; i++) reads.push((await User.objects.using(routed)).map((u) => u.name).sort().join());
    assert.deepEqual(reads.sort(), ['R2-0,R2-1', 'Replica']);
    assert.deepEqual([await users.count(), await users.count()].sort(), [1, 2]);
    const r2 = users.filter(User.name.eq('R2-0'));
    assert.deepEqual([await r2.exists(), await r2.exists()].sort(), [false, true]);
    const byName = users.filter(User.name.eq(param('n'))).prepare();
    assert.deepEqual([(await byName.all({ n: 'Replica' })).length, (await byName.all({ n: 'Replica' })).length].sort(), [0, 1]);
    assert.deepEqual((await users.using('primary')).map((u) => u.name), ['Primary']);
    assert.deepEqual((await User.objects.using(routed.primary)).map((u) => u.name), ['Primary']);
    await routed.transaction(async () => {
      assert.deepEqual((await User.objects.using(routed)).map((u) => u.name), ['Primary']);
      assert.equal(routed.primary.tx(), routed.tx());
    });
    assert.equal(await users.filter(User.name.eq('Primary')).update({ name: 'P2' }), 1);
    assert.deepEqual((await User.objects.using(db)).map((u) => u.name), ['P2']);
    assert.throws(() => User.objects.using('replica' as never), TypeError);
  } finally {
    // Closing a primary view closes the whole database: the primary and each replica.
    await routed.primary.close();
  }
  assert.deepEqual([await backends(url), await backends(url2)], [0, 0]);
  // A replica that fails to open closes the pools opened before it.
  const before = await backends(url);
  await assert.rejects(connect(db.url, { replicas: [url, url.replace(/\/[^/]*$/, '/orm_s6_missing')], default: false }));
  assert.equal(await backends(url), before);
});

test('tenant ids are checked', async () => {
  const db = getDatabase();
  for (const bad of [true, null, NaN, Infinity]) {
    assert.throws(() => db.tenant(bad as never, async () => {}), /tenant id/);
  }
  // An empty id would look like no tenant: Postgres gives '' for a setting a pooled
  // connection had before.
  assert.throws(() => db.tenant('', async () => {}), /empty/);
});

const NOTES = `datasource db { provider = "postgresql" }
model Note {
  id     Int    @id @default(autoincrement())
  tenant String
  body   String
  @@map("s6_js_notes")
}
`;

const RLS = `
DO $$ BEGIN
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'orm_s6_app') THEN
    CREATE ROLE orm_s6_app LOGIN PASSWORD 'app';
  END IF;
END $$;
GRANT SELECT, INSERT, UPDATE, DELETE ON s6_js_notes TO orm_s6_app;
GRANT USAGE ON ALL SEQUENCES IN SCHEMA public TO orm_s6_app;
ALTER TABLE s6_js_notes ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant ON s6_js_notes
  USING (tenant = current_setting('app.tenant', true))
  WITH CHECK (tenant = current_setting('app.tenant', true));
`;

test('db.tenant sets app.tenant in each transaction for row-level security', async () => {
  const registry = new Registry();
  const Note = loads(NOTES, { registry })['Note'] as any;
  const url = getDatabase().url;
  const admin = await connect(url, { registry, default: false, maxConnections: 1 });
  await admin.dropTables();
  await admin.createTables();
  await admin.execute(RLS);
  await Note.objects.using(admin).insertMany([{ tenant: 'a', body: '1' }, { tenant: 'b', body: '2' }]);
  const app = await connect(`postgres://orm_s6_app:app@${url.split('@')[1]}`, { registry, default: false, maxConnections: 1 });
  try {
    assert.equal(await Note.objects.using(app).count(), 0);
    await app.tenant('a', async () => {
      assert.deepEqual((await Note.objects.using(app)).map((n: any) => n.body), ['1']);
      await app.transaction(async () => {
        assert.equal(await Note.objects.using(app).count(), 1);
        await Note.objects.using(app).insert({ tenant: 'a', body: '3' });
      });
      await assert.rejects(Note.objects.using(app).insert({ tenant: 'b', body: 'x' }), /row-level security/);
      await app.tenant(7, async () => assert.equal(await Note.objects.using(app).count(), 0));
      await app.primary.transaction(async () => assert.equal(await Note.objects.using(app).count(), 2));
    });
    assert.equal(await Note.objects.using(admin).count(), 3);
    assert.equal(await Note.objects.using(app).count(), 0);
  } finally {
    await app.close();
    await admin.dropTables();
    await admin.close();
  }
  const sqlite = await sqliteDb();
  try {
    assert.throws(() => sqlite.tenant('a', async () => {}), /row-level security/);
  } finally {
    await sqlite.close();
  }
});

const queryDefaults = (JSON.parse(native().nativeArtifact()) as { capabilities?: string[] }).capabilities?.includes('query-defaults') ?? false;

function scopedSchema(dialect: string) {
  const field = (name: string, type = 'int', flags = {}) => ({ name, column: name, type, ...flags });
  return { dialect, models: [
    { name: 'Shop', table: 's6_js_shops', fields: [field('id', 'int', { primary_key: true, auto_increment: true }), field('name', 'string')],
      relations: [{ name: 'orders', kind: 'many', target: 'Order', from: 'id', to: 'shop_id' }] },
    { name: 'Order', table: 's6_js_orders', fields: [field('id', 'int', { primary_key: true, auto_increment: true }), field('shop_id'), field('total')],
      relations: [{ name: 'shop', kind: 'one', target: 'Shop', from: 'shop_id', to: 'id', foreign_key: true, on_delete: 'cascade' }] },
  ], behavior: { schema_contract: 1, query_defaults: [{ model: 'Order', filter: {
    t: 'cmp', op: 'eq', l: { t: 'col', path: [], name: 'shop_id' }, r: { t: 'scope', name: 'shop' } } }] } };
}

for (const dialect of ['postgres', 'sqlite']) {
  test(`scope values in default filters are closed by default (${dialect})`, { skip: !queryDefaults }, async () => {
    const registry = new Registry();
    const m = define(scopedSchema(dialect) as never, { registry });
    const Shop = m['Shop'] as any, Order = m['Order'] as any;
    const sdb = await connect(dialect === 'sqlite' ? 'sqlite://:memory:' : getDatabase().url, { registry, default: false, maxConnections: 2 });
    await sdb.dropTables();
    await sdb.createTables();
    try {
      const [s1, s2] = await Shop.objects.using(sdb).insertMany([{ name: 'one' }, { name: 'two' }]);
      const orders = Order.objects.using(sdb);
      const [o1, , o3] = await orders.insertMany([{ shopId: s1.id, total: 10 }, { shopId: s1.id, total: 20 }, { shopId: s2.id, total: 30 }]);
      for (const read of [() => orders.all(), () => orders.count(), () => Shop.objects.using(sdb).filter(Shop.orders.total.gt(25)).count()]) {
        await assert.rejects(read(), /default filter of Order reads scope\.shop/);
      }
      assert.equal(await orders.withoutDefaults().count(), 3);
      await scope({ shop: s1.id }, async () => {
        assert.deepEqual((await orders.all()).map((o: any) => o.total).sort(), [10, 20]);
        assert.equal(await orders.count(), 2);
        assert.deepEqual((await orders.filter(Order.total.gt(param('min'))).prepare().all({ min: 15 })).map((o: any) => o.total), [20]);
        assert.equal(await Shop.objects.using(sdb).filter(Shop.orders.total.gt(25)).count(), 0);
        await scope({ shop: s2.id }, async () => assert.deepEqual((await orders.all()).map((o: any) => o.total), [30]));
        // A plain function that gives a query set (a thenable) still runs in the scope.
        assert.deepEqual((await scope({ shop: s2.id }, (() => orders) as never) as any[]).map((o: any) => o.total), [30]);
        // Every path that plans a statement reads the scope.
        assert.match(orders.sql(), /s6_js_orders/);
        assert.match(orders.filter(Order.total.gt(param('t'))).prepare().sql({ t: 1 }), /s6_js_orders/);
        assert.match(orders.select({ total: Order.total }).sql(), /s6_js_orders/);
        assert.equal(orders.filter(Order.id.eq(o1.id)).prepareUpdate({ total: 11 }).unique, true);
        assert.equal(await orders.update({ total: 0 }), 2);
        assert.equal(await orders.updateMany([{ id: o3.id, total: 99 }]), 0);
        assert.equal(await orders.filter(Order.id.eq(o3.id)).delete(), 0);
      });
      assert.deepEqual((await orders.withoutDefaults().all()).map((o: any) => o.total).sort(), [0, 0, 30]);
    } finally {
      await sdb.dropTables();
      await sdb.close();
    }
  });
}
