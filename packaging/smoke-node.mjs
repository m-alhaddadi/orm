import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import { connect, loads, Registry } from "orm";
const require = createRequire(import.meta.url);
const { native, nativeAdapters } = await import(new URL("./native.js", pathToFileURL(require.resolve("orm"))));
const addon = native();
const metadata = JSON.parse(addon.profileMetadata());
assert.equal(metadata.profile, process.env.ORM_PROFILE);
assert.deepEqual(nativeAdapters(), []);
const selector = process.env.ORM_PROFILE;
process.env.ORM_PROFILE = "invalid-after-initialization";
assert.equal(native(), addon);
process.env.ORM_PROFILE = selector;
assert.equal(typeof addon.cli === "function", metadata.capabilities.cli);
assert.equal(typeof addon.generateTypescript === "function", metadata.capabilities["generate-typescript"]);
const source = backend => `datasource db { provider = "${backend}" }
model Probe {
  id BigInt @id @default(autoincrement())
  name String @unique
}`;
for (const backend of ["postgres", "sqlite"]) {
  const provider = backend === "postgres" ? "postgresql" : backend;
  const registry = new Registry();
  if (!metadata.backends.includes(backend)) {
    assert.throws(() => loads(source(provider), { registry }), /not compiled/);
    continue;
  }
  if (backend === "postgres" && !process.env.ORM_TEST_DATABASE_URL) continue;
  const { Probe } = loads(source(provider), { registry });
  const db = await connect(backend === "postgres" ? process.env.ORM_TEST_DATABASE_URL : "sqlite://:memory:", { registry, default: false });
  try {
    await db.createTables();
    const first = await Probe.objects.using(db).insert({ name: "first" });
    assert.equal((await Probe.objects.using(db).get(Probe.id.eq(first.id))).name, "first");
    await db.transaction(async () => { await Probe.objects.using(db).insert({ name: "second" }); });
    assert.equal(await Probe.objects.using(db).count(), 2);
    await first.update({ name: "changed" });
    assert.equal((await Probe.objects.using(db).get(Probe.id.eq(first.id))).name, "changed");
    await db.dropTables();
  } finally { await db.close(); }
}
console.log(JSON.stringify(metadata));
