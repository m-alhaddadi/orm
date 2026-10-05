import { randomUUID } from "node:crypto";
import { S3Client, CreateMultipartUploadCommand, UploadPartCommand, CompleteMultipartUploadCommand,
  AbortMultipartUploadCommand, GetObjectCommand, DeleteObjectCommand } from "@aws-sdk/client-s3";
import { getSignedUrl } from "@aws-sdk/s3-request-presigner";
import { Reference, chunks, checkReference, checkExpiry, StorageError, UploadError,
  type Provider, type Source, type UploadOptions } from "./index.js";

const PART_SIZE = 5 * 1024 * 1024;
/** SDK imports live exclusively in this optional entrypoint. Application owns client lifetime. */
export class S3Storage implements Provider {
  constructor(readonly storage: string, readonly bucket: string, readonly client: S3Client) {
    new Reference({ v: 1, storage, key: "validation" });
    if (!bucket) throw new TypeError("bucket must be nonempty");
  }
  private params(reference: Reference) {
    checkReference(this.storage, reference);
    return { Bucket: this.bucket, Key: reference.key,
      ...(reference.version !== undefined ? { VersionId: reference.version } : {}) };
  }
  async upload(source: Source, options: UploadOptions = {}): Promise<Reference> {
    const reference = new Reference({ v: 1, storage: this.storage, key: randomUUID(),
      ...(options.filename !== undefined ? { filename: options.filename } : {}),
      content_type: options.contentType || "application/octet-stream" });
    const params = this.params(reference);
    let uploadId: string | undefined;
    let completing = false;
    const parts: { PartNumber: number; ETag: string }[] = [];
    let buffer = Buffer.allocUnsafe(PART_SIZE), filled = 0, size = 0;
    const part = async (data: Uint8Array) => {
      if (parts.length >= 10000) throw new StorageError("S3 multipart limit exceeded");
      const number = parts.length + 1;
      const response = await this.client.send(new UploadPartCommand({ ...params, UploadId: uploadId,
        PartNumber: number, Body: data }), { abortSignal: options.signal });
      if (!response.ETag) throw new StorageError("provider omitted part ETag");
      parts.push({ PartNumber: number, ETag: response.ETag });
    };
    try {
      options.signal?.throwIfAborted();
      const created = await this.client.send(new CreateMultipartUploadCommand({ ...params, ContentType: reference.contentType }));
      uploadId = created.UploadId;
      if (!uploadId) throw new StorageError("provider omitted multipart upload identity");
      for await (const chunk of chunks(source, options)) {
        let offset = 0; size += chunk.length;
        while (offset < chunk.length) {
          const count = Math.min(PART_SIZE - filled, chunk.length - offset);
          buffer.set(chunk.subarray(offset, offset + count), filled);
          filled += count; offset += count;
          if (filled === PART_SIZE) { await part(buffer); filled = 0; }
        }
      }
      if (filled || !parts.length) await part(buffer.subarray(0, filled));
      options.signal?.throwIfAborted();
      completing = true;
      const completed = await this.client.send(new CompleteMultipartUploadCommand({ ...params, UploadId: uploadId,
        MultipartUpload: { Parts: parts } }));
      // Do not report cancellation after completion: the durable object already exists.
      return new Reference({ ...reference.toJSON(), size,
        ...(completed.VersionId !== undefined ? { version: completed.VersionId } : {}) });
    } catch (error) {
      let cleanupError: unknown;
      if (uploadId !== undefined) {
        try { await this.client.send(new AbortMultipartUploadCommand({ ...params, UploadId: uploadId })); }
        catch (abortError) { cleanupError = abortError; }
      }
      throw new UploadError({ reference, uploadId, completionUnknown: completing, cleanupError }, error);
    }
  }

  async *open(reference: Reference): AsyncGenerator<Uint8Array> {
    const response = await this.client.send(new GetObjectCommand(this.params(reference)));
    if (!response.Body) throw new StorageError("provider omitted object body");
    // transformToWebStream retains streaming and supports cancellation on early exit.
    const reader = response.Body.transformToWebStream().getReader();
    try {
      while (true) { const next = await reader.read(); if (next.done) break; yield next.value; }
    } finally { await reader.cancel(); reader.releaseLock(); }
  }
  async delete(reference: Reference): Promise<void> {
    await this.client.send(new DeleteObjectCommand(this.params(reference)));
  }
  async signedUrl(reference: Reference, options: { expiresIn?: number } = {}): Promise<string> {
    const expiresIn = options.expiresIn ?? 300;
    checkExpiry(expiresIn);
    return getSignedUrl(this.client, new GetObjectCommand(this.params(reference)), { expiresIn });
  }
}
