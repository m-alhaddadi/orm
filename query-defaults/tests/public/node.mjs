import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { connect, define, NotLoaded, Registry } from '../../../js/dist/src/index.js';
const registry = new Registry();
const schema = JSON.parse(readFileSync(process.argv[2], 'utf8'));
const { Policy } = define(schema, { registry });
const db = await connect(process.env.ORM_TEST_DATABASE_URL ?? 'sqlite://:memory:', { registry, default: false });
try {
  await db.dropTables(); await db.createTables();
  await db.execute("INSERT INTO policy (id, active, name, bio) VALUES (1, TRUE, 'Alice', 'large'), (2, TRUE, 'unrequested', 'large')");
  assert.deepEqual((await Policy.objects.using(db).orderBy(Policy.id).all()).map(r => r.name), ['Alice', 'unrequested']);
  const row = await Policy.objects.using(db).only(Policy.display).get(Policy.id.eq(1));
  assert.deepEqual(row.toJSON(), { display: 'Hello, Alice!' });
  assert.equal(row.pk, 1);
  for (const field of ['id', 'name', 'bio']) assert.throws(() => row[field], NotLoaded);
  await row.refresh(); assert.deepEqual(row.toJSON(), { display: 'Hello, Alice!' });
  await row.update({ name: 'Bob' }); assert.deepEqual(row.toJSON(), { display: 'Hello, Bob!' });
  console.log('Node compiled-policy/native-computed shape: passed');
} finally { await db.dropTables(); await db.close(); }
