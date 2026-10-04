/** Paired public TypeScript benchmark with two release addons. */
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { writeFileSync } from 'node:fs';
import { parseArgs } from 'node:util';
import { performance } from 'node:perf_hooks';
import { Database, Decimal, define, Registry, func, window, type Expression } from '../../js/src/index.js';
import { sqliteRegistry, Book as TypedBook, Author as TypedAuthor } from '../../js/test/sqlite/models.js';
import type { NativeEngine, NativeSchema } from '../../js/src/native.js';

const { values: args } = parseArgs({ options: {
  before: { type: 'string' }, after: { type: 'string' }, out: { type: 'string' },
  url: { type: 'string', default: 'sqlite://:memory:' },
  batches: { type: 'string', default: '24' }, iterations: { type: 'string', default: '100' },
}});
const require = createRequire(import.meta.url);
const reg = new Registry();
const ir = { ...sqliteRegistry.ir(), dialect: args.url!.startsWith('sqlite:') ? 'sqlite' as const : 'postgres' as const };
const models = define(ir, { registry: reg });
const Book = models['Book'] as unknown as typeof TypedBook;
const Author = models['Author'] as unknown as typeof TypedAuthor;
const dbs: Database[] = [];
for (const path of [args.before!, args.after!]) {
  const native = require(path) as { Schema: new (s: string) => NativeSchema; connect: (url: string, s: NativeSchema, n: number, disable: string[]) => Promise<NativeEngine>; setDecimalClass: (c: unknown) => void };
  native.setDecimalClass(Decimal);
  dbs.push(new Database(await native.connect(args.url!, new native.Schema(JSON.stringify(reg.ir())), 4, []), args.url!, reg));
}
await dbs[0]!.createTables();
if (args.url!.startsWith('sqlite:')) await dbs[1]!.createTables();
const authors: bigint[] = [];
for (const db of args.url!.startsWith('sqlite:') ? dbs : dbs.slice(0, 1)) {
  const a = await Author.objects.using(db).insert({ email: `bench-${process.pid}-${Date.now()}@x`, name: 'bench' });
  await Book.objects.using(db).insertMany(Array.from({ length: 1000 }, (_, i) => ({ authorId: a.id, title: `b${i}`, pages: i })));
  authors.push(a.id);
}
if (authors.length === 1) authors.push(authors[0]!);

function query(db: Database, aid: bigint, name: string) {
  const q = Book.objects.using(db).filter(Book.authorId.eq(aid));
  if (name.startsWith('read-')) return q.orderBy(Book.pages).limit(Number(name.split('-')[1]));
  if (name.startsWith('nested-')) {
    let c = q.filter(Book.pages.lt(8)).cte('base' as string);
    for (let i = 0; i < Number(name.split('-')[1]); i++) c = Book.objects.using(db).from(c).filter(c.c.pages.gte(0)).cte(`d${i}` as string);
    return Book.objects.using(db).from(c).orderBy(Book.pages);
  }
  if (name === 'shared') {
    const c = q.filter(Book.pages.lt(8)).cte('base');
    const left = Book.objects.using(db).from(c).filter(c.c.pages.gte(0)).cte('left_part');
    const right = Book.objects.using(db).from(c).filter(c.c.pages.lte(7)).cte('right_part');
    return Book.objects.using(db).from(left).join(right, right.c.id.eq(Book.id)).orderBy(Book.pages);
  }
  if (name === 'recursive') {
    const c = q.filter(Book.pages.eq(0)).cte('chain', { recursive: c => q.filter(Book.pages.eq(c.c.pages.add(1)), Book.pages.lt(8)) });
    return Book.objects.using(db).from(c).orderBy(Book.pages);
  }
  if (name.startsWith('window-')) {
    const w = window({ orderBy: Book.pages, rows: [null, 0] });
    const items: Record<string, Expression<unknown, 'Book', unknown>> = { pages: Book.pages };
    for (let i = 0; i < Number(name.split('-')[1]); i++) items[`w${i}`] = func.sum(Book.pages).over(w);
    return q.filter(Book.pages.lt(8)).select(items);
  }
  if (name === 'projection') return q.filter(Book.pages.lt(50)).select({ title: Book.title, pages: Book.pages }).orderBy(Book.pages);
  throw new Error(name);
}

const cases: Record<string, { baseline: number[]; candidate: number[] }> = {};
for (const name of ['read-1', 'read-50', 'read-1000', 'projection', 'nested-4', 'nested-16', 'shared', 'recursive', 'window-3', 'window-16', 'window-64'].filter(c => !args.url!.startsWith('sqlite:') || !c.startsWith('window-'))) {
  const operation = (side: number) => query(dbs[side]!, authors[side]!, name).all();
  const normalize = (rows: unknown[]) => rows.map(r => {
    const o = r as Record<string, unknown>;
    return name.startsWith('read-') || ['shared', 'recursive'].includes(name) || name.startsWith('nested-') ? [o['title'], o['pages'], o['status'], o['metadata']] : Object.values(o);
  });
  assert.deepEqual(normalize(await operation(0)), normalize(await operation(1)), name);
  const expected = name.startsWith('read-') ? Number(name.split('-')[1]) : name === 'projection' ? 50 : 8;
  assert.equal((await operation(0)).length, expected, name);
  for (const side of [0, 1]) for (let i = 0; i < 30; i++) await operation(side);
  const samples: [number[], number[]] = [[], []];
  const n = name === 'read-1000' ? Math.max(10, Number(args.iterations) / 5) : Number(args.iterations);
  for (let batch = 0; batch < Number(args.batches); batch++) {
    for (const side of batch % 2 === 0 ? [0, 1] : [1, 0]) {
      const start = performance.now();
      for (let i = 0; i < n; i++) await operation(side);
      samples[side]!.push((performance.now() - start) * 1000 / n);
    }
  }
  cases[name] = { baseline: samples[0], candidate: samples[1] };
  console.log(name);
}
writeFileSync(args.out!, JSON.stringify({ runtime: process.version, platform: process.platform + '/' + process.arch, url: args.url, batches: args.batches, iterations: args.iterations, cases }, null, 2));
for (let side = 0; side < 2; side++) {
  if (side === 0 || args.url!.startsWith('sqlite:')) await Author.objects.using(dbs[side]!).filter(Author.id.eq(authors[side]!)).delete();
  await dbs[side]!.close();
}
