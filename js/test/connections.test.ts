import assert from 'node:assert/strict';
import { test } from 'node:test';
import { connect, getDatabase, loads, Registry } from '../src/index.js';
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
