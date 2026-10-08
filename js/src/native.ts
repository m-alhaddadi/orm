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
export type NativeShape = readonly { field: number; slot: number; public: boolean }[] | null;

export interface NativeInstances {
  readonly shape?: NativeShape;
  readonly model: string;
  readonly joins: readonly { parent: number; attr: string; model: string; start: number; pk: number; shape?: NativeShape }[];
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
  readonly shape?: NativeShape;
  readonly model: string;
  readonly rows: NativeRows;
}

export interface NativeTransaction {
  commit(): Promise<void>;
  rollback(): Promise<void>;
}

export interface NativeSchema {
  validateInsert(model: string, fields: string[], rows: unknown[][]): void;
  uniqueRowUpdate(opJson: string, params: unknown[]): boolean;
  sql(opJson: string, params: unknown[]): string;
  statement(opJson: string, params: unknown[]): string;
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
  run(opJson: string, params: unknown[], tx: NativeTransaction | null, allowed?: readonly string[]): Promise<unknown>;
  insert(
    model: string,
    fields: string[],
    rows: unknown[][],
    conflict: string[] | null,
    update: string[] | null,
    set: string | null,
    params: unknown[],
    tx: NativeTransaction | null,
    allowed?: readonly string[],
  ): Promise<unknown>;
  attach(model: string, parentId: unknown, fields: string[], rows: unknown[][], tx: NativeTransaction | null, allowed?: readonly string[]): Promise<unknown>;
  updateMany(
    model: string,
    fields: string[],
    rows: unknown[][],
    filtersJson: string,
    params: unknown[],
    returning: boolean,
    batchSize: number | null,
    tx: NativeTransaction | null,
    withoutDefaults: boolean,
    allowed?: readonly string[],
  ): Promise<unknown>;
  begin(tx: NativeTransaction | null): Promise<NativeTransaction>;
  advisoryLock(key: string, name: Buffer | null, exclusive: boolean, nowait: boolean, tx: NativeTransaction): Promise<boolean>;
  execute(sql: string, tx: NativeTransaction | null): Promise<number>;
  fetchText(sql: string, tx: NativeTransaction | null): Promise<(string | null)[][]>;
  executeScript(statements: string[], tx: NativeTransaction | null): Promise<void>;
  migrationStatus(dir: string): Promise<string>;
  migrateUp(dir: string, target: string | null): Promise<string[]>;
  migrateDown(dir: string, steps: number, target: string | null): Promise<string[]>;
  pullSchema(): Promise<string>;
  migrationDrift(dir: string): Promise<string>;
  migrateBaseline(dir: string): Promise<string>;
  migrationPending(dir: string, target: string | null): Promise<string[][]>;
  migrationBegin(name: string, path: string): Promise<NativeTransaction | null>;
  migrationFinish(tx: NativeTransaction, name: string, path: string): Promise<void>;
  createTables(): Promise<void>;
  dropTables(): Promise<void>;
  close(): Promise<void>;
}

interface Addon {
  Schema: new (schemaJson: string) => NativeSchema;
  connect(url: string, schema: NativeSchema, maxConnections: number, disable: string[]): Promise<NativeEngine>;
  prepareSchema(schemaJson: string, contextJson?: string): string;
  nativeArtifact(): string;
  profileMetadata(): string;
  compileSchema(source: string, path?: string | null): string;
  compileSchemaFile(path: string): string;
  generateTypescript(path: string, runtime?: string | null): string;
  setDecimalClass(ctor: unknown): void;
  cli(argv: string[]): Promise<number>;
  cliMigrateArgs(argv: string[]): (string | null)[] | null;
  listMigrations(dir: string): string[][];
  findMigration(dir: string, name: string): string[];
}

const profiles: Record<string, readonly string[]> = {
  postgres: ["postgres"], sqlite: ["sqlite"], combined: ["postgres", "sqlite"], tooling: ["postgres", "sqlite"],
};

/** The artifact's embedded Cargo features, rustc, target, profile and git revision. */
function buildRecord(build: unknown): boolean {
  if (typeof build !== "object" || build === null || Array.isArray(build)) return false;
  const record = build as Record<string, unknown>;
  return Array.isArray(record["features"]) && record["features"].every(f => typeof f === "string") &&
    ["rustc", "target", "profile", "revision"].every(key => typeof record[key] === "string");
}

