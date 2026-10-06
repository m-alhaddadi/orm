/**
 * The Rust engine (`bindings/node`), loaded once. Internal: use the package API.
 *
 * The addon is `orm.node` next to `package.json` (`npm run build:native`), or the file
 * `ORM_NATIVE` names.
 */

import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

import { Decimal } from "./decimal.js";
import { fromNative } from "./errors.js";

/** Rows of one statement: `n` rows of `width` cells, row after row. */
export interface NativeRows {
  readonly n: number;
  readonly width: number;
  readonly values: unknown[];
}

/** How rows become instances: the root model's fields first, then each
 * `select_related` object (`parent` -1: attached to the root object). */
export interface NativeInstances {
  readonly model: string;
  readonly joins: readonly { parent: number; attr: string; model: string; start: number; pk: number }[];
}

/** `select()` rows: per item, the width of a model instance or -1 for one value. */
export interface NativeColumns {
  readonly model: string | null;
  readonly items: readonly number[];
}

export interface NativePrefetch {
  readonly attr: string;
  readonly many: boolean;
  readonly keyPos: number;
  readonly childKeyPos: number;
  readonly back: string | null;
  readonly rows: NativeRows;
  readonly output: NativeInstances;
  readonly children: readonly NativePrefetch[];
}

export interface NativeSelect {
  readonly rows: NativeRows;
  readonly output: NativeInstances | NativeColumns;
  readonly prefetched: readonly NativePrefetch[];
}

/** Rows a write returned (`RETURNING`). */
export interface NativeReturned {
  readonly model: string;
  readonly rows: NativeRows;
}

export interface NativeTransaction {
  commit(): Promise<void>;
  rollback(): Promise<void>;
}

export interface NativeSchema {
  sql(opJson: string, params: unknown[]): string;
  updateManySql(
    model: string,
    fields: string[],
    rows: unknown[][],
    filtersJson: string,
    params: unknown[],
    batchSize: number | null | undefined,
    disable: string[],
  ): string[];
  ddl(): string[];
  snapshot(): string;
  migration(previous?: string | null): string;
  planMigration(dir: string): string;
  makeMigration(dir: string, name?: string | null, empty?: boolean | null): string | null;
}

export interface NativeEngine {
  run(opJson: string, params: unknown[], tx: NativeTransaction | null): Promise<unknown>;
  insert(
    model: string,
    fields: string[],
    rows: unknown[][],
    conflict: string[] | null,
    update: string[] | null,
    set: string | null,
    params: unknown[],
    tx: NativeTransaction | null,
  ): Promise<unknown>;
  attach(model: string, parentId: unknown, fields: string[], rows: unknown[][], tx: NativeTransaction | null): Promise<unknown>;
  updateMany(
    model: string,
    fields: string[],
    rows: unknown[][],
    filtersJson: string,
    params: unknown[],
    returning: boolean,
    batchSize: number | null,
    tx: NativeTransaction | null,
  ): Promise<unknown>;
  begin(tx: NativeTransaction | null): Promise<NativeTransaction>;
  advisoryLock(key: string, name: Buffer | null, exclusive: boolean, nowait: boolean, tx: NativeTransaction): Promise<boolean>;
  execute(sql: string, tx: NativeTransaction | null): Promise<number>;
  fetchText(sql: string, tx: NativeTransaction | null): Promise<(string | null)[][]>;
  executeScript(statements: string[], tx: NativeTransaction | null): Promise<void>;
  migrationStatus(dir: string): Promise<string>;
  migrateUp(dir: string, target: string | null): Promise<string[]>;
  migrateDown(dir: string, steps: number, target: string | null): Promise<string[]>;
  createTables(): Promise<void>;
  dropTables(): Promise<void>;
  close(): Promise<void>;
}

interface Addon {
  Schema: new (schemaJson: string) => NativeSchema;
  connect(url: string, schema: NativeSchema, maxConnections: number, disable: string[]): Promise<NativeEngine>;
  prepareSchema(schemaJson: string, contextJson?: string): string;
  nativeArtifact(): string;
  compileSchema(source: string, path?: string | null): string;
  compileSchemaFile(path: string): string;
  generateTypescript(path: string, runtime?: string | null): string;
  setDecimalClass(ctor: unknown): void;
  cli(argv: string[]): Promise<number>;
  listMigrations(dir: string): string[][];
  findMigration(dir: string, name: string): string[];
}

function load(): Addon {
  const require = createRequire(import.meta.url);
  const candidates = [
    process.env["ORM_NATIVE"],
    // src/native.ts (run directly, e.g. by Bun) and dist/src/native.js
    fileURLToPath(new URL("../orm.node", import.meta.url)),
    fileURLToPath(new URL("../../orm.node", import.meta.url)),
  ];
  for (const path of candidates) {
    if (path && existsSync(path)) {
      const addon = require(path) as Addon;
      addon.setDecimalClass(Decimal);
      return addon;
    }
  }
  throw new Error("the orm native addon (orm.node) is missing; build it with `npm run build:native`");
}

let addon: Addon | undefined;

export function native(): Addon {
  addon ??= load();
  return addon;
}

/** Calls into the addon, turning its errors into the package's error classes. */
export function call<T>(f: () => T): T {
  try {
    return f();
  } catch (e) {
    throw fromNative(e);
  }
}

/** Awaits an addon promise, turning its errors into the package's error classes. */
export async function wait<T>(f: () => Promise<T>): Promise<T> {
  try {
    return await f();
  } catch (e) {
    throw fromNative(e);
  }
}
