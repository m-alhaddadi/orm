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
 * Generating a migration (the Rust core) diffs the schema against the newest
 * `snapshot.json`, so it needs no database. The runner records applied migrations with
 * a checksum and refuses to continue if an applied file changed.
 */

import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

import type { Database } from "./db.js";
import { ORMError } from "./errors.js";
import { Registry } from "./model.js";
import { call, native, type NativeSchema } from "./native.js";

const TABLE = "orm_migrations";
/** Arbitrary constant: the advisory lock serializing concurrent migrators (the Python
 * package's too). */
const LOCK_ID = 0x6f726d6d;

export class MigrationError extends ORMError {
  override name = "MigrationError";
}

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

const lit = (text: string) => `'${text.replaceAll("'", "''")}'`;

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
    if (!existsSync(this.directory) || !statSync(this.directory).isDirectory()) {
      return [];
    }
    const found = readdirSync(this.directory)
      .sort()
      .filter((n) => /^\d{4}_/.test(n) && existsSync(join(this.directory, n, "up.sql")))
      .map((n) => new Migration(n, join(this.directory, n)));
    const numbers = found.map((m) => m.name.slice(0, 4));
    const dupes = [...new Set(numbers.filter((n, i) => numbers.indexOf(n) !== i))].sort();
    if (dupes.length) {
      throw new MigrationError(`several migrations share the number(s) ${dupes.join(", ")}; renumber them`);
    }
    return found;
  }

  get(name: string): Migration {
    const m = this.all().find((m) => m.name === name || m.name.slice(0, 4) === name.padStart(4, "0"));
    if (!m) {
      throw new MigrationError(`no migration ${JSON.stringify(name)} in ${this.directory}`);
    }
    return m;
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

export interface Status {
  readonly migration: Migration;
  readonly applied: boolean;
  readonly appliedAt: string | null;
}

/** Applies and reverts the migrations of a directory on a database. */
export class Migrator {
  constructor(
    readonly db: Database,
    readonly migrations: Migrations,
  ) {}

  private async ensureTable(): Promise<void> {
    await this.db.execute(
      `CREATE TABLE IF NOT EXISTS ${TABLE} (name text PRIMARY KEY, checksum text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now())`,
    );
  }

  private async applied(): Promise<Map<string, [string, string]>> {
    const rows = await this.db.fetchText(`SELECT name, checksum, applied_at::text FROM ${TABLE} ORDER BY name`);
    return new Map(rows.map((r) => [r[0]!, [r[1]!, r[2]!]]));
  }

  private async locked(): Promise<void> {
    await this.db.execute(`SELECT pg_advisory_xact_lock(${LOCK_ID})`);
  }

  async status(): Promise<Status[]> {
    await this.ensureTable();
    const applied = await this.applied();
    const out = this.migrations.all().map((m) => ({ migration: m, applied: applied.has(m.name), appliedAt: applied.get(m.name)?.[1] ?? null }));
    const known = new Set(out.map((s) => s.migration.name));
    const unknown = [...applied.keys()].filter((n) => !known.has(n)).sort();
    if (unknown.length) {
      throw new MigrationError(`the database has migrations missing from the directory: ${unknown.join(", ")}`);
    }
    return out;
  }

  private verify(statuses: Status[], applied: Map<string, [string, string]>): void {
    let pending: string | undefined;
    for (const s of statuses) {
      if (!s.applied) {
        pending ??= s.migration.name;
      } else if (pending) {
        throw new MigrationError(
          `${s.migration.name} is applied but the earlier ${pending} is not; renumber the unapplied migration after the applied ones`,
        );
      } else if (applied.get(s.migration.name)![0] !== s.migration.checksum) {
        throw new MigrationError(`${s.migration.name}/up.sql changed after it was applied`);
      }
    }
  }

  /**
   * Applies pending migrations up to and including `target` (default: all). Each runs in
   * its own transaction together with its bookkeeping row, so a failing migration leaves
   * the database at the previous one.
   */
  async upgrade(target?: string): Promise<Migration[]> {
    const statuses = await this.status();
    this.verify(statuses, await this.applied());
    let pending = statuses.filter((s) => !s.applied).map((s) => s.migration);
    if (target !== undefined) {
      const stop = this.migrations.get(target).name;
      pending = pending.filter((m) => m.name <= stop);
    }
    const done: Migration[] = [];
    for (const m of pending) {
      const ran = await this.db.transaction(async () => {
        await this.locked();
        if ((await this.applied()).has(m.name)) {
          return false; // applied concurrently
        }
        await this.db.execute(m.upSql);
        await this.db.execute(`INSERT INTO ${TABLE} (name, checksum) VALUES (${lit(m.name)}, ${lit(m.checksum)})`);
        return true;
      });
      if (ran) {
        done.push(m);
      }
    }
    return done;
  }

  /** Reverts the last `steps` applied migrations, or every one after `target` (`"zero"`
   * reverts all). */
  async downgrade(options: { readonly steps?: number; readonly target?: string } = {}): Promise<Migration[]> {
    const applied = (await this.status()).filter((s) => s.applied).map((s) => s.migration);
    let revert: Migration[];
    if (options.target !== undefined) {
      const keep = options.target === "zero" ? "" : this.migrations.get(options.target).name;
      revert = applied.filter((m) => m.name > keep);
    } else {
      const steps = options.steps ?? 1;
      revert = steps > 0 ? applied.slice(-steps) : [];
    }
    const done: Migration[] = [];
    for (const m of revert.reverse()) {
      await this.db.transaction(async () => {
        await this.locked();
        await this.db.execute(m.downSql);
        await this.db.execute(`DELETE FROM ${TABLE} WHERE name = ${lit(m.name)}`);
      });
      done.push(m);
    }
    return done;
  }
}