function validate(addon: Addon, profile?: string): void {
  if (typeof addon.profileMetadata !== "function") throw new Error("incompatible orm native artifact; rebuild for metadata ABI 1");
  const meta = JSON.parse(addon.profileMetadata()) as {
    abi: number; version: string; language: string; profile: string; backends: string[];
    capabilities: Record<string, boolean>; adapters: string[]; build: Record<string, unknown>;
  };
  if (typeof meta !== "object" || meta === null || Array.isArray(meta) || meta.abi !== 1 || meta.version !== "0.1.0" || meta.language !== "node" ||
      !buildRecord(meta.build) ||
      !Array.isArray(meta.backends) || meta.backends.length === 0 ||
      new Set(meta.backends).size !== meta.backends.length || meta.backends.some(b => !["postgres", "sqlite"].includes(b)) ||
      typeof meta.capabilities !== "object" || meta.capabilities === null ||
      Array.isArray(meta.capabilities) || Object.values(meta.capabilities).some(v => typeof v !== "boolean") ||
      !Array.isArray(meta.adapters) || meta.adapters.some(a => typeof a !== "string" || meta.capabilities[a] !== true) ||
      new Set(meta.adapters).size !== meta.adapters.length) {
    throw new Error("incompatible orm native artifact; rebuild or install matching orm 0.1.0 packages");
  }
  if (profile) {
    const capabilities = { cli: profile === "tooling", "generate-python": profile === "tooling",
      "generate-typescript": profile === "tooling", composition: false };
    if (meta.profile !== profile || meta.adapters.length !== 0 || JSON.stringify(meta.backends) !== JSON.stringify(profiles[profile]) ||
        Object.keys(capabilities).some(key => meta.capabilities[key] !== capabilities[key as keyof typeof capabilities]) ||
        Object.keys(meta.capabilities).length !== Object.keys(capabilities).length) {
      throw new Error(`incompatible orm native profile ${profile}; reinstall matching packages`);
    }
  }
}

function load(): Addon {
  const require = createRequire(import.meta.url);
  const selected = process.env["ORM_PROFILE"];
  if (selected !== undefined && !Object.hasOwn(profiles, selected)) {
    throw new Error(`unknown ORM_PROFILE ${selected}; choose ${Object.keys(profiles).join(", ")}`);
  }
  const explicit = process.env["ORM_NATIVE"];
  if (explicit) {
    const addon = require(explicit) as Addon;
    validate(addon, selected);
    addon.setDecimalClass(Decimal);
    return addon;
  }
  const available = Object.keys(profiles).filter(profile => {
    try { require.resolve(`@orm/native-${profile}`); return true; }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "MODULE_NOT_FOUND") throw error;
      return false;
    }
  });
  if (selected || available.length) {
    const profile = selected ?? (available.length === 1 ? available[0] : undefined);
    if (!profile || !available.includes(profile)) {
      throw new Error(`select one installed native profile with ORM_PROFILE; installed: ${available.join(", ") || "none"}`);
    }
    const addon = require(`@orm/native-${profile}`) as Addon;
    validate(addon, profile);
    addon.setDecimalClass(Decimal);
    return addon;
  }
  // Source checkout: the addon that `npm run build:native` writes.
  for (const path of [fileURLToPath(new URL("../orm.node", import.meta.url)),
                      fileURLToPath(new URL("../../orm.node", import.meta.url))]) {
    if (existsSync(path)) {
      const addon = require(path) as Addon;
      validate(addon);
      addon.setDecimalClass(Decimal);
      return addon;
    }
  }
  throw new Error("install @orm/native-postgres, @orm/native-sqlite, @orm/native-combined or @orm/native-tooling alongside orm");
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

/** Fixed adapter set. Bind specialized methods at module/definition initialization. */
let adapters: readonly string[] | undefined;
export function nativeAdapters(): readonly string[] {
  if (adapters === undefined) {
    const meta = JSON.parse(native().profileMetadata()) as { adapters: string[]; capabilities: Record<string, boolean> };
    if (!Array.isArray(meta.adapters) || meta.adapters.some(name => meta.capabilities[name] !== true)) {
      throw new Error("native adapter/capability metadata mismatch");
    }
    adapters = Object.freeze([...meta.adapters]);
  }
  return adapters;
}
