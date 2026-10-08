/**
 * Objects from the rows the engine returns, built in one pass: instances (with
 * `selectRelated` objects and prefetched relations attached) and `select()` rows.
 */

import { Decimal } from "./decimal.js";
import type { Database } from "./db.js";
import { related, type ModelMeta, type Registry } from "./model.js";
import type {
  NativeColumns,
  NativeInstances,
  NativePrefetch,
  NativePrefetched,
  NativeReturned,
  NativeRows,
  NativeSelect,
} from "./native.js";

type Obj = Record<PropertyKey, unknown>;

/** A key for grouping related rows: values compared by value (`Date`, `Decimal`). */
function key(v: unknown): unknown {
  if (v instanceof Date) {
    return `d${v.getTime()}`;
  }
  if (v instanceof Decimal) {
    return `n${v.toString()}`;
  }
  return v;
}

export class Builder {
  constructor(
    readonly registry: Registry,
    readonly db: Database | undefined,
  ) {}

  private meta(name: string | null): ModelMeta {
    if (name === null) {
      throw new TypeError("rows of a CTE have no model");
    }
    return this.registry.get(name);
  }

  /** One instance per row, `selectRelated` objects attached. */
  instances(out: NativeInstances, rows: NativeRows): Obj[] {
    const { n, width, values } = rows;
    const meta = this.meta(out.model);
    const joins = out.joins.map((j) => ({ ...j, meta: this.meta(j.model) }));
    const objs: Obj[] = new Array(n);
    const joined: (Obj | null)[] = new Array(joins.length);
    for (let r = 0; r < n; r++) {
      const base = r * width;
      const root = meta.instance(values, base, this.db, out.shape);
      for (let i = 0; i < joins.length; i++) {
        const j = joins[i]!;
        // A LEFT JOIN without a match gives NULLs, the primary key included.
        const child = values[base + j.start + j.pk] === null ? null : j.meta.instance(values, base + j.start, this.db, j.shape);
        joined[i] = child;
        const parent = j.parent < 0 ? root : joined[j.parent];
        if (parent) {
          const pmeta = j.parent < 0 ? meta : joins[j.parent]!.meta;
          related(parent)[pmeta.relationByIr.get(j.attr)!.name] = child;
        }
      }
      objs[r] = root;
    }
    return objs;
  }

  /** The objects of a top-level SELECT: instances (prefetched relations attached), or
   * `select()` rows keyed by `keys`. */
  select(res: NativeSelect, keys?: readonly string[]): Obj[] {
    if ("joins" in res.output) {
      const objs = this.instances(res.output, res.rows);
      const meta = this.meta(res.output.model);
      for (const f of res.prefetched) {
        this.attach(meta, objs, res.rows, f);
      }
      return objs;
    }
    return this.columns(res.output, res.rows, keys ?? []);
  }

  /** Puts the relations of `res` on `parents`, whose key rows `res.rows` are (`prefetch()`). */
  prefetched(meta: ModelMeta, parents: Obj[], res: NativePrefetched): void {
    for (const f of res.prefetched) {
      this.attach(meta, parents, res.rows, f);
    }
  }

  private columns(out: NativeColumns, rows: NativeRows, keys: readonly string[]): Obj[] {
    const { n, width, values } = rows;
    const objs: Obj[] = new Array(n);
    for (let r = 0; r < n; r++) {
      const o: Obj = {};
      let pos = r * width;
      for (let i = 0; i < out.items.length; i++) {
        const w = out.items[i]!;
        if (w < 0) {
          o[keys[i]!] = values[pos];
          pos += 1;
        } else {
          o[keys[i]!] = this.meta(out.model).instance(values, pos, this.db);
          pos += w;
        }
      }
      objs[r] = o;
    }
    return objs;
  }

  /** Instances for rows a write returned. */
  returned(res: NativeReturned): Obj[] {
    return this.instances({ model: res.model, shape: res.shape ?? null, joins: [] }, res.rows);
  }

  /** Puts the related objects of `f` on `parents` (built from `parentRows`, in order). */
  private attach(meta: ModelMeta, parents: Obj[], parentRows: NativeRows, f: NativePrefetch): void {
    const children = this.instances(f.output, f.rows);
    const childMeta = this.meta(f.output.model);
    for (const c of f.children) {
      this.attach(childMeta, children, f.rows, c);
    }
    const groups = new Map<unknown, Obj | Obj[]>();
    const cw = f.rows.width;
    for (let i = 0; i < children.length; i++) {
      const k = key(f.rows.values[i * cw + f.childKeyPos]);
      if (f.many) {
        const list = groups.get(k) as Obj[] | undefined;
        if (list) {
          list.push(children[i]!);
        } else {
          groups.set(k, [children[i]!]);
        }
      } else {
        groups.set(k, children[i]!);
      }
    }
    // The relation itself (by its schema name), or a `toAttr` attribute.
    const rel = meta.relationByIr.get(f.attr);
    const back = f.back === null ? undefined : childMeta.relationByIr.get(f.back)!.name;
    const pw = parentRows.width;
    for (let i = 0; i < parents.length; i++) {
      const parent = parents[i]!;
      const k = parentRows.values[i * pw + f.keyPos];
      const found = k === null ? undefined : groups.get(key(k));
      let value: unknown;
      if (f.many) {
        const list = (found as Obj[] | undefined) ?? [];
        if (back !== undefined) {
          for (const c of list) {
            related(c)[back] = parent;
          }
        }
        value = list;
      } else {
        if (back !== undefined && found) {
          related(found as Obj)[back] = parent;
        }
        value = found ?? null;
      }
      if (rel) {
        related(parent)[rel.name] = value;
      } else {
        parent[f.attr] = value;
      }
    }
  }
}
