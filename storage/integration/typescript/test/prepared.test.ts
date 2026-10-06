import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { LocalStorage, Registry, Reference } from "@orm/storage";
import { Upload, FileField, PreparedFileWrite, FileWriteError, FileFieldError, FileNotLoaded, MissingFile } from "../src/index.js";

async function environment(run: (registry: Registry, fields: Map<string, FileField>, root: string) => Promise<void>) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "file-write-")));
  const registry = new Registry(new Map([["reports", new LocalStorage("reports", root)]]));
  try { await run(registry, new Map([["file", new FileField("file", "reports", true)]]), root); }
  finally { await rm(root, { recursive: true, force: true }); }
}
test("SQL failure and retry retain one completed upload", () => environment(async (registry, fields, root) => {
  const upload = new Upload(Buffer.from("report"), { filename: "report.xlsx" });
  assert.equal((await readdir(root)).length, 0);
  const operation = new PreparedFileWrite({ file: upload }, fields, registry);
  await assert.rejects(operation.execute(async () => { throw new Error("uncertain SQL acknowledgement"); }), error => {
    assert.ok(error instanceof FileWriteError); assert.equal(error.operation, operation); return true;
  });
  assert.equal(operation.references.size, 1);
  const json = await operation.execute(async values => values.file);
  assert.deepEqual(Reference.fromJSON(json), operation.references.get("file"));
  assert.equal((await readdir(root)).length, 1);
}));
test("unsupported shape rejects before upload; references retain normal write semantics", () => environment(async (registry, fields, root) => {
  const ref = await registry.resolve(new Reference({ v: 1, storage: "reports", key: "lookup" })).upload(Buffer.from("report"));
  for (const shape of ["bulk", "multi_update", "upsert", "ignore", "expression"]) {
    assert.throws(() => new PreparedFileWrite({ file: new Upload(Buffer.from("new")) }, fields, registry, { shape }), FileFieldError);
    assert.deepEqual((await new PreparedFileWrite({ file: ref }, fields, registry, { shape }).prepare()).file, ref.toJSON());
  }
  assert.throws(() => new PreparedFileWrite({ file: new Upload(Buffer.from("valid")), other: new Upload(Buffer.from("invalid")) }, fields, registry));
  assert.equal((await readdir(root)).length, 1);
}));
test("null and unloaded operations fail before provider access", () => environment(async (registry, fields, root) => {
  const field = fields.get("file")!;
  await assert.rejects(field.signedUrl({}, registry), FileNotLoaded);
  assert.throws(() => field.open({ file: null }, registry), MissingFile);
  assert.equal(field.decode(null), null);
  assert.equal((await readdir(root)).length, 0);
}));
test("partial failure is terminal and retains completed references", () => environment(async (registry, _fields, root) => {
  const fields = new Map([["first", new FileField("first", "reports")], ["second", new FileField("second", "reports")]]);
  async function* fail() { throw new Error("source failed"); yield Buffer.from("unused"); }
  const operation = new PreparedFileWrite({ first: new Upload(Buffer.from("first")), second: new Upload(fail()) }, fields, registry);
  await assert.rejects(operation.prepare(), FileWriteError);
  assert.deepEqual([...operation.references.keys()], ["first"]);
  await assert.rejects(operation.prepare(), FileFieldError);
  assert.equal((await readdir(root)).length, 1);
}));
