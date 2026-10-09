import assert from "node:assert/strict";
import { connect, define, loads, Registry } from "../dist/src/index.js";
import { native } from "../dist/src/native.js";
const expected = process.argv[2] === "enabled";
assert.equal(JSON.parse(native().nativeArtifact()).capabilities.includes("reference-loading"), expected);
const registry = new Registry();
const m = loads(`datasource db {\n provider = "sqlite"\n}
model Parent {\n id Int @id\n children Child[]\n}
model Child {\n id Int @id\n parent_id Int\n parent Parent @relation(fields: [parent_id], references: [id])\n}`, { registry });
const db = await connect("sqlite://:memory:", { registry, default: false });
try {
  await db.createTables();
  await m.Parent.objects.using(db).insert({ id: 1 });
  const child = await m.Child.objects.using(db).insert({ id: 1, parentId: 1 });
  assert.equal(typeof child.loadParent === "function", expected);
  await child.update({ parentId: 1 });
  await child.refresh();
  if (expected) assert.equal((await child.loadParent()).id, 1);
  else {
    assert.ok(!Object.getOwnPropertySymbols(child).some((symbol) => symbol.description === "orm.referenceState"));
    const joined = await m.Child.objects.using(db).load(m.Child.parent).get();
    assert.equal(joined.parent.id, 1);
    assert.throws(() => define({ models: [] }, { requiredCapabilities: ["reference-loading"] }), /rebuild/);
  }
} finally { await db.close(); }
console.log(`reference profile ${process.argv[2]} verified`);
