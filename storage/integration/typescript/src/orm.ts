/** Selected ORM statement class. No ORM imports on storage/core-only paths. */
import { Reference } from "@orm/storage";
import { FileFieldError, Upload } from "./index.js";
import type { ModelAdapter } from "./model.js";

type Values = Record<string, unknown>;

/** The public ORM interface that file models use: `QuerySet.prepareInsert`,
 * `QuerySet.prepareUpdate` and `ModelMeta.addRowDecoder`. */
export interface HostQuerySet {
  insert(values: Values): PromiseLike<unknown>;
  insertMany(rows: readonly Values[], options?: unknown): PromiseLike<unknown>;
  update(values: Values, options?: unknown): Promise<unknown>;
  updateMany(rows: readonly Values[], options?: unknown): Promise<unknown>;
  prepareInsert(values: Values): { execute(values?: Values): Promise<unknown> };
  prepareUpdate(values: Values): { readonly unique: boolean; execute(values?: Values, options?: { readonly returning?: boolean }): Promise<unknown> };
}
export interface HostModel {
  objects: HostQuerySet;
  readonly fieldList: readonly { readonly name: string }[];
  addRowDecoder(decode: (row: Values) => void): void;
}

export function installQueries(meta: HostModel, adapter: ModelAdapter): void {
  const Base = meta.objects.constructor as new (meta: HostModel) => HostQuerySet;
  function normalize(values: Values, allowUpload = false): Values {
    const result = { ...values };
    for (const [name, value] of Object.entries(values)) {
      if (value === undefined) continue;
      if (value instanceof Upload) {
        if (!allowUpload || !adapter.fields.has(name)) throw new FileFieldError("Upload requires a single insert or unique-row update of a file field");
      } else if (adapter.fields.has(name)) result[name] = adapter.fields.get(name)!.decode(value)?.toJSON() ?? null;
    }
    return result;
  }
  function placeholders(values: Values): Values {
    return Object.fromEntries(Object.entries(values).map(([name, value]) => [name, value instanceof Upload
      ? new Reference({ v: 1, storage: adapter.fields.get(name)!.storage, key: "preflight" }).toJSON() : value]));
  }
  class FileQuerySet extends Base {
    // A rejected write, also a protected one, gives a rejected promise and never a throw.
    override insert(values: Values): PromiseLike<unknown> {
      let file: { execute(): Promise<unknown> } | undefined;
      let failed: unknown;
      try {
        values = normalize(values, true);
        if (!Object.values(values).some(value => value instanceof Upload)) return super.insert(values);
        file = this.prepareFileInsert(values);
      } catch (error) {
        failed = error;
      }
      // Uploads start when the insert is awaited, so a rejected onConflict uploads nothing.
      let started: Promise<unknown> | undefined;
      const run = () => (started ??= file ? file.execute() : Promise.reject(failed));
      return {
        onConflict(): never { throw new FileFieldError("Upload is unsupported in conflict writes"); },
        then: (onfulfilled, onrejected) => run().then(onfulfilled, onrejected),
        catch: (onrejected: (reason: unknown) => unknown) => run().catch(onrejected),
        finally: (onfinally: () => void) => run().finally(onfinally),
      } as PromiseLike<unknown> & { onConflict(): never };
    }
    prepareFileInsert(values: Values) {
      values = normalize(values, true);
      // Validate every ordinary value and the statement before the first upload.
      const prepared = this.prepareInsert(placeholders(values));
      const operation = adapter.prepareWrite(values, "insert");
      return { operation, execute: () => operation.execute(data => prepared.execute(data)) };
    }
    override insertMany(rows: readonly Values[], options?: unknown): PromiseLike<unknown> {
      return super.insertMany(rows.map(row => normalize(row)), options);
    }
    override async update(values: Values, options?: { readonly returning?: boolean }): Promise<unknown> {
      values = normalize(values, true);
      if (!Object.values(values).some(value => value instanceof Upload)) return super.update(values, options);
      const prepared = this.prepareUpdate(placeholders(values));
      if (!prepared.unique) throw new FileFieldError("Upload update must target one unique row");
      const operation = adapter.prepareWrite(values, "unique_update");
      return operation.execute(data => prepared.execute(data, options));
    }
    override updateMany(rows: readonly Values[], options?: unknown): Promise<unknown> {
      return super.updateMany(rows.map(row => normalize(row)), options);
    }
  }
  meta.objects = new FileQuerySet(meta);
}
