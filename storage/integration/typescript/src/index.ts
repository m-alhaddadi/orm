import { Reference, Registry, UploadError, type Source, type UploadOptions, type UploadRecovery } from "@orm/storage";

export class FileFieldError extends Error {}
export class FileNotLoaded extends FileFieldError {}
export class MissingFile extends FileFieldError {}
export class Upload {
  readonly options: Readonly<UploadOptions>;
  constructor(readonly source: Source, options: Readonly<UploadOptions> = {}) {
    if (!(source instanceof Uint8Array) && (!source || typeof source[Symbol.asyncIterator] !== "function")) throw new TypeError("Upload requires bytes or a byte stream");
    this.options = Object.freeze({ ...options });
  }
}
export class FileField {
  constructor(readonly name: string, readonly storage: string, readonly nullable = false) {}
  decode(value: unknown): Reference | null {
    if (value === null) {
      if (!this.nullable) throw new MissingFile(`${this.name} is not nullable`);
      return null;
    }
    const reference = value instanceof Reference ? value : Reference.fromJSON(value);
    if (reference.storage !== this.storage) throw new FileFieldError(`${this.name} requires storage ${this.storage}`);
    return reference;
  }
  reference(snapshot: Readonly<Record<string, unknown>>): Reference {
    if (!Object.hasOwn(snapshot, this.name)) throw new FileNotLoaded(`${this.name} was not selected`);
    const reference = this.decode(snapshot[this.name]);
    if (reference === null) throw new MissingFile(`${this.name} has no file`);
    return reference;
  }
  async signedUrl(snapshot: Readonly<Record<string, unknown>>, registry: Registry, options: { expiresIn?: number } = {}): Promise<string> {
    const reference = this.reference(snapshot);
    return registry.resolve(reference).signedUrl(reference, options);
  }
  open(snapshot: Readonly<Record<string, unknown>>, registry: Registry): AsyncIterable<Uint8Array> {
    const reference = this.reference(snapshot);
    return registry.resolve(reference).open(reference);
  }
}
export class FileWriteError extends Error {
  constructor(readonly operation: PreparedFileWrite, cause: unknown) {
    super("file write failed; inspect operation.references and operation.recovery", { cause });
  }
}
export class PreparedFileWrite {
  private readonly values: Readonly<Record<string, unknown>>;
  private readonly fields: ReadonlyMap<string, FileField>;
  private readonly data: Record<string, unknown>;
  private readonly completed = new Map<string, Reference>();
  private readonly recoveries: UploadRecovery[] = [];
  private state = "new";
  private executing = false;
  constructor(values: Readonly<Record<string, unknown>>, fields: ReadonlyMap<string, FileField>,
    private readonly registry: Registry, options: { shape?: string } = {}) {
    this.values = { ...values }; this.fields = new Map(fields); this.data = { ...values };
    for (const [name, value] of Object.entries(this.values)) {
      const field = fields.get(name);
      if (!field) {
        if (value instanceof Upload) throw new FileFieldError(`${name} is not a file field`);
        continue;
      }
      if (value instanceof Upload) {
        if (!["insert", "unique_update"].includes(options.shape ?? "insert")) throw new FileFieldError(`Upload is unsupported for ${options.shape}`);
        new Reference({ v: 1, storage: field.storage, key: "validation",
          ...(value.options.filename !== undefined ? { filename: value.options.filename } : {}),
          ...(value.options.contentType !== undefined ? { content_type: value.options.contentType } : {}) });
        const limit = value.options.maxSize ?? Number.MAX_SAFE_INTEGER;
        if (!Number.isSafeInteger(limit) || limit < 0) throw new FileFieldError("maxSize must be a nonnegative safe integer");
        registry.resolve(new Reference({ v: 1, storage: field.storage, key: "validation" }));
      } else {
        this.data[name] = field.decode(value)?.toJSON() ?? null;
      }
    }
  }
  get references(): ReadonlyMap<string, Reference> { return new Map(this.completed); }
  get recovery(): readonly UploadRecovery[] { return [...this.recoveries]; }
  async prepare(): Promise<Readonly<Record<string, unknown>>> {
    if (this.state === "ready") return { ...this.data };
    if (this.state !== "new") throw new FileFieldError("upload preparation already running or failed; sources cannot be replayed");
    this.state = "preparing";
    try {
      for (const [name, value] of Object.entries(this.values)) {
        if (!(value instanceof Upload)) continue;
        const field = this.fields.get(name)!;
        const reference = await this.registry.resolve(new Reference({ v: 1, storage: field.storage, key: "validation" }))
          .upload(value.source, value.options);
        this.completed.set(name, reference); this.data[name] = reference.toJSON();
      }
      this.state = "ready"; return { ...this.data };
    } catch (error) {
      this.state = "failed";
      if (error instanceof UploadError) this.recoveries.push(error.recovery);
      throw new FileWriteError(this, error);
    }
  }
  async execute<T>(statement: (values: Readonly<Record<string, unknown>>) => Promise<T>): Promise<T> {
    if (this.executing) throw new FileFieldError("file write already executing");
    this.executing = true;
    try { return await statement(await this.prepare()); }
    catch (error) { if (error instanceof FileWriteError) throw error; throw new FileWriteError(this, error); }
    finally { this.executing = false; }
  }
}
