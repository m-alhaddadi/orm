import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { test } from "node:test";

const nativeUrl = new URL("../src/native.js", import.meta.url).href;
test("native profiles reject invalid selections and compatibility before use", () => {
  const artifacts = fileURLToPath(new URL("../../target/", import.meta.url));
  mkdirSync(artifacts, { recursive: true });
  const directory = mkdtempSync(join(artifacts, "orm-profile-"));
  const path = join(directory, "mock.cjs");
  const build = { features: ["sqlite"], rustc: "rustc 1.0.0", target: "t", profile: "release", revision: "r" };
  const metadata = { abi: 1, version: "0.1.0", language: "node", profile: "sqlite", backends: ["sqlite"], adapters: [], build,
    capabilities: { cli: false, "generate-python": false, "generate-typescript": false, composition: false } };
  const run = (profile: string) => execFileSync(process.execPath,
    ["--input-type=module", "-e", `const { native } = await import(${JSON.stringify(nativeUrl)}); native();`],
    { env: { ...process.env, ORM_NATIVE: path, ORM_PROFILE: profile }, stdio: "pipe" });
  try {
    writeFileSync(path, `module.exports={setDecimalClass(){},profileMetadata(){return ${JSON.stringify(JSON.stringify(metadata))}}};`);
    run("sqlite");
    for (const selector of ["unknown", "__proto__", "constructor", "toString"]) {
      assert.throws(() => run(selector), /unknown ORM_PROFILE/);
    }
    assert.throws(() => run("postgres"), /incompatible orm native profile/);
    writeFileSync(path, `module.exports={profileMetadata(){return ${JSON.stringify(JSON.stringify({ ...metadata, abi: 2 }))}}};`);
    assert.throws(() => run("sqlite"), /incompatible orm native artifact/);
    for (const invalid of [null, [], { ...metadata, capabilities: [] }, { ...metadata, build: undefined },
      { ...metadata, build: { ...build, features: "sqlite" } }, { ...metadata, build: { ...build, revision: null } },
      { ...metadata, adapters: ["reference-loading", "reference-loading"],
        capabilities: { "reference-loading": true } }]) {
      writeFileSync(path, `module.exports={profileMetadata(){return ${JSON.stringify(JSON.stringify(invalid))}}};`);
      assert.throws(() => run("sqlite"), /incompatible orm native artifact/);
    }
  } finally { rmSync(directory, { recursive: true, force: true }); }
});

test("the loaded native artifact embeds its build record", async () => {
  const { native } = await import(nativeUrl);
  const metadata = JSON.parse(native().profileMetadata());
  const build = metadata.build;
  for (const backend of metadata.backends) assert.ok(build.features.includes(backend), backend);
  assert.match(build.rustc, /^rustc /);
  assert.ok(build.target && build.revision);
});

test("the loaded native artifact reports no adapters", async () => {
  const { native, nativeAdapters } = await import(nativeUrl);
  assert.deepEqual(JSON.parse(native().profileMetadata()).adapters, []);
  assert.deepEqual(nativeAdapters(), []);
});
