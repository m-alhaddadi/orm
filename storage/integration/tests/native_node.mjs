// Public tests against this worktree's selected native addon only.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, realpath } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { connect, define, Registry as ModelRegistry } from '../../../js/dist/src/index.js';
import { LocalStorage, Reference, Registry } from '../../typescript/dist/src/index.js';
import { Upload, FileField, FileWriteError, FileFieldError } from '../typescript/dist/src/index.js';
import { installModel } from '../typescript/dist/src/model.js';

class CountingStorage extends LocalStorage {
  uploads = 0; deletes = 0;
  upload(...args) { this.uploads++; return super.upload(...args); }
  delete(...args) { this.deletes++; return super.delete(...args); }
}
async function fixture(run) {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'orm-file07-')));
  const storage = new CountingStorage('reports', root);
  const providers = new Registry(new Map([['reports', storage]]));
  const url = process.env.FILE_STORAGE_DATABASE_URL ?? 'sqlite://:memory:';
  const ir = {
    dialect: url.startsWith('sqlite:') ? 'sqlite' : 'postgres',
    models: [{ name: 'FileReport07', table: 'file_reports_07_node', fields: [
      { name: 'id', column: 'id', type: 'int', primary_key: true },
      { name: 'file', column: 'file', type: 'json', nullable: true }] }],
    behavior: { schema_contract: 1,
      field_adapters: [{ model: 'FileReport07', field: 'file', adapter: 'file-storage.reference.v1' }],
      file_fields: [{ model: 'FileReport07', field: 'file', storage: 'reports', reference_contract: 1 }] },
  };
  const registry = new ModelRegistry();
  const Report = define(ir, { registry }).FileReport07;
  installModel(Report._meta.Row.prototype, new Map([['file', new FileField('file', 'reports', true)]]), providers, Report._meta);
  const db = await connect(url, { registry });
  try { await db.dropTables(); await db.createTables(); await run({ Report, db, storage }); }
  finally { await db.dropTables(); await db.close(); await rm(root, { recursive: true, force: true }); }
}
async function bytes(stream) { const chunks = []; for await (const chunk of stream) chunks.push(chunk); return Buffer.concat(chunks); }

test('reference writes, shared objects, null and explicit opening', () => fixture(async ({ Report, storage }) => {
  const ref = await storage.upload(Buffer.from('report'));
  const row = await Report.objects.insert({ id: 1, file: ref });
  await Report.objects.insertMany([{ id: 2, file: ref }, { id: 3, file: null }]);
  assert.equal(storage.uploads, 1);
  assert.ok(row.file instanceof Reference);
  const rows = await Report.objects.orderBy(Report.id);
  assert.deepEqual([rows[0].file.toJSON(), rows[2].file], [ref.toJSON(), null]);
  assert.equal((await bytes(row.fileOpen())).toString(), 'report');
  await Report.objects.filter(Report.id.eq(1)).update({ file: null });
  await Report.objects.filter(Report.id.eq(2)).delete();
  assert.equal(storage.deletes, 0);
  assert.equal((await bytes(storage.open(ref))).toString(), 'report');
}));

test('unsupported shapes and invalid ordinary values reject before upload', () => fixture(async ({ Report, storage }) => {
  for (const run of [() => Report.objects.insertMany([{ id: 1, file: new Upload(Buffer.from('x')) }]),
    () => Report.objects.update({ file: new Upload(Buffer.from('x')) }),
    () => Report.objects.insert({ id: 1, file: new Upload(Buffer.from('x')) }).onConflict(Report.id, { update: false }),
    () => Report.objects.insert({ id: 'invalid', file: new Upload(Buffer.from('x')) })]) {
    await assert.rejects(async () => run());
  }
  assert.equal(storage.uploads, 0);
}));

test('SQL failure exposes reference and explicit retry never uploads again', () => fixture(async ({ Report, storage }) => {
  await Report.objects.insert({ id: 1, file: null });
  let operation;
  try { await Report.objects.insert({ id: 1, file: new Upload(Buffer.from('generated')) }); }
  catch (error) { assert.ok(error instanceof FileWriteError); operation = error.operation; }
  assert.ok(operation);
  const reference = operation.references.get('file');
  await operation.execute(data => Report.objects.insert({ ...data, id: 2 }));
  assert.equal(storage.uploads, 1);
  await Report.objects.filter(Report.id.eq(2)).update({ file: new Upload(Buffer.from('replacement')) });
  assert.equal(storage.uploads, 2);
  assert.equal(storage.deletes, 0);
  assert.equal((await bytes(storage.open(reference))).toString(), 'generated');
}));

test('outer rollback and savepoint retain successfully uploaded references', () => fixture(async ({ Report, db, storage }) => {
  const retained = Report.objects.prepareFileInsert({ id: 1, file: new Upload(Buffer.from('rollback')) });
  await assert.rejects(db.transaction(async () => { await retained.execute(); throw new Error('outer rollback'); }));
  assert.equal(await Report.objects.count(), 0);
  const ref = retained.operation.references.get('file');
  assert.equal((await bytes(storage.open(ref))).toString(), 'rollback');
  let nested;
  await db.transaction(async () => {
    await Report.objects.insert({ id: 2, file: ref });
    nested = Report.objects.prepareFileInsert({ id: 3, file: new Upload(Buffer.from('savepoint')) });
    await assert.rejects(db.transaction(async () => { await nested.execute(); throw new Error('savepoint rollback'); }));
  });
  assert.equal(await Report.objects.count(), 1);
  assert.equal(storage.deletes, 0);
  assert.equal((await bytes(storage.open(nested.operation.references.get('file')))).toString(), 'savepoint');
}));
