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
  | { readonly first: number; readonly after?: string | null | undefined; readonly last?: never; readonly before?: null }
  | { readonly last: number; readonly before?: string | null | undefined; readonly first?: never; readonly after?: null };

/** @internal */
export type Key = readonly [FieldMeta, Ordering<string, unknown>];

/** @internal Whether the column can hold NULL; a proxy can declare a nullable column non-null. */
export function storedNullable(meta: ModelMeta, f: FieldMeta): boolean {
  return f.nullable || meta.narrowed.has(f.ir);
}

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
      throw new QueryError(`paginate() can't order by ${String(e)}: ${f.array ? "array" : f.enumName !== undefined ? "enum" : f.type} columns have no cursor value`);
    }
    if (storedNullable(meta, f) && o.nulls === undefined) {
      const direction = o.descending ? "desc" : "asc";
      throw new QueryError(`${String(e)} is nullable: order by ${String(e)}.${direction}({ nulls: "first" }) or ({ nulls: "last" }) to paginate`);
    }
    return [f, o] as const;
  });
  if (!keys.some(([f]) => f.primaryKey || (f.unique && !storedNullable(meta, f)))) {
    keys.push([meta.pk, meta.column(meta.pk).asc()]);
  }
  return keys;
}

/** @internal */
export function fingerprint(meta: ModelMeta, keys: readonly Key[]): string {
  const text = `${meta.name}:${keys.map(([f, o]) => `${o.descending ? "-" : ""}${f.ir}${o.nulls === undefined ? "" : ` nulls ${o.nulls}`}`).join(",")}`;
  return Buffer.from(blake2b(new TextEncoder().encode(text), 8)).toString("hex");
}

/** The hidden property of a `Date` from a `DateTime` column with the microseconds
 * below its milliseconds (set by the native driver). */
const MICROS = "orm:micros";
const INT32 = 2 ** 31;
const INT64 = 2n ** 63n;
const UUID = /^[0-9a-f]{8}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{12}$/i;
const NONFINITE: Record<string, number> = { Infinity: Infinity, "-Infinity": -Infinity, NaN: NaN };

// The cursor text of a DateTime is Python's `isoformat()`: microseconds and `+00:00`.
function isoDateTime(d: Date): string {
  const [day, time] = d.toISOString().split("T") as [string, string];
  const us = d.getUTCMilliseconds() * 1000 + (((d as unknown as Record<string, unknown>)[MICROS] as number | undefined) ?? 0);
  return `${day}T${time.slice(0, 8)}${us ? `.${String(us).padStart(6, "0")}` : ""}+00:00`;
}

function encode(f: FieldMeta, v: unknown): unknown {
  if (v === null || v === undefined) return null;
  switch (f.type) {
    case "float":
      return Number.isFinite(v) ? v : String(v);
    case "bool":
    case "string":
    case "text":
      return v;
    case "date_time":
      return isoDateTime(v as Date);
    case "date":
      return (v as Date).toISOString().split("T")[0];
    case "decimal":
      if (!(v as Decimal).isFinite()) throw new QueryError(`paginate() can't make a cursor from the decimal ${String(v)}`);
      return String(v);
    default:
      return String(v);
  }
}

function invalid(): never {
  throw new QueryError("invalid cursor");
}

function decodeDateTime(text: string): Date {
  const m = /^[+-]?\d{4,6}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.(\d{1,9}))?(?:Z|[+-]\d\d:\d\d)$/.exec(text);
  const d = m ? new Date(text) : undefined;
  if (!d || Number.isNaN(d.getTime())) invalid();
  const us = Number((m![1] ?? "").padEnd(6, "0").slice(3, 6));
  if (us) Object.defineProperty(d, MICROS, { value: us });
  return d;
}

function decode(f: FieldMeta, nullable: boolean, v: unknown): unknown {
  if (v === null) return nullable ? null : invalid();
  const text = typeof v === "string" ? v : undefined;
  switch (f.type) {
    case "big_int": {
      const n = text !== undefined && /^-?\d+$/.test(text) ? BigInt(text) : invalid();
      return n >= -INT64 && n < INT64 ? n : invalid();
    }
    case "int": {
      const n = text !== undefined && /^-?\d+$/.test(text) ? Number(text) : invalid();
      return n >= -INT32 && n < INT32 ? n : invalid();
    }
    case "decimal":
      try {
        const d = text !== undefined ? new Decimal(text) : invalid();
        return d.isFinite() ? d : invalid();
      } catch {
        return invalid();
      }
    case "uuid":
      return text !== undefined && UUID.test(text) ? text : invalid();
    case "date_time":
      return text !== undefined ? decodeDateTime(text) : invalid();
    case "date": {
      const d = text !== undefined && /^[+-]?\d{4,6}-\d\d-\d\d$/.test(text) ? new Date(text) : undefined;
      return d && !Number.isNaN(d.getTime()) ? d : invalid();
    }
    case "float":
      return typeof v === "number" ? v : text !== undefined && Object.hasOwn(NONFINITE, text) ? NONFINITE[text] : invalid();
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
export function decodeCursor(meta: ModelMeta, cursor: string, fp: string, keys: readonly Key[]): unknown[] {
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
  return keys.map(([f], i) => decode(f, storedNullable(meta, f), (d.v as unknown[])[i]));
}

/**
 * @internal Rows after the position `values` in `order` (the expanded form of a row comparison).
 *
 * A NOT NULL first column also gets a plain bound (`a >= v`), which an index on the
 * order can use to start at the cursor; the expanded form alone reads from the start.
 */
export function after(meta: ModelMeta, order: readonly Ordering<string, unknown>[], values: readonly unknown[]): Condition<string, unknown> {
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
  const first = order[0]!.expr as Column<unknown, string>;
  if (order.length > 1 && !storedNullable(meta, first.field)) {
    return and(order[0]!.descending ? first.lte(values[0] as never) : first.gte(values[0] as never), or(...branches));
  }
  return or(...branches);
}
