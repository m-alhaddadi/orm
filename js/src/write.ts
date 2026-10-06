/** Rows and assignments for INSERT / UPDATE statements. */

import { Expression, ParamRef, wrap, type IR, type IRContext } from "./expr.js";
import type { ModelMeta } from "./model.js";

/** Values of one row by TypeScript name, a to-one relation (`author: user`) giving its
 * key column. Expressions are refused unless `expressions`. */
function normalize(meta: ModelMeta, row: object, what: string, expressions: boolean): Map<string, unknown> {
  const values = new Map<string, unknown>();
  for (const [key, value] of Object.entries(row)) {
    if (value === undefined) {
      continue;
    }
    if (!expressions && (value instanceof Expression || value instanceof ParamRef)) {
      throw new TypeError(`${meta.name}.${key}: ${what} takes plain values, not expressions`);
    }
    const f = meta.inputFields.get(key);
    if (f) {
      values.set(f.ir, value);
      continue;
    }
    const rel = meta.relations.get(key);
    if (rel?.kind === "belongsTo") {
      const target = meta.registry.get(rel.target);
      const to = target.fieldByIr.get(rel.to)!.name;
      values.set(rel.from, value === null ? null : (value as Record<string, unknown>)[to]);
      continue;
    }
    throw new TypeError(`${meta.name} has no field ${JSON.stringify(key)}`);
  }
  return values;
}

/**
 * Validates insert rows and aligns them on one column list: the fields any row sets
 * (IR names, schema order), each row's values in that order (`undefined`: the column's
 * default), and the fields given explicitly (the default `doUpdate` columns).
 */
export function prepareRows(
  meta: ModelMeta,
  rows: readonly object[],
): { fields: string[]; rows: unknown[][]; provided: Set<string> } {
  const normalized: Map<string, unknown>[] = [];
  const provided = new Set<string>();
  for (const row of rows) {
    const values = normalize(meta, row, "insert", false);
    for (const k of values.keys()) {
      provided.add(k);
    }
    for (const f of meta.inputFieldList) {
      if (!values.has(f.ir) && !(f.hasInsertDefault || f.nullable)) {
        throw new TypeError(`${meta.name}.${f.name} is required`);
      }
    }
    normalized.push(values);
  }
  const fields = meta.fieldList.map((f) => f.ir).filter((n) => normalized.some((v) => v.has(n)));
  return { fields, rows: normalized.map((v) => fields.map((n) => v.get(n))), provided };
}

/**
 * Validates `updateMany` rows: each has the primary key and the same fields, plain
 * values only, no primary key twice. Gives the fields (IR names, the primary key first)
 * and the rows aligned on them.
 */
export function prepareUpdateRows(meta: ModelMeta, rows: readonly object[]): { fields: string[]; rows: unknown[][] } {
  const pk = meta.pk.ir;
  const normalized: Map<string, unknown>[] = [];
  const seen = new Set<unknown>();
  let fields: Set<string> | undefined;
  rows.forEach((row, n) => {
    const values = normalize(meta, row, "updateMany", false);
    const id = values.get(pk);
    if (id === undefined || id === null) {
      throw new TypeError(`updateMany row ${n} has no ${meta.pk.name}`);
    }
    const k = typeof id === "number" ? BigInt(id) : id;
    if (seen.has(k)) {
      throw new TypeError(`updateMany: ${meta.pk.name}=${String(id)} appears twice`);
    }
    seen.add(k);
    const keys = new Set(values.keys());
    if (fields === undefined) {
      if (keys.size === 1) {
        throw new TypeError(`updateMany rows need a field to set besides ${meta.pk.name}`);
      }
      fields = keys;
    } else if (keys.size !== fields.size || [...keys].some((x) => !fields!.has(x))) {
      throw new TypeError(`updateMany rows must set the same fields; row ${n} differs`);
    }
    normalized.push(values);
  });
  if (fields === undefined) {
    return { fields: [], rows: [] };
  }
  const names = [pk, ...meta.fieldList.map((f) => f.ir).filter((f) => fields!.has(f) && f !== pk)];
  return { fields: names, rows: normalized.map((v) => names.map((f) => v.get(f))) };
}

/** `SET` items as IR: plain values become parameters, expressions compile in `ctx`. */
export function assignments(meta: ModelMeta, values: object, ctx: IRContext): IR[] {
  return [...normalize(meta, values, "update", true)].map(([field, value]) => ({
    field,
    value: wrap(value).ir(ctx),
  }));
}

/** Validate explicit local-only attachment using definition-prepared inputs. */
export function prepareAttach(meta: ModelMeta, row: object): { fields: string[]; rows: unknown[][] } {
  const local = meta.attachInputFields;
  if (local === undefined) throw new TypeError(`${meta.name} is not a composed child`);
  const values = normalize(meta, row, "attach", false);
  const allowed = new Set(local.map((f) => f.ir));
  for (const name of values.keys()) {
    if (!allowed.has(name)) throw new TypeError(`${meta.name}.${name}: attach accepts only local child fields`);
  }
  for (const field of local) {
    if (!values.has(field.ir) && !(field.hasServerValue || field.nullable)) throw new TypeError(`${meta.name}.${field.name} is required`);
  }
  const fields = local.filter((f) => values.has(f.ir)).map((f) => f.ir);
  return { fields, rows: [fields.map((f) => values.get(f))] };
}
