import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readdir, rm, symlink, writeFile, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readFile } from "node:fs/promises";
import { Reference, LocalStorage, CapabilityError, SizeLimitError, StorageError, CHUNK_SIZE, Registry, UploadError } from "../src/index.js";
import { S3Storage } from "../src/s3.js";
import { S3Client } from "@aws-sdk/client-s3";

async function withLocal(run: (storage: LocalStorage, root: string) => Promise<void>) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "storage-test-")));
  try { await run(new LocalStorage("local", root), root); }
  finally { await rm(root, { recursive: true, force: true }); }
}
async function collect(source: AsyncIterable<Uint8Array>): Promise<Buffer> {
  const chunks: Uint8Array[] = []; for await (const chunk of source) chunks.push(chunk); return Buffer.concat(chunks);
}
test("shared wire vectors and invalid values", async () => {
  const vectors = JSON.parse(await readFile(new URL("../../../fixtures/references.json", import.meta.url), "utf8"));
  for (const data of vectors) assert.deepEqual(Reference.fromJSON(data).toJSON(), data);
  for (const extra of [{ v: 2 }, { size: -1 }, { size: true }, { size: 2 ** 53 }, { version: null }, { storage: "" }, { secret: "no" }, { key: "\ud800" }]) {
    assert.throws(() => Reference.fromJSON({ v: 1, storage: "local", key: "key", ...extra }));
  }
});
test("local streaming, references, capabilities and explicit deletion", () => withLocal(async (storage, root) => {
  async function* source() { for (let i = 0; i < 200; i++) yield Buffer.alloc(CHUNK_SIZE, 120); }
  const ref = await storage.upload(source(), { filename: "report.xlsx" });
  assert.equal(ref.size, 200 * CHUNK_SIZE);
  for await (const chunk of storage.open(ref)) assert.ok(chunk.length <= CHUNK_SIZE);
  assert.equal((await collect(storage.open(ref))).length, ref.size);
  assert.deepEqual(Reference.fromJSON(JSON.stringify(ref)), ref);
  assert.equal(new Registry(new Map([["local", storage]])).resolve(ref), storage);
  await assert.rejects(storage.signedUrl(ref), CapabilityError);
  await assert.rejects(storage.delete(new Reference({ v: 1, storage: "local", key: "../escape" })), StorageError);
  await assert.rejects(storage.upload(source(), { maxSize: 1 }), SizeLimitError);
  assert.equal((await readdir(root)).length, 1);
  const controller = new AbortController();
  async function* cancel() { yield Buffer.from("partial"); controller.abort(); yield Buffer.from("more"); }
  await assert.rejects(storage.upload(cancel(), { signal: controller.signal }), { name: "AbortError" });
  assert.equal((await readdir(root)).length, 1);
  await storage.delete(ref); await storage.delete(ref);
  assert.equal((await readdir(root)).length, 0);
}));
test("local signer and symlink defense", () => withLocal(async (storage, root) => {
  const ref = await storage.upload(Buffer.from("report"));
  const signer = new LocalStorage("local", root, async (r, expiry) => `https://files/${r.key}?ttl=${expiry}`);
  assert.match(await signer.signedUrl(ref), /ttl=300$/);
  await assert.rejects(signer.signedUrl(ref, { expiresIn: 0 }), RangeError);
  await writeFile(join(root, "secret"), "secret"); await symlink(join(root, "secret"), join(root, "link"));
  await assert.rejects(collect(storage.open(new Reference({ v: 1, storage: "local", key: "link" }))));
}));
class FakeS3 {
  calls: { name: string; input: any }[] = [];
  fail?: string;
  async send(command: any) {
    const name = command.constructor.name;
    // Copy buffers, as an actual HTTP transport finishes reading before send resolves.
    this.calls.push({ name, input: { ...command.input, ...(command.input.Body ? { Body: Buffer.from(command.input.Body) } : {}) } });
    if (name === this.fail) throw new Error(name);
    return { UploadId: "upload", ETag: "tag", VersionId: "version" };
  }
  client(): S3Client { return this as unknown as S3Client; }
}
test("S3 streaming multipart/versioned delegation", async () => {
  const client = new FakeS3(); const storage = new S3Storage("s3", "bucket", client.client());
  async function* source() { for (let i = 0; i < 100; i++) yield Buffer.alloc(CHUNK_SIZE); }
  const ref = await storage.upload(source());
  assert.equal(ref.size, 100 * CHUNK_SIZE); assert.equal(ref.version, "version");
  const parts = client.calls.filter(c => c.name === "UploadPartCommand");
  assert.equal(parts[0].input.Body.length, 5 * 1024 * 1024); assert.ok(parts[1].input.Body.length < 5 * 1024 * 1024);
  await storage.delete(ref); assert.equal(client.calls.at(-1)!.input.VersionId, "version");
});
test("S3 failure and cancellation abort only unfinished multipart", async () => {
  for (const fail of ["UploadPartCommand", "CompleteMultipartUploadCommand"]) {
    const client = new FakeS3(); client.fail = fail;
    await assert.rejects(new S3Storage("s3", "bucket", client.client()).upload(Buffer.from("report")));
    assert.equal(client.calls.at(-1)!.name, "AbortMultipartUploadCommand");
    assert.ok(!client.calls.some(c => c.name === "DeleteObjectCommand"));
  }
  const client = new FakeS3();
  await assert.rejects(new S3Storage("s3", "bucket", client.client()).upload(Buffer.from("large"), { maxSize: 1 }), error => {
    assert.ok(error instanceof UploadError); assert.ok(error.cause instanceof SizeLimitError);
    assert.equal(error.recovery.completionUnknown, false); return true;
  });
  assert.equal(client.calls.at(-1)!.name, "AbortMultipartUploadCommand");
});
test("real SDK signs versioned references without storage request", async () => {
  const client = new S3Client({ region: "us-east-1", endpoint: "https://objects.example.com", forcePathStyle: true,
    credentials: { accessKeyId: "TEST", secretAccessKey: "test-secret" } });
  try {
    const url = new URL(await new S3Storage("s3", "bucket", client).signedUrl(
      new Reference({ v: 1, storage: "s3", key: "report.xlsx", version: "v1" }), { expiresIn: 300 }));
    assert.equal(url.searchParams.get("versionId"), "v1");
    assert.equal(url.searchParams.get("X-Amz-Expires"), "300");
    assert.ok(url.searchParams.has("X-Amz-Signature"));
  } finally { client.destroy(); }
});
test("S3 cancelled completion exposes uncertain reference and never deletes", async () => {
  const controller = new AbortController();
  class CancelComplete extends FakeS3 {
    async send(command: any, options?: any) {
      if (command.constructor.name === "CompleteMultipartUploadCommand") {
        controller.abort(); options?.abortSignal?.throwIfAborted();
      }
      return super.send(command);
    }
  }
  const client = new CancelComplete();
  await assert.rejects(new S3Storage("s3", "bucket", client.client()).upload(Buffer.from("report"), { signal: controller.signal }), error => {
    assert.ok(error instanceof UploadError);
    assert.equal(error.recovery.completionUnknown, true);
    assert.equal(error.recovery.uploadId, "upload");
    assert.equal(error.recovery.reference.storage, "s3");
    assert.equal((error.cause as Error).name, "AbortError");
    return true;
  });
  assert.equal(client.calls.at(-1)!.name, "AbortMultipartUploadCommand");
});
