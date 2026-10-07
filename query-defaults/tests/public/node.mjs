// `@@query.defaults` from the schema language to a query; needs a query-defaults artifact.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { connect, loads, NotLoaded, Registry } from '../../../js/dist/src/index.js';
import { native } from '../../../js/dist/src/native.js';

const fixture = readFileSync(new URL('../fixtures/schema.prisma', import.meta.url), 'utf8');
assert.ok(JSON.parse(native().nativeArtifact()).capabilities.includes('query-defaults'), 'select orm-query-defaults in the build');

async function run(provider, url) {
  const registry = new Registry();
  const { Policy } = loads(fixture.replace('"sqlite"', `"${provider}"`), { registry });
  const db = await connect(url, { registry, default: false });
  await db.dropTables();
  await db.createTables();
  try {
    await Policy.objects.using(db).insertMany([{ name: 'Alice', bio: 'large' }, { name: 'hidden', active: false, bio: 'large' }]);
    assert.equal(await Policy.objects.using(db).count(), 1);
    assert.equal(await Policy.objects.using(db).withoutDefaults().count(), 2);
    const row = await Policy.objects.using(db).get();
    assert.equal(row.name, 'Alice');
    assert.throws(() => row.bio, NotLoaded);
    const full = await Policy.objects.using(db).withoutDefaults().get(Policy.name.eq('hidden'));
    assert.equal(full.bio, 'large');
    assert.equal(full.active, false);
  } finally {
    await db.dropTables();
    await db.close();
  }
}

await run('sqlite', 'sqlite://:memory:');
if (process.env.ORM_TEST_DATABASE_URL) await run('postgresql', process.env.ORM_TEST_DATABASE_URL);
console.log('Node compiled query defaults: passed');
