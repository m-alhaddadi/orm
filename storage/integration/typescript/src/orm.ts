/** Selected ORM statement class. No ORM imports on storage/core-only paths. */
import { Reference } from "@orm/storage";
import { FileFieldError, Upload } from "./index.js";
import type { ModelAdapter } from "./model.js";

// The host passes its prepared model once. Values/IR are validated by native
// planning before upload; no schema/provider registry lookup selects execution.
export function installQueries(meta: any, adapter: ModelAdapter): void {
  const Base = meta.objects.constructor as new (...args: any[]) => any;
  function normalize(values: Record<string, unknown>, allowUpload = false): Record<string, unknown> {
    const result = { ...values };
    for (const [name, value] of Object.entries(values)) {
      if (value === undefined) continue;
      if (value instanceof Upload) {
        if (!allowUpload || !adapter.fields.has(name)) throw new FileFieldError("Upload requires a single insert or unique-row update of a file field");
      } else if (adapter.fields.has(name)) result[name] = adapter.fields.get(name)!.decode(value)?.toJSON() ?? null;
    }
    return result;
  }
  function placeholders(values: Record<string, unknown>): Record<string, unknown> {
    return Object.fromEntries(Object.entries(values).map(([name, value]) => [name, value instanceof Upload
      ? new Reference({ v: 1, storage: adapter.fields.get(name)!.storage, key: "preflight" }).toJSON() : value]));
  }
  function insertPreflight(values: Record<string, unknown>): void {
    const byIr = new Map<string, unknown>();
    for (const [name, value] of Object.entries(values)) {
      if (value === undefined) continue;
      const field = meta.inputFields.get(name);
      if (field) byIr.set(field.ir, value);
      else {
        const relation = meta.relations.get(name);
        if (relation?.kind !== "belongsTo") throw new TypeError(`${meta.name} has no field ${name}`);
        const target = meta.registry.get(relation.target);
        byIr.set(relation.from, value === null ? null : (value as Record<string, unknown>)[target.fieldByIr.get(relation.to).name]);
      }
    }
    for (const field of meta.inputFieldList) {
      if (!byIr.has(field.ir) && !field.hasServerValue && !field.nullable) throw new TypeError(`${meta.name}.${field.name} is required`);
    }
    const fields = meta.fieldList.filter((field: any) => byIr.has(field.ir)).map((field: any) => field.ir);
    meta.registry.native().validateFileInsert(meta.name, fields, [fields.map((name: string) => byIr.get(name))]);
  }
  class FileQuerySet extends Base {
    insert(values: Record<string, unknown>, options?: unknown): Promise<unknown> {
      values = normalize(values, true);
      if (!Object.values(values).some(value => value instanceof Upload)) return super.insert(values, options);
      if (options !== undefined) throw new FileFieldError("Upload is unsupported in conflict writes");
      return this.prepareFileInsert(values).execute();
    }
    prepareFileInsert(values: Record<string, unknown>) {
      values = normalize(values, true);
      this.db();
      insertPreflight(placeholders(values));
      const operation = adapter.prepareWrite(values, "insert");
      return { operation, execute: () => operation.execute(data => super.insert(data)) };
    }
    insertMany(rows: readonly Record<string, unknown>[], options?: unknown): Promise<unknown> {
      return super.insertMany(rows.map(row => normalize(row)), options);
    }
    update(values: Record<string, unknown>, options?: unknown): Promise<unknown> {
      values = normalize(values, true);
      if (!Object.values(values).some(value => value instanceof Upload)) return super.update(values, options);
      this.db();
      const params: unknown[] = [];
      const ir = this.mutationIr("update", params, placeholders(values));
      const unique = new Set(meta.fieldList.filter((f: any) => f.primaryKey || f.unique).map((f: any) => f.ir));
      function provesUnique(expr: any): boolean {
        if (expr.t === "and") return expr.items.some(provesUnique);
        if (expr.t !== "cmp" || expr.op !== "eq") return false;
        return [[expr.l, expr.r], [expr.r, expr.l]].some(([column, parameter]) => column?.t === "col"
          && !column.path.length && unique.has(column.name) && parameter?.t === "param" && params[parameter.i] != null);
      }
      if (!ir.filters.some(provesUnique)) throw new FileFieldError("Upload update must target one unique row");
      meta.registry.native().sql(JSON.stringify(ir), params);
      const operation = adapter.prepareWrite(values, "unique_update");
      return operation.execute(data => super.update(data, options));
    }
    updateMany(rows: readonly Record<string, unknown>[], options?: unknown): Promise<unknown> {
      return super.updateMany(rows.map(row => normalize(row)), options);
    }
  }
  meta.objects = new FileQuerySet(meta, meta.objects.state);
}
