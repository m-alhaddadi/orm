import assert from 'node:assert/strict';
import { test } from 'node:test';
import { connect, getDatabase, loads, param, Registry } from '../src/index.js';
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
