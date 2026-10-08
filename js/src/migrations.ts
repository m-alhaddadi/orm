/**
 * Migrations: generated from the schema file, stored as SQL, applied in order. The same
 * files and bookkeeping as the Python package (`orm_migrations`, SHA-256 checksums of
 * `up.sql`), so either can manage a database; a migration with a `data.py` needs Python,
 * one with a `data.ts` needs this package.
 *
 * ```
 * migrations/
 *   0001_initial/
 *     up.sql          forward DDL, a comment per step (and warnings)
 *     down.sql        reverse DDL
 *     snapshot.json   the database schema after up.sql
 * ```
 *
 * Generating a migration (`core/src/migrate`) diffs the schema against the newest
 * `snapshot.json`, so it needs no database. Applying and reverting
 * (`engine/src/migrate.rs`) records each migration with a checksum and refuses to
 * continue if an applied file changed. Both are the Rust code the `orm` command line and
 * the Python package run too.
 *
 * A live database under migrations: `pull()` writes its schema file, `Migrator.baseline()` marks
 * the first migration as applied (it does not run it), and `Migrator.drift()` compares
 * the database with the newest migration's snapshot.
 */

import { createHash } from "node:crypto";
import { existsSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

import { connect, inTransaction, type Database } from "./db.js";
import { MigrationError } from "./errors.js";
import { Registry, load, registry as defaultRegistry } from "./model.js";
import { call, native, wait, type NativeSchema } from "./native.js";

export { MigrationError };

export interface Step {
  readonly summary: string;
  readonly sql: string;
  readonly warning?: string | null;
}

export interface Plan {
  readonly up: readonly Step[];
  readonly down: readonly Step[];
  readonly snapshot: unknown;
}

export class Migration {
  constructor(
    readonly name: string,
    readonly path: string,
  ) {}

  get upSql(): string {
    return readFileSync(join(this.path, "up.sql"), "utf8");
  }

  get downSql(): string {
    return readFileSync(join(this.path, "down.sql"), "utf8");
  }

  get checksum(): string {
    return createHash("sha256").update(this.upSql, "utf8").digest("hex");
  }

  toString(): string {
    return this.name;
  }
}

/** A schema file path, a registry (e.g. of models from `load()`), or a compiled schema. */
export type SchemaSource = Registry | NativeSchema | string;

function nativeSchema(schema: SchemaSource): NativeSchema {
  if (schema instanceof Registry) {
    return schema.native();
  }
  if (typeof schema === "string") {
    return call(() => new (native().Schema)(native().compileSchemaFile(schema)));
  }
  return schema;
}

/** A migrations directory and the schema it migrates to. */
export class Migrations {
  constructor(
    readonly directory: string,
    private readonly source: SchemaSource,
  ) {}

  get schema(): NativeSchema {
    return nativeSchema(this.source);
  }

  all(): Migration[] {
    return call(() => native().listMigrations(this.directory)).map(([name, path]) => new Migration(name!, path!));
  }

  /** A migration by folder name or number (`"3"`, `"0003_add_tags"`). */
  get(name: string): Migration {
    const [found, path] = call(() => native().findMigration(this.directory, name));
    return new Migration(found!, path!);
  }

  /** What the next migration would contain (no `up` steps if the schema is unchanged). */
  plan(): Plan {
    return JSON.parse(call(() => this.schema.planMigration(this.directory))) as Plan;
  }

  /** Writes the next migration; `null` if there is nothing to migrate. `empty` writes one
   * even without schema changes (for data migrations or hand-written SQL). */
  make(name?: string, options: { readonly empty?: boolean } = {}): Migration | null {
    const folder = call(() => this.schema.makeMigration(this.directory, name ?? null, options.empty ?? false));
    return folder === null ? null : new Migration(folder, join(this.directory, folder));
  }
}

/** A schema file read from a live database (`pull()`). */
export class Pulled {
  constructor(
    readonly schema: string,
    /** What the schema leaves out (rules, views, unsupported types, ...), one line each. */
    readonly gaps: readonly string[],
    /** What a migration from `schema` would still change on the database; empty when the
     * schema reproduces it. */
    readonly differences: readonly Step[],
  ) {}

  write(path: string): void {
    writeFileSync(path, this.schema);
  }
}

/**
 * Reads the database `db` connects to (Postgres: its current schema) as a schema file,
 * and checks the result by creating it again in a shadow schema that is rolled back
 * (SQLite: an in-memory database).
 */
export async function pull(db: Database): Promise<Pulled> {
  const out = JSON.parse(await wait(() => db.engine.pullSchema())) as { schema: string; gaps: string[]; steps: Step[] };
  return new Pulled(out.schema, out.gaps, out.steps);
}

/** The live database against the newest migration's snapshot (`Migrator.drift()`). */
export interface Drift {
  /** The migration compared with, or `null` for an empty directory. */
  readonly migration: string | null;
  /** Steps that bring the database to the snapshot; empty when they match. */
  readonly steps: readonly Step[];
  /** Live objects drift does not compare (rules, views, ...). */
  readonly gaps: readonly string[];
}

type DataRun = (db: Database) => Promise<unknown>;

/** `run` of the migration's `data.ts`, or null without one. */
async function dataStep(m: Migration): Promise<DataRun | null> {
  const file = join(m.path, "data.ts");
  if (!existsSync(file)) return null;
  // the version query makes an edited file load again: ESM caches modules by URL
  const url = `${pathToFileURL(file).href}?v=${statSync(file).mtimeMs}`;
  let module: { run?: unknown };
  try {
    module = (await import(url)) as { run?: unknown };
  } catch (e) {
    if ((e as { code?: string }).code === "ERR_UNKNOWN_FILE_EXTENSION") {
      throw new MigrationError(`${file}: this runtime does not load TypeScript; use Node 22.18+ (type stripping) or Bun`);
    }
    throw e;
  }
  if (typeof module.run !== "function") throw new MigrationError(`${file}: needs \`export async function run(db)\``);
  return module.run as DataRun;
}

/** Whether a migration of `directory` has a `data.ts`. */
export function hasDataSteps(directory: string): boolean {
  return call(() => native().listMigrations(directory)).some(([, path]) => existsSync(join(path!, "data.ts")));
}

/** @internal `npx orm migrate` for a directory with data steps: applies with
 * `Migrator.upgrade()`. The data modules are imported first, so the models they import
 * are in the default registry when the database connects. */
export async function migrateCommand(schema: string, directory: string, url: string, target: string | null): Promise<number> {
  const dialect = url.startsWith("sqlite:") ? "sqlite" : undefined;
  const probe = await connect(url, { maxConnections: 1, default: false, registry: emptyRegistry(dialect) });
  let pending: Pending;
  try {
    pending = await new Migrator(probe, new Migrations(directory, schema)).loadPending(target);
  } finally {
    await probe.close();
  }
  if ([...defaultRegistry].length === 0 && existsSync(schema)) load(schema);
  // data steps that use db.execute only, and no schema file (e.g. a deploy image)
  const registry = [...defaultRegistry].length === 0 ? emptyRegistry(dialect) : defaultRegistry;
  const db = await connect(url, { maxConnections: 2, registry });
  try {
    const done = await new Migrator(db, new Migrations(directory, schema)).apply(pending);
    for (const m of done) process.stdout.write(`Applied ${m.name}\n`);
    if (done.length === 0) process.stdout.write("Nothing to apply.\n");
  } finally {
    await db.close();
  }
  return 0;
}

function emptyRegistry(dialect: string | undefined): Registry {
  const registry = new Registry();
  if (dialect !== undefined) registry.addExtra({ dialect, models: [] });
  return registry;
}

type Pending = [Migration, DataRun | null][];

export interface Status {
  readonly migration: Migration;
  readonly applied: boolean;
  readonly appliedAt: string | null;
}

/**
 * Applies and reverts the migrations of a directory on a database. Each migration runs
 * in its own transaction (on a connection of the pool, not in the caller's transaction)
 * together with its `orm_migrations` row, under an advisory lock: a failing migration
 * leaves the database at the previous one, and concurrent migrators apply each
 * migration once.
 */
export class Migrator {
  constructor(
    readonly db: Database,
    readonly migrations: Migrations,
  ) {}

  private get dir(): string {
    return this.migrations.directory;
  }

  private named(names: readonly string[]): Migration[] {
    return names.map((n) => new Migration(n, join(this.dir, n)));
  }

  async status(): Promise<Status[]> {
    const json = await wait(() => this.db.engine.migrationStatus(this.dir));
    const rows = JSON.parse(json) as { name: string; path: string; applied: boolean; appliedAt: string | null }[];
    return rows.map((r) => ({ migration: new Migration(r.name, r.path), applied: r.applied, appliedAt: r.appliedAt }));
  }

  /**
   * Applies pending migrations up to and including `target` (default: all).
   *
   * A migration folder may hold a `data.ts` with `export async function run(db)`. It runs
   * after `up.sql`, in the same transaction and under the same lock, before the migration
   * is recorded; queries on `db` inside it go into that transaction. An error rolls back
   * the SQL, the data changes and the record.
   */
  async upgrade(target?: string): Promise<Migration[]> {
    return this.apply(await this.loadPending(target ?? null));
  }

  /** @internal The pending migrations with their loaded data steps; refuses before anything runs. */
  async loadPending(target: string | null): Promise<Pending> {
    const todo = (await wait(() => this.db.engine.migrationPending(this.dir, target))).map(([name, path]) => new Migration(name!, path!));
    for (const m of todo) {
      if (existsSync(join(m.path, "data.py"))) {
        throw new MigrationError(`${m.name} has a data step (data.py); apply it with python -m orm migrate`);
      }
    }
    const out: Pending = [];
    for (const m of todo) out.push([m, await dataStep(m)]);
    return out;
  }

  /** @internal */
  async apply(pending: Pending): Promise<Migration[]> {
    const done: Migration[] = [];
    for (const [m, run] of pending) {
      const tx = await wait(() => this.db.engine.migrationBegin(m.name, m.path));
      if (tx === null) continue;
      let after: (() => unknown)[];
      try {
        after = await inTransaction(this.db, tx, async () => {
          if (run !== null) await run(this.db);
        });
      } catch (e) {
        await wait(() => tx.rollback()).catch(() => undefined);
        throw e;
      }
      await wait(() => this.db.engine.migrationFinish(tx, m.name, m.path));
      for (const fn of after) await fn();
      done.push(m);
    }
    return done;
  }

  /** Compares the database with the snapshot of the newest migration. The snapshot is
   * created in a shadow (a Postgres schema in a transaction that is rolled back, or an
   * in-memory SQLite database) and read back, so both sides use the database's text. */
  async drift(): Promise<Drift> {
    return JSON.parse(await wait(() => this.db.engine.migrationDrift(this.dir))) as Drift;
  }

  /** Marks the first migration as applied and does not run it, for a database that
   * already has the schema (after `pull()`). Writes the first migration from the schema
   * when the directory has none. Fails once any migration is applied. */
  async baseline(): Promise<Migration> {
    if (this.migrations.all().length === 0) {
      await this.status(); // fails first if the database has applied migrations
      this.migrations.make();
    }
    const name = await wait(() => this.db.engine.migrateBaseline(this.dir));
    return new Migration(name, join(this.dir, name));
  }

  /** Reverts the last `steps` applied migrations (default 1), or every one after
   * `target` (`"zero"` reverts all). */
  async downgrade(options: { readonly steps?: number; readonly target?: string } = {}): Promise<Migration[]> {
    const steps = Math.max(0, options.steps ?? 1);
    return this.named(await wait(() => this.db.engine.migrateDown(this.dir, steps, options.target ?? null)));
  }
}
