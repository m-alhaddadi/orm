/** Connections and transactions. */

import { AsyncLocalStorage } from "node:async_hooks";

import { LockNotAvailable, NotConnected, QueryError, TransactionRequired } from "./errors.js";
import type { IR } from "./expr.js";
import { registry as defaultRegistry, type Registry } from "./model.js";
import { call, native, wait, type NativeEngine, type NativeTransaction } from "./native.js";
import { allowedWrites } from "./protection.js";
import { active as debugging, record } from "./debug.js";

let defaultDb: Database | undefined;

/** The innermost open transaction of the current async context, and its database. */
const current = new AsyncLocalStorage<{ readonly db: Database; readonly tx: NativeTransaction }>();
/** For each database with an open transaction: the onCommit callbacks of the innermost one. */
const callbacks = new AsyncLocalStorage<ReadonlyMap<Database, (() => unknown)[]>>();

export interface LockOptions {
  /** A shared lock (any number of shared holders, but no exclusive one). */
  readonly exclusive?: boolean;
  /** Give `false` instead of waiting when the lock is held. */
  readonly nowait?: boolean;
  readonly session?: false;
}

export interface SessionLockOptions {
  /** Hold the lock while the function runs, on a connection of its own, outside any
   * transaction. */
  readonly session: true;
  /** A shared lock (any number of shared holders, but no exclusive one). */
  readonly exclusive?: boolean;
  /** Throw `LockNotAvailable` at once when the lock is held. */
  readonly nowait?: boolean;
  /** Seconds to wait before `LockNotAvailable` (no limit when absent). */
  readonly timeout?: number;
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
    return this.runJson(JSON.stringify(ir), params);
  }

  /** @internal */
  runJson(json: string, params: unknown[]): Promise<unknown> {
    if (debugging()) record(`run:${json}`, () => this.registry.native().statement(json, params));
    return wait(() => this.engine.run(json, params, this.tx(), allowedWrites()));
  }

  /**
   * Runs `fn` in a transaction: commits when it resolves, rolls back when it throws.
   * Queries on this database inside it (in this async context, and in work it starts)
   * run in the transaction. Nested calls use savepoints. Gives what `fn` gives.
   */
  async transaction<T>(fn: () => Promise<T>): Promise<T> {
    const outer = this.tx() === null;
    const tx = await wait(() => this.engine.begin(this.tx()));
    const mine: (() => unknown)[] = [];
    const scoped = new Map(callbacks.getStore() ?? []).set(this, mine);
    let result: T;
    try {
      result = await current.run({ db: this, tx }, () => callbacks.run(scoped, fn));
    } catch (e) {
      await wait(() => tx.rollback());
      throw e;
    }
    await wait(() => tx.commit());
    await this.afterCommit(mine, outer);
    return result;
  }

  /** A released savepoint hands its callbacks to the enclosing transaction. */
  private async afterCommit(mine: readonly (() => unknown)[], outer: boolean): Promise<void> {
    if (!outer) {
      callbacks.getStore()!.get(this)!.push(...mine);
      return;
    }
    for (const fn of mine) {
      await fn();
    }
  }

  /**
   * Calls `fn()` after the outermost transaction on this database commits; a rollback
   * drops it (a rolled-back savepoint drops only the callbacks registered inside it).
   * Outside a transaction, `fn()` runs at once. A promise result is awaited. Callbacks
   * run in order, outside the transaction; an error in one goes to the caller of
   * `transaction()` and the later ones do not run.
   */
  async onCommit(fn: () => unknown): Promise<void> {
    const mine = callbacks.getStore()?.get(this);
    if (mine !== undefined && this.tx() !== null) {
      mine.push(fn);
      return;
    }
    await fn();
  }

  /**
   * Takes an advisory lock on `key`: a lock on a name rather than on rows ("only one
   * worker imports this file at a time").
   *
   * `db.lock(key, options)` holds the lock until the transaction ends and must run inside
   * `db.transaction()`. It waits for the lock unless `nowait`, which gives `false` instead
   * of waiting.
   *
   * `db.lock(key, { session: true, timeout }, fn)` holds the lock while `fn` runs, on a
   * connection of its own, with no transaction, and gives what `fn` gives. It waits at
   * most `timeout` seconds (no limit when absent; not at all with `nowait`) and throws
   * `LockNotAvailable` when another session still holds the lock.
   *
   * A string key is hashed to a 64-bit one the way the Python package hashes it (the
   * first 8 bytes of its BLAKE2b digest, signed big-endian), so both lock the same name.
   */
  lock(key: bigint | number | string, options?: LockOptions): Promise<boolean>;
  lock<T>(key: bigint | number | string, options: SessionLockOptions, fn: () => Promise<T>): Promise<T>;
  async lock<T>(
    key: bigint | number | string,
    options: LockOptions | SessionLockOptions = {},
    fn?: () => Promise<T>,
  ): Promise<boolean | T> {
    if (options.session === true) {
      return this.sessionLock(key, options, fn!);
    }
    if (this.url.startsWith("sqlite://")) {
      throw new QueryError("sqlite does not support advisory locks");
    }
    if (this.tx() === null) {
      throw new TransactionRequired(
        "db.lock() outside a transaction would release the lock at once; run it inside `db.transaction(...)` or use `{ session: true }`",
      );
    }
    const [k, name] = lockKey(key);
    const { exclusive = true, nowait = false } = options;
    return wait(() => this.engine.advisoryLock(k, name, Boolean(exclusive), Boolean(nowait), this.tx()!));
  }

  private async sessionLock<T>(key: bigint | number | string, options: SessionLockOptions, fn: () => Promise<T>): Promise<T> {
    if (this.url.startsWith("sqlite://")) {
      throw new QueryError("sqlite does not support advisory locks");
    }
    if (typeof fn !== "function") {
      throw new TypeError("db.lock(key, { session: true }, fn) needs the function to run under the lock");
    }
    const [k, name] = lockKey(key);
    const { exclusive = true, nowait = false, timeout } = options;
    if (timeout !== undefined && !(timeout >= 0)) {
      throw new RangeError("lock timeout must be a number of seconds >= 0");
    }
    const timeoutMs = timeout === undefined ? null : Math.ceil(timeout * 1000);
    const held = await wait(() => this.engine.sessionLock(k, name, Boolean(exclusive), Boolean(nowait), timeoutMs));
    if (held === null) {
      const after = nowait || timeout === undefined ? "" : ` after ${timeout}s`;
      throw new LockNotAvailable(`advisory lock ${JSON.stringify(String(key))} is held by another session${after}`);
    }
    try {
      return await fn();
    } finally {
      await wait(() => held.release());
    }
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

/** A validated lock key: a decimal 64-bit integer, or the UTF-8 name the engine hashes. */
function lockKey(key: bigint | number | string): [string, Buffer | null] {
  if (typeof key === "string") {
    return ["0", Buffer.from(new TextEncoder().encode(key))];
  }
  if (typeof key === "bigint" || Number.isSafeInteger(key)) {
    const k = BigInt(key);
    if (k !== BigInt.asIntN(64, k)) {
      throw new RangeError("lock key must fit in 64 bits");
    }
    return [String(k), null];
  }
  throw new TypeError(`lock key must be an integer or a string, got ${String(key)}`);
}

/** `db`, or the default database. */
export function resolve(db: Database | undefined): Database {
  return db ?? getDatabase();
}
