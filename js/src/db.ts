/** Connections and transactions. */

import { AsyncLocalStorage } from "node:async_hooks";

import { LockNotAvailable, NotConnected, QueryError, TransactionRequired } from "./errors.js";
import type { IR } from "./expr.js";
import { registry as defaultRegistry, type Registry } from "./model.js";
import { call, native, wait, type NativeEngine, type NativeTransaction } from "./native.js";
import { allowedWrites } from "./protection.js";
import { active as debugging, record } from "./debug.js";

let defaultDb: Database | undefined;

/** For each database (its root) with an open transaction in the current async context: the innermost one. */
const current = new AsyncLocalStorage<ReadonlyMap<Database, NativeTransaction>>();
/** For each database in a `tenant()` call: its primary and replica engines with the tenant set. */
const tenants = new AsyncLocalStorage<ReadonlyMap<Database, { readonly primary: NativeEngine; readonly replicas: readonly NativeEngine[] }>>();
/** The `scope.<name>` values of default filters. */
const scopeValues = new AsyncLocalStorage<Readonly<Record<string, unknown>>>();
/** For each database with an open transaction: the onCommit callbacks of the innermost one. */
const callbacks = new AsyncLocalStorage<ReadonlyMap<Database, (() => unknown)[]>>();
/** The callback lists of transactions that committed or rolled back: a call started in the
 * transaction can still see its list after that. */
const ended = new WeakSet<(() => unknown)[]>();

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

