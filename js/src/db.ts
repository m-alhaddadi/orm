/** Connections and transactions. */

import { AsyncLocalStorage } from "node:async_hooks";

import { NotConnected, QueryError, TransactionRequired } from "./errors.js";
import type { IR } from "./expr.js";
import { registry as defaultRegistry, type Registry } from "./model.js";
import { call, native, wait, type NativeEngine, type NativeTransaction } from "./native.js";

let defaultDb: Database | undefined;

/** The innermost open transaction of the current async context, and its database. */
const current = new AsyncLocalStorage<{ readonly db: Database; readonly tx: NativeTransaction }>();

export interface LockOptions {
  /** A shared lock (any number of shared holders, but no exclusive one). */
  readonly exclusive?: boolean;
  /** Give `false` instead of waiting when the lock is held. */
  readonly nowait?: boolean;
}

/** A connection pool. Created by {@link connect}. */
export class Database {
  /** @internal */
  constructor(
    readonly engine: NativeEngine,
    readonly url: string,
    readonly registry: Registry,
  ) {}

  /** @internal The transaction queries on this database run in, if any. */
  tx(): NativeTransaction | null {
    const c = current.getStore();
    return c !== undefined && c.db === this ? c.tx : null;
  }

  /** @internal */
  run(ir: IR, params: unknown[]): Promise<unknown> {
    const json = JSON.stringify(ir);
    return wait(() => this.engine.run(json, params, this.tx()));
  }

  /** @internal */
  runJson(json: string, params: unknown[]): Promise<unknown> {
    return wait(() => this.engine.run(json, params, this.tx()));
  }

  /**
   * Runs `fn` in a transaction: commits when it resolves, rolls back when it throws.
   * Queries on this database inside it (in this async context, and in work it starts)
   * run in the transaction. Nested calls use savepoints. Gives what `fn` gives.
   */
  async transaction<T>(fn: () => Promise<T>): Promise<T> {
    const tx = await wait(() => this.engine.begin(this.tx()));
    let result: T;
    try {
      result = await current.run({ db: this, tx }, fn);
    } catch (e) {
      await wait(() => tx.rollback());
      throw e;
    }
    await wait(() => tx.commit());
    return result;
  }

  /**
   * Takes an advisory lock on `key` until the transaction ends: a lock on a name rather
   * than on rows ("only one worker imports this file at a time").
   *
   * Waits for the lock unless `nowait`, which gives `false` instead of waiting. A string
   * key is hashed to a 64-bit one the way the Python package hashes it (the first 8 bytes
   * of its BLAKE2b digest, signed big-endian), so both lock the same name. Must run inside
   * `db.transaction()`.
   */
  async lock(key: bigint | number | string, options: LockOptions = {}): Promise<boolean> {
    if (this.url.startsWith("sqlite://")) {
      throw new QueryError("sqlite does not support advisory locks");
    }
    if (this.tx() === null) {
      throw new TransactionRequired(
        "db.lock() outside a transaction would release the lock at once; run it inside `db.transaction(...)`",
      );
    }
    let k: bigint;
    let name: Buffer | null = null;
    if (typeof key === "string") {
      name = Buffer.from(new TextEncoder().encode(key));
      k = 0n;
    } else if (typeof key === "bigint" || Number.isSafeInteger(key)) {
      k = BigInt(key);
      if (k !== BigInt.asIntN(64, k)) {
        throw new RangeError("lock key must fit in 64 bits");
      }
    } else {
      throw new TypeError(`lock key must be an integer or a string, got ${String(key)}`);
    }
    const { exclusive = true, nowait = false } = options;
    return wait(() => this.engine.advisoryLock(String(k), name, Boolean(exclusive), Boolean(nowait), this.tx()!));
  }

  /** Runs raw SQL (one or more statements); gives the number of rows affected. */
  execute(sql: string): Promise<number> {
    return wait(() => this.engine.execute(sql, this.tx()));
  }

  /** Raw query whose columns are all read as text. For tooling (migrations). */
  fetchText(sql: string): Promise<(string | null)[][]> {
    return wait(() => this.engine.fetchText(sql, this.tx()));
  }

  /**
   * Creates the whole schema (extensions, tables, constraints, indexes, functions,
   * triggers) with `IF NOT EXISTS` / `OR REPLACE` DDL, in one transaction. A development
   * and test helper: it never alters what exists. Evolving databases use migrations.
   */
  createTables(): Promise<void> {
    return wait(() => this.engine.createTables());
  }

  /** `DROP TABLE ... CASCADE` every model table and drop generated functions;
   * extensions stay. */
  dropTables(): Promise<void> {
    return wait(() => this.engine.dropTables());
  }

  async close(): Promise<void> {
    await wait(() => this.engine.close());
    if (defaultDb === this) {
      defaultDb = undefined;
    }
  }

  toString(): string {
    return `Database(${this.url.split("@").pop()})`;
  }
}

export interface ConnectOptions {
  readonly maxConnections?: number;
  /** Make this the database queries use unless `.using(db)` says otherwise. */
  readonly default?: boolean;
  /** The models to compile (the default registry unless given). */
  readonly registry?: Registry;
  /** Switch database capabilities off (`"ilike"`, `"update_from_values"`, ...) to test
   * the SQL other databases get. */
  readonly disable?: readonly string[];
}

/**
 * Opens a connection pool. Import your model modules first: the schema is compiled from
 * the models registered at this point.
 */
export async function connect(url: string, options: ConnectOptions = {}): Promise<Database> {
  const reg = options.registry ?? defaultRegistry;
  const schema = reg.native();
  const engine = await wait(() =>
    call(() => native().connect(url, schema, options.maxConnections ?? 10, [...(options.disable ?? [])])),
  );
  const db = new Database(engine, url, reg);
  if (options.default ?? true) {
    defaultDb = db;
  }
  return db;
}

/** The default database ({@link connect}ed last with `default: true`). */
export function getDatabase(): Database {
  if (defaultDb === undefined) {
    throw new NotConnected("no default database; call `await connect(url)` first");
  }
  return defaultDb;
}

/** `db`, or the default database. */
export function resolve(db: Database | undefined): Database {
  return db ?? getDatabase();
}
