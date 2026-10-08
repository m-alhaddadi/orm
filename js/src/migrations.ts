/**
 * Migrations: generated from the schema file, stored as SQL, applied in order. The same
 * files and bookkeeping as the Python package (`orm_migrations`, SHA-256 checksums of
 * `up.sql`), so either can manage a database.
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
 * Adopting a live database: `pull()` writes its schema file, `Migrator.baseline()` marks
 * the first migration as applied without running it, and `Migrator.drift()` compares
 * the database with the newest migration's snapshot.
 */

import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import type { Database } from "./db.js";
import { MigrationError } from "./errors.js";
import { Registry } from "./model.js";
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

  /** Applies pending migrations up to and including `target` (default: all). */
  async upgrade(target?: string): Promise<Migration[]> {
    return this.named(await wait(() => this.db.engine.migrateUp(this.dir, target ?? null)));
  }

  /** Compares the database with the snapshot of the newest migration. The snapshot is
   * created in a shadow (a Postgres schema in a transaction that is rolled back, or an
   * in-memory SQLite database) and read back, so both sides use the database's text. */
  async drift(): Promise<Drift> {
    return JSON.parse(await wait(() => this.db.engine.migrationDrift(this.dir))) as Drift;
  }

  /** Marks the first migration as applied without running it, for a database that
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
