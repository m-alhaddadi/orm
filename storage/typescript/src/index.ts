import { randomUUID } from "node:crypto";
import { constants } from "node:fs";
import { mkdir, open, unlink, realpath } from "node:fs/promises";
import { join, resolve } from "node:path";

export const CHUNK_SIZE = 64 * 1024;
export class StorageError extends Error {}
export class CapabilityError extends StorageError {}
export class SizeLimitError extends StorageError {}
export interface ReferenceData {
  readonly v: 1;
  readonly storage: string;
  readonly key: string;
  readonly version?: string;
  readonly filename?: string;
  readonly content_type?: string;
  readonly size?: number;
}
export class Reference {
  readonly storage: string;
  readonly key: string;
  readonly version?: string;
  readonly filename?: string;
  readonly contentType?: string;
  readonly size?: number;
  constructor(data: ReferenceData) {
    const allowed = ["v", "storage", "key", "version", "filename", "content_type", "size"];
    if (data.v !== 1 || Object.keys(data).some(k => !allowed.includes(k))) throw new TypeError("invalid reference version or properties");
    for (const name of ["storage", "key"] as const) {
      if (typeof data[name] !== "string" || !data[name] || data[name].includes("\0")) throw new TypeError(`invalid ${name}`);
    }
    for (const name of ["version", "filename", "content_type"] as const) {
      if (Object.hasOwn(data, name) && (typeof data[name] !== "string" || data[name]!.includes("\0"))) throw new TypeError(`invalid ${name}`);
    }
    if (Object.hasOwn(data, "size") && (!Number.isSafeInteger(data.size) || data.size! < 0)) throw new TypeError("size must be a nonnegative safe integer");
    this.storage = data.storage; this.key = data.key; this.version = data.version;
    this.filename = data.filename; this.contentType = data.content_type; this.size = data.size;
    Object.freeze(this);
  }
  toJSON(): ReferenceData {
    return { v: 1, storage: this.storage, key: this.key,
      ...(this.version !== undefined ? { version: this.version } : {}),
      ...(this.filename !== undefined ? { filename: this.filename } : {}),
      ...(this.contentType !== undefined ? { content_type: this.contentType } : {}),
      ...(this.size !== undefined ? { size: this.size } : {}) };
  }
  static fromJSON(value: string | unknown): Reference {
    const data: unknown = typeof value === "string" ? JSON.parse(value) : value;
    if (!data || typeof data !== "object" || Array.isArray(data)) throw new TypeError("reference must be an object");
    return new Reference(data as ReferenceData);
  }
}
export interface UploadRecovery {
  readonly reference: Reference;
  readonly uploadId?: string;
  readonly completionUnknown: boolean;
  readonly cleanupError?: unknown;
}
export class UploadError extends StorageError {
  constructor(readonly recovery: UploadRecovery, cause: unknown) {
    super(`upload failed; recovery key=${recovery.reference.key}`, { cause });
  }
}
export type Source = Uint8Array | AsyncIterable<Uint8Array>;
export interface UploadOptions { filename?: string; contentType?: string; maxSize?: number; signal?: AbortSignal }
export interface Provider {
  readonly storage: string;
  upload(source: Source, options?: UploadOptions): Promise<Reference>;
  open(reference: Reference): AsyncIterable<Uint8Array>;
  delete(reference: Reference): Promise<void>;
  signedUrl(reference: Reference, options?: { expiresIn?: number }): Promise<string>;
}
export function checkReference(storage: string, reference: Reference): void {
  if (reference.storage !== storage) throw new StorageError(`reference belongs to ${reference.storage}, not ${storage}`);
}
export function checkExpiry(expiresIn: number): void {
  if (!Number.isInteger(expiresIn) || expiresIn < 1 || expiresIn > 604800) throw new RangeError("expiresIn must be an integer from 1 to 604800 seconds");
}
export async function* chunks(source: Source, options: UploadOptions = {}): AsyncGenerator<Uint8Array> {
  const limit = options.maxSize ?? Number.MAX_SAFE_INTEGER;
  if (!Number.isSafeInteger(limit) || limit < 0) throw new RangeError("maxSize must be a nonnegative safe integer");
  let size = 0;
  // Do not call return() on caller iterators: it may close a caller-owned stream.
  const iterator = source instanceof Uint8Array ? undefined : source[Symbol.asyncIterator]();
  let offset = 0;
  while (true) {
    options.signal?.throwIfAborted();
    let chunk: Uint8Array;
    if (source instanceof Uint8Array) {
      if (offset >= source.length) break;
      chunk = source.subarray(offset, offset + CHUNK_SIZE); offset += chunk.length;
    } else {
      const next = await iterator!.next();
      options.signal?.throwIfAborted();
      if (next.done) break;
      chunk = next.value;
    }
    if (!(chunk instanceof Uint8Array)) throw new TypeError("upload streams must yield Uint8Array");
    size += chunk.length;
    if (size > limit) throw new SizeLimitError(`upload exceeds ${limit} bytes`);
    for (let i = 0; i < chunk.length; i += CHUNK_SIZE) {
      options.signal?.throwIfAborted();
      yield chunk.subarray(i, i + CHUNK_SIZE);
    }
  }
}
export class LocalStorage implements Provider {
  readonly root: string;
  private readonly ready: Promise<void>;
  constructor(readonly storage: string, root: string,
    private readonly signer?: (reference: Reference, expiresIn: number) => string | Promise<string>) {
    new Reference({ v: 1, storage, key: "validation" });
    this.root = resolve(root);
    this.ready = mkdir(this.root, { recursive: true }).then(async () => {
      if (await realpath(this.root) !== this.root) throw new StorageError("local root must use its canonical path");
    });
  }
  private path(reference: Reference): string {
    checkReference(this.storage, reference);
    if (reference.version !== undefined) throw new CapabilityError("local storage does not support object versions");
    if (!/^[a-zA-Z0-9_-]+$/.test(reference.key)) throw new StorageError("local keys must be flat safe identifiers");
    return join(this.root, reference.key);
  }
  async upload(source: Source, options: UploadOptions = {}): Promise<Reference> {
    const data: ReferenceData = { v: 1, storage: this.storage, key: randomUUID(),
      ...(options.filename !== undefined ? { filename: options.filename } : {}),
      content_type: options.contentType || "application/octet-stream" };
    const reference = new Reference(data);
    await this.ready;
    const path = this.path(reference);
    const output = await open(path, "wx");
    let size = 0;
    try {
      for await (const chunk of chunks(source, options)) {
        let offset = 0;
        while (offset < chunk.length) {
          const { bytesWritten } = await output.write(chunk, offset, chunk.length - offset);
          if (!bytesWritten) throw new StorageError("filesystem write made no progress");
          offset += bytesWritten;
        }
        size += chunk.length;
      }
      options.signal?.throwIfAborted();
      await output.close();
      return new Reference({ ...data, size });
    } catch (error) {
      await output.close();
      await unlink(path);
      throw error;
    }
  }
  async *open(reference: Reference): AsyncGenerator<Uint8Array> {
    await this.ready;
    const input = await open(this.path(reference), constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      while (true) {
        const chunk = Buffer.allocUnsafe(CHUNK_SIZE);
        const { bytesRead } = await input.read(chunk, 0, chunk.length, null);
        if (!bytesRead) break;
        yield chunk.subarray(0, bytesRead);
      }
    } finally { await input.close(); }
  }
  async delete(reference: Reference): Promise<void> {
    await this.ready;
    try { await unlink(this.path(reference)); }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; }
  }
  async signedUrl(reference: Reference, options: { expiresIn?: number } = {}): Promise<string> {
    this.path(reference);
    const expiry = options.expiresIn ?? 300;
    checkExpiry(expiry);
    if (!this.signer) throw new CapabilityError("local signed URLs require an application serving/signing mechanism");
    return this.signer(reference, expiry);
  }
}
export class Registry {
  private readonly providers: ReadonlyMap<string, Provider>;
  constructor(providers: ReadonlyMap<string, Provider>) {
    for (const [name, provider] of providers) if (name !== provider.storage) throw new StorageError("registry identity must match provider identity");
    this.providers = new Map(providers);
  }
  resolve(reference: Reference): Provider {
    const provider = this.providers.get(reference.storage);
    if (!provider) throw new StorageError(`unconfigured storage: ${reference.storage}`);
    return provider;
  }
}
