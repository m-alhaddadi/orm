import assert from "node:assert/strict";
import { test } from "node:test";
import { Registry, define, loads, SchemaError } from "../src/index.js";

const good = `model User {
  id Int @id
  name String
}`;

test("definition prepares atomically and old models retain their snapshot", () => {
  const registry = new Registry();
  const User = loads(good, { registry })["User"]!;
  const prepared = registry.native();
  const before = registry.ir();
  assert.throws(() => define({ models: [{ name: "Bad", table: "bad", fields: [
    { name: "id", column: "id", type: "int", primary_key: true },
  ], relations: [{ name: "missing", kind: "one", target: "Missing", from: "id", to: "id" }] }] }, { registry }), SchemaError);
  assert.deepEqual(registry.ir(), before);
  assert.equal(registry.native(), prepared);
  loads(`model Other {\n id Int @id\n}`, { registry });
  assert.equal(User._meta.registry.native(), prepared);
  assert.notEqual(registry.native(), prepared);
});
