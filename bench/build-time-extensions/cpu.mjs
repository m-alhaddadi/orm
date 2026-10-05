/** Focused diagnostic: identical query input, reversed module loading, A/A support. */
import { parseArgs } from 'node:util';
import { pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';
import { writeFileSync } from 'node:fs';

const { values: args } = parseArgs({ options: {
  before: { type: 'string' }, after: { type: 'string' }, out: { type: 'string' },
  reverse: { type: 'boolean', default: false }, database: { type: 'boolean', default: false },
  sideLabel: { type: 'boolean', default: false }, discard: { type: 'boolean', default: false },
  warmup: { type: 'string', default: '20000' }, iterations: { type: 'string' }, batches: { type: 'string', default: '40' },
} });
const roots = [args.before, args.after], models = [], schemas = [], dbs = [];
const source = `datasource db {\n provider = "sqlite"\n}\nmodel BenchItem {\n id BigInt @id @default(autoincrement())\n label String\n number Int @default(0)\n data Json?\n}`;
for (const side of args.reverse ? [1, 0] : [0, 1]) {
  process.env.ORM_NATIVE = `${roots[side]}/js/orm.node`;
  const module = await import(pathToFileURL(`${roots[side]}/js/dist/src/index.js`).href);
  const registry = new module.Registry();
  models[side] = module.loads(source, { registry }).BenchItem;
  schemas[side] = registry.native();
  if (args.database) dbs[side] = await module.connect('sqlite://:memory:', { registry, default: false });
}
const query = side => {
  const m = models[side];
  const q = args.database ? m.objects.using(dbs[side]) : m.objects;
  return q.filter(m.label.startsWith(args.sideLabel ? `seed-${side}-` : 'seed-')).orderBy(m.number).limit(50);
};
const op = JSON.stringify({ op: 'select', model: 'BenchItem', limit: 50 });
const results = {};
let sink;
for (const name of ['construct', 'plan-native', 'construct+sql']) {
  const operation = side => name === 'construct' ? query(side) : name === 'plan-native' ? schemas[side].sql(op, []) : query(side).sql();
  for (let i = 0; i < Number(args.warmup); i++) { sink = operation(0); sink = operation(1); }
  const samples = [[], []], n = args.iterations ? Number(args.iterations) : name === 'construct' ? 50000 : 10000;
  for (let batch = 0; batch < Number(args.batches); batch++) {
    for (const side of batch % 2 ? [1, 0] : [0, 1]) {
      global.gc?.();
      const start = performance.now();
      for (let i = 0; i < n; i++) { if (args.discard) operation(side); else sink = operation(side); }
      samples[side].push((performance.now() - start) * 1000 / n);
    }
  }
  results[name] = { baseline: samples[0], candidate: samples[1] };
}
globalThis.__benchmarkSink = sink;
writeFileSync(args.out, JSON.stringify({ options: args, runtime: process.version, cases: results }, null, 2));

for (const db of dbs) if (db) await db.close();
