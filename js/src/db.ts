/** Connections and transactions. */

import { AsyncLocalStorage } from "node:async_hooks";

import { NotConnected, QueryError, TransactionRequired } from "./errors.js";
import type { IR } from "./expr.js";
import { registry as defaultRegistry, type Registry } from "./model.js";
import { call, native, wait, type NativeEngine, type NativeTrace, type NativeTransaction } from "./native.js";
import { allowedWrites } from "./protection.js";
import { active as debugging, capture, record } from "./debug.js";

let defaultDb: Database | undefined;

/** The innermost open transaction of the current async context, and its database. */
const current = new AsyncLocalStorage<{ readonly db: Database; readonly tx: NativeTransaction }>();

export interface LockOptions {
  /** A shared lock (any number of shared holders, but no exclusive one). */
  readonly exclusive?: boolean;
  /** Give `false` instead of waiting when the lock is held. */
  readonly nowait?: boolean;
}

/** One statement the database ran, given to the hooks of {@link Database.onQuery}. */
export interface QueryEvent {
  /** The SQL with its parameter placeholders: the statement shape, without values. */
  readonly sql: string;
  /** When the statement started, in Unix milliseconds (`Date.now()`). */
  readonly start: number;
  /** How long it ran, in milliseconds. */
  readonly duration: number;
  /** Rows returned, or rows affected by a statement that returns no rows. */
  readonly rows: number;
  /** The database's error message when the statement failed. */
  readonly error: string | null;
}

/** A connection pool. Created by {@link connect}. */
export class Database {
  readonly #hooks: ((event: QueryEvent) => unknown)[] = [];

  /** @internal */
  constructor(
    readonly engine: NativeEngine,
    readonly url: string,
    readonly registry: Registry,
  ) {}

  /**
   * Calls `hook(event)` after each statement this database runs (prefetch queries and
   * `updateMany` batches included), also when it fails. Gives a function that removes
   * the hook.
   *
   * The hook runs in the async context that sent the query, after the ORM call ends, so
   * `AsyncLocalStorage` values (a current span, a request id) are those of the caller.
   * An error the hook throws rejects that call.
   */
  onQuery(hook: (event: QueryEvent) => unknown): () => void {
    this.#hooks.push(hook);
    return () => {
      const i = this.#hooks.indexOf(hook);
      if (i >= 0) this.#hooks.splice(i, 1);
    };
  }

  /** @internal Runs an engine call in the current transaction; traced when a hook or an
   * N+1 scope listens. */
  send<T>(start: (tx: NativeTransaction | null, trace: NativeTrace | null) => Promise<T>): Promise<T> {
    const tx = this.tx();
    if (!this.#hooks.length && !debugging()) return wait(() => start(tx, null));
    const trace = new (native().Trace)();
    return this.#observe(trace, capture(), wait(() => start(tx, trace)));
  }

  async #observe<T>(trace: NativeTrace, origin: ReturnType<typeof capture>, pending: Promise<T>): Promise<T> {
    try {
      return await pending;
    } finally {
      const events: QueryEvent[] = trace.take().map((e) => ({ ...e, error: e.error ?? null }));
      if (origin) for (const e of events) record(e.sql, origin);
      for (const hook of [...this.#hooks]) for (const e of events) hook(e);
    }
  }

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
    const allowed = allowedWrites();
    return this.send((tx, trace) => this.engine.run(json, params, tx, allowed, trace));
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
    return this.send((tx, trace) => this.engine.advisoryLock(String(k), name, Boolean(exclusive), Boolean(nowait), tx!, trace));
  }

  /** Runs raw SQL (one or more statements); gives the number of rows affected. */
  execute(sql: string): Promise<number> {
    return this.send((tx, trace) => this.engine.execute(sql, tx, trace));
  }

  /**
   * Runs one raw SQL query with parameters; gives its rows as objects by column name.
   *
   * Placeholders are `$1, $2, ...` on Postgres and `?` on SQLite. A parameter's type
   * comes from its JS value (`bigint` and integer numbers are `bigint`, strings `text`,
   * plain objects and arrays JSON); cast in the SQL where the column needs another type
   * (`$1::uuid`). Cells come back by the column types the database reports. Runs in the
   * current transaction, and query hooks see it.
   */
  fetch(sql: string, ...params: unknown[]): Promise<Record<string, unknown>[]> {
    return this.send((tx, trace) => this.engine.fetch(sql, params, tx, trace));
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
