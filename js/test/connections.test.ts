import assert from 'node:assert/strict';
import { test } from 'node:test';
import { connect, define, getDatabase, loads, param, Registry, scope } from '../src/index.js';
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

test('a session lock needs Postgres', async () => {
  const db = await sqliteDb();
  try {
    await assert.rejects(db.lock(1, { session: true }, async () => {}), { name: 'QueryError', message: 'sqlite does not support advisory locks' });
  } finally {
    await db.close();
  }
});

/** A second database with the same tables, standing in for a replica. */
async function replicaUrl(): Promise<string> {
  const db = getDatabase();
  const url = db.url.replace(/\/[^/]*$/, '/orm_s6_replica_js');
  if ((await db.fetchText("SELECT 1 FROM pg_database WHERE datname = 'orm_s6_replica_js'")).length === 0) {
    await db.execute('CREATE DATABASE orm_s6_replica_js');
  }
  const replica = await connect(url, { default: false, maxConnections: 1 });
  try {
    await replica.dropTables();
    await replica.createTables();
    await User.objects.using(replica).insert({ email: 'r@example.com', name: 'Replica' });
  } finally {
    await replica.close();
  }
  return url;
}

test('replicas answer reads outside a transaction; writes and transactions use the primary', async () => {
  const db = getDatabase(), url = await replicaUrl();
  const routed = await connect(db.url, { replicas: [url, url], default: false, maxConnections: 2 });
  try {
    await User.objects.using(routed).insert({ email: 'p@example.com', name: 'Primary' });
    const users = User.objects.using(routed);
    assert.deepEqual((await users).map((u) => u.name), ['Replica']);
    assert.equal(await users.count(), 1);
    const byName = users.filter(User.name.eq(param('n'))).prepare();
    assert.deepEqual((await byName.all({ n: 'Replica' })).map((u) => u.name), ['Replica']);
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
    await routed.close();
  }
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
      const [s1, s2] = await Shop.objects.using(sdb).insertMany([{ name: 'one' }, { name: 'two' }]).returning();
      const orders = Order.objects.using(sdb);
      const [, , o3] = await orders.insertMany([{ shopId: s1.id, total: 10 }, { shopId: s1.id, total: 20 }, { shopId: s2.id, total: 30 }]).returning();
      for (const read of [() => orders.all(), () => orders.count(), () => Shop.objects.using(sdb).filter(Shop.orders.total.gt(25)).count()]) {
        await assert.rejects(read(), /scope\.shop/);
      }
      assert.equal(await orders.withoutDefaults().count(), 3);
      await scope({ shop: s1.id }, async () => {
        assert.deepEqual((await orders.all()).map((o: any) => o.total).sort(), [10, 20]);
        assert.equal(await orders.count(), 2);
        assert.deepEqual((await orders.filter(Order.total.gt(param('min'))).prepare().all({ min: 15 })).map((o: any) => o.total), [20]);
        assert.equal(await Shop.objects.using(sdb).filter(Shop.orders.total.gt(25)).count(), 0);
        await scope({ shop: s2.id }, async () => assert.deepEqual((await orders.all()).map((o: any) => o.total), [30]));
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