/** Statements a replica may answer (the IR starts with its `op`). */
const READ = /^\{"op":"(select|count|exists)"/;

/** A connection pool. Created by {@link connect}. */
export class Database {
  private turn = 0;
  /** @internal The database a `primary` view belongs to: they share transactions and callbacks. */
  root: Database = this;

  /** @internal */
  constructor(
    private readonly base: NativeEngine,
    readonly url: string,
    readonly registry: Registry,
    private readonly replicas: readonly NativeEngine[] = [],
  ) {}

  /** @internal The primary's engine, with the tenant of an enclosing `tenant()` call. */
  get engine(): NativeEngine {
    return tenants.getStore()?.get(this.root)?.primary ?? this.base;
  }

  /** This database without its replicas: every statement goes to the primary. */
  get primary(): Database {
    const view = new Database(this.base, this.url, this.registry);
    view.root = this.root;
    return view;
  }

  /** @internal The transaction queries on this database run in, if any. */
  tx(): NativeTransaction | null {
    return current.getStore()?.get(this.root) ?? null;
  }

  /** @internal The engine for a read: the next replica outside a transaction, else the primary. */
  reader(): NativeEngine {
    if (this.replicas.length === 0 || this.tx() !== null) {
      return this.engine;
    }
    const replicas = tenants.getStore()?.get(this.root)?.replicas ?? this.replicas;
    this.turn = (this.turn + 1) % replicas.length;
    return replicas[this.turn]!;
  }

  /**
   * Runs `fn` with a tenant: every transaction on this database in it first runs
   * `SELECT set_config('app.tenant', <id>, true)` (`SET LOCAL`), so Postgres row-level
   * security policies can read `current_setting('app.tenant')`. A statement outside a
   * transaction runs in a transaction of its own. A transaction that is already open keeps
   * its setting. Gives what `fn` gives.
   */
  tenant<T>(id: string | number | bigint, fn: () => Promise<T>): Promise<T> {
    if (this.url.startsWith("sqlite://")) {
      throw new QueryError("db.tenant() sets a Postgres setting for row-level security; sqlite has none");
    }
    if (!["string", "number", "bigint"].includes(typeof id)) {
      throw new TypeError(`tenant id must be a string or a number, got ${String(id)}`);
    }
    const root = this.root, value = [String(id)];
    const engines = {
      primary: root.base.withSettings(["app.tenant"], value),
      replicas: root.replicas.map((r) => r.withSettings(["app.tenant"], value)),
    };
    return tenants.run(new Map(tenants.getStore() ?? []).set(root, engines), async () => await fn());
  }

  /** @internal */
  run(ir: IR, params: unknown[]): Promise<unknown> {
    return this.runJson(JSON.stringify(ir), params);
  }

  /** @internal */
  runJson(json: string, params: unknown[]): Promise<unknown> {
    [json, params] = withScope(json, params);
    if (debugging()) record(`run:${json}`, () => this.registry.native().statement(json, params));
    const engine = READ.test(json) ? this.reader() : this.engine;
    return wait(() => engine.run(json, params, this.tx(), allowedWrites()));
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
    const scoped = new Map(callbacks.getStore() ?? []).set(this.root, mine);
    let result: T;
    const txs = new Map(current.getStore() ?? []).set(this.root, tx);
    try {
      result = await current.run(txs, () => callbacks.run(scoped, fn));
    } catch (e) {
      ended.add(mine);
      await wait(() => tx.rollback());
      throw e;
    }
    ended.add(mine);
    await wait(() => tx.commit());
    await this.afterCommit(mine, outer);
    return result;
  }

  /** A released savepoint hands its callbacks to the enclosing transaction. */
  private async afterCommit(mine: readonly (() => unknown)[], outer: boolean): Promise<void> {
    if (!outer) {
      callbacks.getStore()!.get(this.root)!.push(...mine);
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
   * `transaction()` and the later ones do not run. A call that the transaction started and
   * that runs this after the transaction ended throws `TransactionRequired`.
   */
  async onCommit(fn: () => unknown): Promise<void> {
    const mine = callbacks.getStore()?.get(this.root);
    if (mine !== undefined && ended.has(mine)) {
      throw new TransactionRequired("onCommit(): the transaction of this call has ended");
    }
    if (mine !== undefined) {
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

  /** Closes the pools of the primary and of each replica. */
  async close(): Promise<void> {
    if (this.root !== this) {
      return this.root.close();
    }
    await wait(() => this.base.close());
    for (const replica of this.replicas) {
      await wait(() => replica.close());
    }
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
  /** URLs of read replicas: reads outside a transaction go to one of them, in turn;
   * writes and every statement in a transaction go to the primary. */
  readonly replicas?: readonly string[];
}

/**
 * Opens a connection pool. Import your model modules first: the schema is compiled from
 * the models registered at this point.
 */
export async function connect(url: string, options: ConnectOptions = {}): Promise<Database> {
  const reg = options.registry ?? defaultRegistry;
  const schema = reg.native();
  const open = (u: string) =>
    wait(() => call(() => native().connect(u, schema, options.maxConnections ?? 10, [...(options.disable ?? [])])));
  const engine = await open(url);
  const readers: NativeEngine[] = [];
  for (const replica of options.replicas ?? []) {
    readers.push(await open(replica));
  }
  const db = new Database(engine, url, reg, readers);
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

/**
 * Runs `fn` with the values that `scope.<name>` reads in default filters
 * (`@@query.filter("shop_id == scope.shop")`). A query on a model whose default filter
 * reads a value that no enclosing `scope()` sets throws `QueryError`. Inner values
 * replace outer ones. Gives what `fn` gives.
 */
export function scope<T>(values: Readonly<Record<string, unknown>>, fn: () => Promise<T>): Promise<T> {
  // `await` inside: a returned thenable (a query set) must run while the values are set.
  return scopeValues.run({ ...scopeValues.getStore(), ...values }, async () => await fn());
}

/** @internal `json` (a statement's IR, or with `key` a list wrapped under it) with the
 * scope's parameter indexes, and `params` with the scope's values appended. */
export function withScope(json: string, params: unknown[], key?: string): [string, unknown[]] {
  const values = scopeValues.getStore();
  if (values === undefined || Object.keys(values).length === 0) {
    return [json, params];
  }
  const names = Object.keys(values);
  const index = JSON.stringify(Object.fromEntries(names.map((n, i) => [n, params.length + i])));
  const op = key === undefined ? `${json.slice(0, -1)},"scope":${index}}` : `{"${key}":${json},"scope":${index}}`;
  return [op, [...params, ...names.map((n) => values[n])]];
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
