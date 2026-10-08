/**
 * Debug tools: find N+1 query patterns.
 *
 * `debug.nPlusOne(fn, { threshold: 5, fail: true })` counts the queries `fn` sends by
 * statement shape. A shape that runs more than `threshold` times is an N+1: the report
 * names the shape, the count and the call site of the first query, and the fix when a
 * relation load sent the queries.
 */

import { AsyncLocalStorage } from "node:async_hooks";
import { fileURLToPath } from "node:url";
import { dirname, relative, sep } from "node:path";

import { ORMError } from "./errors.js";

/** One statement shape: how often it ran and where it ran first. */
export interface Shape {
  readonly key: string;
  readonly sql: string;
  count: number;
  readonly site: string;
  readonly fix: string | null;
}

/** Longer SQL, such as a chunk of 10 000 placeholders, is cut in the message. */
const sqlLimit = 500;

/** The queries of one {@link nPlusOne} scope, by shape. */
export class Report {
  readonly shapes = new Map<string, Shape>();
  constructor(readonly threshold: number) {}

  /** The shapes that ran more than `threshold` times, the most frequent first. */
  get repeated(): Shape[] {
    return [...this.shapes.values()].filter((s) => s.count > this.threshold).sort((a, b) => b.count - a.count);
  }

  message(): string {
    const cut = (sql: string) => (sql.length <= sqlLimit ? sql : `${sql.slice(0, sqlLimit)} ...`);
    return this.repeated.map((s) => `${s.count} queries with one shape \`${cut(s.sql)}\`\n  at ${s.site}${s.fix ? `; use ${s.fix}` : ""}`).join("\n");
  }
}

/** A query shape ran more than the threshold of {@link nPlusOne} times. */
export class NPlusOne extends ORMError {
  override name = "NPlusOne";
  constructor(readonly report: Report) {
    super(report.message());
  }
}

export interface NPlusOneOptions {
  /** The highest count of one shape that is not an N+1 (default 5). */
  readonly threshold?: number;
  /** Throw {@link NPlusOne} instead of a warning (default false). */
  readonly fail?: boolean;
}

const scope = new AsyncLocalStorage<Report>();
/** The reports of the enclosing scopes, which count the inner queries too. */
const outers = new AsyncLocalStorage<readonly Report[]>();
const hint = new AsyncLocalStorage<{ readonly fix: string; readonly site: string }>();
/** The shapes that the current internal ORM loop (batches, chunked inBulk) already counted. */
const loop = new AsyncLocalStorage<Set<string>>();
const here = dirname(fileURLToPath(import.meta.url)) + sep;

/**
 * Runs `fn` and counts its queries by statement shape (the SQL without its values).
 * When `fn` resolves, a shape that ran more than `threshold` times throws
 * {@link NPlusOne} with `fail`, or emits a process warning. Work that `fn` starts counts
 * too. The call site is captured only inside the scope, so code outside it pays one
 * `AsyncLocalStorage` read per query. Gives what `fn` gives.
 */
export async function nPlusOne<T>(fn: () => Promise<T>, options: NPlusOneOptions = {}): Promise<T> {
  const report = new Report(options.threshold ?? 5);
  if (!(report.threshold >= 1)) throw new RangeError("threshold must be at least 1");
  const current = scope.getStore();
  const enclosing = current ? [...(outers.getStore() ?? []), current] : (outers.getStore() ?? []);
  const result = await outers.run(enclosing, () => scope.run(report, fn));
  if (report.repeated.length) {
    if (options.fail) throw new NPlusOne(report);
    process.emitWarning(report.message(), { type: "NPlusOneWarning" });
  }
  return result;
}

/** A test helper: runs `fn` and throws {@link NPlusOne} if it sends an N+1. */
export function expectNoNPlusOne<T>(fn: () => Promise<T>, options: { readonly threshold?: number } = {}): Promise<T> {
  return nPlusOne(fn, { ...options, fail: true });
}

/** @internal Whether a scope is open. */
export function active(): boolean {
  return scope.getStore() !== undefined;
}

/** @internal The first stack frame outside the ORM: where user code sent the query. */
export function callSite(): string {
  for (const line of (new Error().stack ?? "").split("\n").slice(1)) {
    const m = /\(?((?:file:\/\/)?[^\s()]+):(\d+):\d+\)?$/.exec(line.trim());
    if (!m) continue;
    const file = m[1]!.startsWith("file://") ? fileURLToPath(m[1]!) : m[1]!;
    if (file.startsWith(here) || file.startsWith("node:") || !file.includes(sep)) continue;
    return `${relative(process.cwd(), file)}:${m[2]}`;
  }
  return "<unknown>";
}

/** @internal Counts each shape of `fn` once for the ORM loop that owns `seen`: its pages are one query to the user. */
export function internalLoop<T>(seen: Set<string>, fn: () => Promise<T>): Promise<T> {
  return loop.run(seen, fn);
}

/** @internal Marks the queries of a relation load, so the report names the fix. */
export function relationLoad<T>(model: string, relation: string, fix: string, fn: () => Promise<T>): Promise<T> {
  return hint.run({ fix: `${fix}(${model}.${relation})`, site: callSite() }, fn);
}

/** @internal Counts one query of shape `key`; `sql()` gives its SQL text when it is new. */
export function record(key: string, sql: () => string): void {
  const report = scope.getStore();
  if (report === undefined) return;
  const seen = loop.getStore();
  if (seen !== undefined) {
    if (seen.has(key)) return;
    seen.add(key);
  }
  let shape = report.shapes.get(key);
  if (shape === undefined) {
    const h = hint.getStore();
    shape = { key, sql: sql(), count: 0, site: h?.site ?? callSite(), fix: h?.fix ?? null };
    report.shapes.set(key, shape);
  }
  shape.count++;
  for (const outer of outers.getStore() ?? []) {
    let counted = outer.shapes.get(key);
    if (counted === undefined) {
      counted = { key, sql: shape.sql, count: 0, site: shape.site, fix: shape.fix };
      outer.shapes.set(key, counted);
    }
    counted.count++;
  }
}
