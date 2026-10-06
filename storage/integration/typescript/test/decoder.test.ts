import test from "node:test";
import assert from "node:assert/strict";
import { Reference } from "@orm/storage";
import { FileField } from "../src/index.js";
import { prepareDecoder } from "../src/decoder.js";

test("public slots only, null loaded, omitted absent", () => {
  const fields = new Map([["file", new FileField("file", "reports", true)], ["hidden", new FileField("hidden", "reports")]]);
  const decode = prepareDecoder(fields, [["id", 2], ["file", 1]]);
  const destination: Record<string, unknown> = { id: 42 };
  decode(["invalid hidden helper", { v: 1, storage: "reports", key: "x" }, 42], 0, destination);
  assert.ok(destination.file instanceof Reference);
  assert.equal(Object.hasOwn(destination, "hidden"), false);
  decode(["helper", null, 42], 0, destination);
  assert.equal(destination.file, null);
  const omitted: Record<string, unknown> = { id: 42 };
  prepareDecoder(fields, [["id", 0]])([42], 0, omitted);
  assert.equal(Object.hasOwn(omitted, "file"), false);
});
