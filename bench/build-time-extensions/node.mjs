/** Paired old/new whole-package benchmark, including each TypeScript frontend. */
import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { performance } from 'node:perf_hooks';
import { createRequire } from 'node:module';

const { values: args } = parseArgs({ options: {
  before: { type: 'string' }, after: { type: 'string' }, out: { type: 'string' },
  url: { type: 'string' }, batches: { type: 'string', default: '24' },
  iterations: { type: 'string', default: '100' },
  'native-profile': { type:'boolean', default:false },
} });
const source = `datasource db {\n provider = "${args.url.startsWith('sqlite:') ? 'sqlite' : 'postgresql'}"\n}\nmodel BenchItem {\n id BigInt @id @default(autoincrement())\n label String\n number Int @default(0)\n data Json?\n}`;
const separator = args['native-profile'] ? 'x' : '-';
const seedLabel = (side,index='') => `seed${separator}${side}${separator}${index}`;
const writeLabel = side => `write${separator}${side}`;
const modules = [], natives = [], registries = [], models = [], dbs = [], seeds = [];
const require = createRequire(import.meta.url);
for (const root of [args.before, args.after]) {
  process.env.ORM_NATIVE = `${root}/js/orm.node`;
  const mod = await import(pathToFileURL(`${root}/js/dist/src/index.js`).href);
  const reg = new mod.Registry();
  const model = mod.loads(source, { registry: reg }).BenchItem;
  const addon = require(process.env.ORM_NATIVE);
  modules.push(mod); natives.push(addon); registries.push(reg); models.push(model);
  const db = await mod.connect(args.url, { registry: reg, default: false, maxConnections: 4 });
  await db.createTables(); dbs.push(db);
}
for (const side of [0, 1]) {
  seeds.push(await models[side].objects.using(dbs[side]).insertMany(Array.from({ length: 1000 }, (_, i) => ({ label: seedLabel(side,i), number: i, data: { n: i } }))));
}
const query = (side, size) => {
  const m = models[side];
  return m.objects.using(dbs[side]).filter(m.label.startsWith(seedLabel(side))).orderBy(m.number).limit(size);
};
const nativeSchemas = registries.map(r => r.native());
const op = JSON.stringify({ op: 'select', model: 'BenchItem', filters: [], limit: 1 });
assert.equal(nativeSchemas[0].sql(op, []), nativeSchemas[1].sql(op, []));
const cases = {};
let sink;
for (const name of ['definition', 'construct', 'plan-native', 'construct+sql', 'read-1', 'read-50', 'read-1000', 'projection', 'update', 'bulk-update-50', 'insert', 'bulk-insert-50']) {
  const sync = ['definition', 'construct', 'plan-native', 'construct+sql'].includes(name);
  const synchronous = (side) => {
    if (name === 'definition') {
      const mod = modules[side], reg = new mod.Registry();
      mod.loads(source, { registry: reg });
      return reg.native();
    }
    if (name === 'construct') return query(side, 50);
    if (name === 'plan-native') return nativeSchemas[side].sql(op, []);
    return query(side, 50).sql();
  };
  const asynchronous = async side => {
    const m = models[side], db = dbs[side];
    if (name.startsWith('read-')) {
      const rows = await query(side, Number(name.split('-')[1])).all();
      assert.equal(rows.length, Number(name.split('-')[1]));
      assert.equal(rows.at(-1).number, rows.length - 1);
      assert.deepEqual(rows.at(-1).data, { n: rows.length - 1 });
      if (args['native-profile']) assert.equal(rows.at(-1).display, `Hello, ${rows.at(-1).label}!`);
      return rows;
    }
    if (name === 'projection') {
      const rows = await query(side, 50).select({ number: m.number, data: m.data }).all();
      assert.equal(rows.length, 50); assert.deepEqual(rows.at(-1), { number: 49, data: { n: 49 } });
      return rows;
    }
    if (name === 'update') {
      const n = await m.objects.using(db).filter(m.id.eq(seeds[side][0].id)).update({ label: seedLabel(side,0) });
      assert.equal(n, 1); return n;
    }
    if (name === 'bulk-update-50') {
      const n = await m.objects.using(db).updateMany(seeds[side].slice(0, 50).map((r, i) => ({ id: r.id, label: seedLabel(side,i) })));
      assert.equal(n, 50); return n;
    }
    const rows = await m.objects.using(db).insertMany(Array.from({ length: name === 'bulk-insert-50' ? 50 : 1 }, (_, i) => ({ label: writeLabel(side), number: i, data: { n: i } })));
    assert.equal(rows.length, name === 'bulk-insert-50' ? 50 : 1);
    assert.deepEqual(rows.at(-1).data, { n: rows.length - 1 });
    return rows;
  };
  const cleanup = async side => {
    if (['insert', 'bulk-insert-50'].includes(name)) await models[side].objects.using(dbs[side]).filter(models[side].label.eq(writeLabel(side))).delete();
  };
  for (const side of [0, 1]) {
    for (let i = 0; i < (sync && name !== "definition" ? 20000 : 30); i++) sink = sync ? synchronous(side) : await asynchronous(side);
    await cleanup(side);
  }
  const samples = [[], []];
  const n = name === "definition" ? Math.max(10, Number(args.iterations) / 5) : sync ? Number(args.iterations) * 5 : ['read-1000', 'bulk-insert-50', 'bulk-update-50'].includes(name) ? Math.max(10, Number(args.iterations) / 5) : Number(args.iterations);
  for (let batch = 0; batch < Number(args.batches); batch++) {
    for (const side of batch % 2 === 0 ? [0, 1] : [1, 0]) {
      global.gc?.();
      const start = performance.now();
      for (let i = 0; i < n; i++) sink = sync ? synchronous(side) : await asynchronous(side);
      samples[side].push((performance.now() - start) * 1000 / n);
      await cleanup(side);
    }
  }
  cases[name] = { baseline: samples[0], candidate: samples[1], iterations: n };
  console.log(name);
}
globalThis.__benchmarkSink = sink;
writeFileSync(args.out, JSON.stringify({ runtime: process.version, platform: `${process.platform}/${process.arch}`, url: args.url, batches: Number(args.batches), warmupCpu: 20000, resultsRetained: true, cases }, null, 2));
for (const side of [0, 1]) {
  const m = models[side], db = dbs[side];
  await m.objects.using(db).filter(m.label.startsWith(seedLabel(side))).delete();
  await db.close();
}
