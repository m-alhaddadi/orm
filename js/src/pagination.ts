/**
 * Cursor pagination: keyset pages over a unique order, with opaque cursors.
 *
 * A cursor is base64url JSON `{"o": <order fingerprint>, "v": [<order values>]}`. It is
 * not signed: a client can change it to start at any position of the same order.
 */

import { blake2b } from "./blake2b.js";
import { Decimal } from "./decimal.js";
import { QueryError } from "./errors.js";
import { Column, Ordering, and, or, type Condition } from "./expr.js";
import type { FieldMeta } from "./meta.js";
import { fieldValue, type ModelMeta } from "./model.js";

/** One page of `paginate()`. `nextCursor` is the cursor of the last item and
 * `previousCursor` that of the first; both are `null` on an empty page. */
export interface Page<R> {
  readonly items: R[];
  readonly hasNext: boolean;
  readonly hasPrevious: boolean;
  readonly nextCursor: string | null;
  readonly previousCursor: string | null;
}

/** `{ first, after }` reads forward from a cursor, `{ last, before }` reads back. */
export type PageOptions =
  | { readonly first: number; readonly after?: string | null | undefined; readonly last?: never; readonly before?: never }
  | { readonly last: number; readonly before?: string | null | undefined; readonly first?: never; readonly after?: never };

/** @internal */
export type Key = readonly [FieldMeta, Ordering<string, unknown>];

// Column types with a cursor value; JSON, arrays and enums have none.
const CURSOR_TYPES = new Set(["big_int", "int", "float", "bool", "string", "text", "date_time", "date", "uuid", "decimal"]);

/** @internal The order columns of a page, with the primary key last when the order is not unique. */
export function keyset(meta: ModelMeta, order: readonly Ordering<string, unknown>[]): Key[] {
  const keys: Key[] = order.map((o) => {
    const e = o.expr;
    if (!(e instanceof Column) || e.path.length || e.root !== meta) {
      throw new QueryError(`paginate() orders by columns of ${meta.name} itself, not by ${String(e)}`);
    }
    const f = e.field;
    if (!CURSOR_TYPES.has(f.type) || f.array || f.enumName !== undefined) {
      throw new QueryError(`paginate() can't order by ${String(e)}: a ${f.array ? "array" : f.enumName !== undefined ? "enum" : f.type} column has no cursor value`);
    }
    if (f.nullable && o.nulls === undefined) {
      const direction = o.descending ? "desc" : "asc";
      throw new QueryError(`${String(e)} is nullable: order by ${String(e)}.${direction}({ nulls: "first" }) or ({ nulls: "last" }) to paginate`);
    }
    return [f, o] as const;
  });
  if (!keys.some(([f]) => f.primaryKey || (f.unique && !f.nullable))) {
    keys.push([meta.pk, meta.column(meta.pk).asc()]);
  }
  return keys;
}

/** @internal */
export function fingerprint(meta: ModelMeta, keys: readonly Key[]): string {
  const text = `${meta.name}:${keys.map(([f, o]) => `${o.descending ? "-" : ""}${f.ir}${o.nulls === undefined ? "" : ` nulls ${o.nulls}`}`).join(",")}`;
  return Buffer.from(blake2b(new TextEncoder().encode(text), 8)).toString("hex");
}

function encode(f: FieldMeta, v: unknown): unknown {
  if (v === null || v === undefined) return null;
  switch (f.type) {
    case "float":
    case "bool":
    case "string":
    case "text":
      return v;
    case "date_time":
      return (v as Date).toISOString();
    case "date":
      return (v as Date).toISOString().slice(0, 10);
    default:
      return String(v);
  }
}

function invalid(): never {
  throw new QueryError("invalid cursor");
}

function decode(f: FieldMeta, v: unknown): unknown {
  if (v === null) return f.nullable ? null : invalid();
  const text = typeof v === "string" ? v : undefined;
  switch (f.type) {
    case "big_int":
      return text !== undefined && /^-?\d+$/.test(text) ? BigInt(text) : invalid();
    case "int":
      return text !== undefined && /^-?\d+$/.test(text) ? Number(text) : invalid();
    case "decimal":
      try {
        return text !== undefined ? new Decimal(text) : invalid();
      } catch {
        return invalid();
      }
    case "uuid":
      return text ?? invalid();
    case "date_time":
    case "date": {
      const d = text !== undefined ? new Date(text) : undefined;
      return d && !Number.isNaN(d.getTime()) ? d : invalid();
    }
    case "float":
      return typeof v === "number" ? v : invalid();
    case "bool":
      return typeof v === "boolean" ? v : invalid();
    default:
      return text ?? invalid();
  }
}

/** @internal */
export function encodeCursor(fp: string, keys: readonly Key[], row: object): string {
  const values = keys.map(([f]) => encode(f, fieldValue(row, f.name)));
  return Buffer.from(JSON.stringify({ o: fp, v: values })).toString("base64url");
}

/** @internal */
export function decodeCursor(cursor: string, fp: string, keys: readonly Key[]): unknown[] {
  if (typeof cursor !== "string") throw new TypeError(`a cursor is a string, got ${String(cursor)}`);
  let data: unknown;
  try {
    data = JSON.parse(Buffer.from(cursor, "base64url").toString("utf8"));
  } catch {
    invalid();
  }
  const d = data as { o?: unknown; v?: unknown } | null;
  if (typeof d !== "object" || d === null || !Array.isArray(d.v) || d.v.length !== keys.length) invalid();
  if (d.o !== fp) throw new QueryError("the cursor belongs to another order or model; paginate with the order that made it");
  return keys.map(([f], i) => decode(f, (d.v as unknown[])[i]));
}

/** @internal Rows after the position `values` in `order` (the expanded form of a row comparison). */
export function after(order: readonly Ordering<string, unknown>[], values: readonly unknown[]): Condition<string, unknown> {
  const branches: Condition<string, unknown>[] = [];
  order.forEach((o, k) => {
    const col = o.expr as Column<unknown, string>;
    const v = values[k];
    let step: Condition<string, unknown> | undefined;
    if (v === null) {
      step = o.nulls === "first" ? col.isNotNull() : undefined;
    } else {
      const beyond = o.descending ? col.lt(v as never) : col.gt(v as never);
      step = o.nulls === "last" ? or(beyond, col.isNull()) : beyond;
    }
    if (step !== undefined) {
      const equal = order.slice(0, k).map((p, i) => {
        const c = p.expr as Column<unknown, string>;
        return values[i] === null ? c.isNull() : c.eq(values[i] as never);
      });
      branches.push(and(...equal, step));
    }
  });
  return or(...branches);
}
