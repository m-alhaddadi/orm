import { writeFileSync } from 'node:fs';
import { performance } from 'node:perf_hooks';
import os from 'node:os';
import { connect, Database } from '../../js/dist/src/index.js';
import { User } from '../../js/dist/test/blog/models.js';
import { lock as baseline } from './baseline.mjs';

const args = process.argv.slice(2);
const option = (name, fallback) => args.includes(name) ? args[args.indexOf(name) + 1] : fallback;
const rounds = Number(option('--rounds', '24'));
const inner = Number(option('--inner', '200'));
const connections = Number(option('--connections', '1'));
const only = option('--only', '').split(',').filter(Boolean);
const db = await connect(process.env.ORM_TEST_DATABASE_URL, { maxConnections: connections });
const other = await connect(db.url, { default: false, maxConnections: 1 });
await db.createTables();
const user = await User.objects.using(db).insert({email:`lockbench-${Date.now()}@x.io`,name:'Bench'});
const candidate = Database.prototype.lock;
const result = { runtime: process.version, platform: `${os.platform()} ${os.arch()}`, rounds, inner, connections, cases: {} };
async function measure(name, fn) {
  if (only.length && !only.includes(name)) return;
  const count = name.startsWith('long/') ? Math.min(inner,40) : inner;
  const samples = { baseline: [], candidate: [], inner: count };
  for (const method of [baseline, candidate]) {
    Database.prototype.lock = method;
    for (let i = 0; i < 30; i++) await fn();
  }
  for (let block = 0; block < rounds; block++) {
    const order = [['baseline', baseline], ['candidate', candidate]];
    if (block % 2) order.reverse();
    for (const [label, method] of order) {
      Database.prototype.lock = method;
      const start = performance.now();
      for (let i = 0; i < count; i++) await fn();
      samples[label].push((performance.now() - start) / count * 1000);
    }
  }
  result.cases[name] = samples;
  const median = a => { const s = [...a].sort((a,b) => a-b); return (s[(s.length-1)>>1]+s[s.length>>1])/2; };
  const b = median(samples.baseline), c = median(samples.candidate);
  console.log(`${name.padEnd(28)} ${b.toFixed(2)} -> ${c.toFixed(2)} us (${((c/b-1)*100).toFixed(1)}%)`);
}
await db.transaction(async () => {
  for (const [name,key] of Object.entries({ integer: 42, bigint: 42n, short: 'import', unicode: 'ورود 🔒', long: 'x'.repeat(4096) })) {
    for (const nowait of [false,true]) for (const exclusive of [true,false]) {
      await measure(`${name}/${nowait}/${exclusive}`, async () => {
        if (await db.lock(key, { nowait, exclusive }) !== true) throw Error('lock not acquired');
      });
    }
  }
  await db.lock('held');
  await other.transaction(async () => {
    await measure('contended', async () => {
      if (await other.lock('held', {nowait:true}) !== false) throw Error('lock acquired');
    });
  });
});
await measure('transaction+short', () => db.transaction(async () => {
  if (await db.lock('import') !== true) throw Error('lock not acquired');
}));
await measure('concurrent transactions', () => Promise.all(Array.from({length:4}, (_,i) => db.transaction(async () => {
  if (await db.lock(i, {nowait:true}) !== true) throw Error('lock not acquired');
}))));
await measure('transaction+read+write', () => db.transaction(async () => {
  if (await db.lock('import') !== true) throw Error('lock not acquired');
  const row = await User.objects.using(db).get(User.id.eq(user.id));
  if (row.email !== user.email) throw Error('wrong row');
  if (await User.objects.using(db).filter(User.id.eq(user.id)).update({name:'Bench'}) !== 1) throw Error('wrong update');
}));
await User.objects.using(db).filter(User.id.eq(user.id)).delete();
Database.prototype.lock = candidate;
result.server = await db.fetchText('SELECT version()');
writeFileSync(option('--out', '/tmp/locks-node.json'), JSON.stringify(result, null, 2)+'\n');
await other.close();
await db.close();
