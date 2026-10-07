/**
 * Schema metadata the runtime works from, and the type-level description of a model
 * (`ModelSpec`) the generated code fills in.
 */

import type { Decimal } from "./decimal.js";

/** Column types of the schema IR. */
export type ColType =
  | "big_int"
  | "int"
  | "float"
  | "bool"
  | "string"
  | "text"
  | "date_time"
  | "date"
  | "uuid"
  | "json"
  | "decimal";

/** A JSON value, as `Json` columns hold it. */
export type JsonValue = string | number | boolean | null | JsonValue[] | { [key: string]: JsonValue };

// -- type level ---------------------------------------------------------------------------

/**
 * Everything the types know about one model; generated per model (`UserSpec`).
 *
 * * `row`: an instance (`User`), `data`: its column values only;
 * * `insert` / `update` / `updateRow`: what `insert()`, `update()` and `updateMany()` take;
 * * `pk`: the primary key's value type.
 */
export interface ModelSpec {
  readonly name: string;
  readonly row: object;
  readonly data: object;
  readonly insert: object;
  readonly update: object;
  readonly updateRow: object;
  readonly pk: unknown;
}

/**
 * One relation hop of a path, for the types of loaded relations: `one` (always there),
 * `opt` (nullable to-one), `many` (to-many), `m2m` (many-to-many).
 */
export interface Hop<N extends string = string, K extends HopKind = HopKind, M extends ModelSpec = ModelSpec> {
  readonly name: N;
  readonly kind: K;
  readonly spec: M;
}

export type HopKind = "one" | "opt" | "many" | "m2m";

/** The value types a column accepts: `bigint` columns take safe-integer numbers too,
 * `Decimal` columns numbers and decimal text. */
export type In<T> = T extends bigint
  ? bigint | number
  : T extends Decimal
    ? Decimal | number | string
    : T extends readonly bigint[]
      ? readonly (bigint | number)[]
      : T extends readonly Decimal[]
        ? readonly (Decimal | number | string)[]
        : T extends readonly (infer E)[]
          ? readonly E[]
          : T;

// -- runtime ------------------------------------------------------------------------------

export interface FieldMeta {
  /** The TypeScript name (camelCase). */
  readonly name: string;
  /** The schema (IR) name. */
  readonly ir: string;
  readonly type: ColType;
  readonly nullable: boolean;
  readonly array: boolean;
  readonly enumName: string | undefined;
  readonly primaryKey: boolean;
  readonly unique: boolean;
  /** Native preparation or the database fills an omitted insert value. */
  readonly hasInsertDefault: boolean;
  /** Physical database default; client defaults do not change it. */
  readonly hasServerValue: boolean;
}

export type RelationKind = "belongsTo" | "hasOne" | "hasMany" | "manyToMany";

export interface RelationMeta {
  readonly name: string;
  readonly ir: string;
  readonly kind: RelationKind;
  readonly target: string;
  /** IR field names: `source.from == target.to` (or through the join model). */
  readonly from: string;
  readonly to: string;
  readonly through: { readonly model: string; readonly source: string; readonly target: string } | undefined;
}

/** `author_id` -> `authorId`: how schema names read in TypeScript. */
export function camel(name: string): string {
  return name.replace(/(?<=[A-Za-z0-9])_+([A-Za-z0-9])/g, (_, c: string) => c.toUpperCase());
}
