import assert from 'node:assert/strict';
import { test } from 'node:test';
import { getDatabase, TransactionRequired } from '../src/index.js';
import { useDatabase, otherDatabase } from './helpers.js';

useDatabase();

test('advisory keys match Python blake2b with an eight-byte digest', async () => {
  const db = getDatabase(), other = await otherDatabase();
  try {
    for (const [name, numeric] of [
      ['', -1970711489451281740n], ['import', -3453690058906639106n],
      ['ورود 🔒', -9085093355493825237n], ['x'.repeat(128), -8062319141637250046n],
      ['x'.repeat(129), 1791892490181060003n], ['x'.repeat(4096), 860336987148385126n],
    ] as const) {
      await db.transaction(async () => {
        assert.equal(await db.lock(name, {exclusive:false}), true);
        await other.transaction(async () => {
          assert.equal(await other.lock(numeric, {exclusive:false,nowait:true}), true);
          assert.equal(await other.lock(numeric, {nowait:true}), false);
        });
      });
      await other.transaction(async () => assert.equal(await other.lock(numeric, {nowait:true}), true));
    }
  } finally {
    await other.close();
  }
});

test('advisory validation retains errors and precedence', async () => {
  const db = getDatabase();
  await assert.rejects(db.lock(true as never), TransactionRequired);
  await db.transaction(async () => {
    for (const value of [true, 1.5, null, Number.MAX_SAFE_INTEGER + 1]) {
      await assert.rejects(db.lock(value as never), {name:'TypeError', message:`lock key must be an integer or a string, got ${String(value)}`});
    }
    for (const value of [-(1n<<63n)-1n, 1n<<63n]) {
      await assert.rejects(db.lock(value), {name:'RangeError', message:'lock key must fit in 64 bits'});
    }
    for (const value of [-(1n<<63n), (1n<<63n)-1n]) assert.equal(await db.lock(value, {nowait:true}), true);
    // TextEncoder replaces unpaired UTF-16 surrogates, as in the original JS implementation.
    assert.equal(await db.lock('\ud800', {nowait:true}), true);
  });
});
