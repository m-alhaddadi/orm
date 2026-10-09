/** Build-selected reference loading adapter. Baseline profiles do not load this module. */
import { active as debugging, relationLoad } from "./debug.js";
import { IntegrityError, NotLoaded } from "./errors.js";
import type { Database } from "./db.js";
import type { ModelMeta, Row } from "./model.js";
import type { RelationMeta } from "./meta.js";

function targetFiltered(meta: ModelMeta, target: string): boolean {
  const behavior = meta.registry.ir()["behavior"] as { query_defaults?: { model: string; filter?: unknown }[] } | undefined;
  return behavior?.query_defaults?.some((policy) => policy.model === target && policy.filter != null) ?? false;
}

type Host = {
  DB: symbol; RELATED: symbol; INTERNAL: symbol;
  resolve(db: Database | undefined): Database;
  related(row: object): Record<string, unknown>;
  instanceUpdate(this: Row, values: object): Promise<void>;
  instanceRefresh(this: Row, ...args: unknown[]): Promise<boolean>;
};
const MISSING = Symbol("orm.missingReferenceKey");
function peekKey(row: Row, name: string, internal: symbol): unknown {
  if (Object.hasOwn(row, name)) return row[name];
  const values = row[internal] as Record<string, unknown> | undefined;
  return values && Object.hasOwn(values, name) ? values[name] : MISSING;
}
export function keyValue(row: Row, name: string, internal: symbol): unknown {
  const value = peekKey(row, name, internal);
  if (value === MISSING) throw new NotLoaded(`${name} is not loaded`);
  return value;
}

const REFERENCE_STATE: unique symbol = Symbol("orm.referenceState");
type ReferenceLoad = { context: readonly unknown[]; promise: Promise<unknown> };
type ReferenceState = { pending: Map<string, ReferenceLoad>; tokens: Map<string, object>; keys: Map<string, unknown> };
function referenceState(row: Row): ReferenceState {
  return ((row as ReferenceRow)[REFERENCE_STATE] ??= { pending: new Map(), tokens: new Map(), keys: new Map() });
}
export function referenceLoaderName(name: string): string {
  return `load${name[0]!.toUpperCase()}${name.slice(1)}`;
}

type ReferenceRow = Row & { [REFERENCE_STATE]?: ReferenceState };
/** One query per relation/key/context; successful loads enter the ordinary cache. */
async function loadReference(owner: Row, meta: ModelMeta, r: RelationMeta, reload: boolean, host: Host, filtered: boolean): Promise<unknown> {
  const from = meta.fieldByIr.get(r.from)!;
  const key = keyValue(owner, from.name, host.INTERNAL);
  if (!reload) {
    try { return owner[r.name]; }
    catch (error) { if (!(error instanceof NotLoaded)) throw error; }
  }
  const state = referenceState(owner);
  const db = host.resolve(owner[host.DB] as Database | undefined);
  const tx = db.tx();
  const target = meta.registry.get(r.target);
  const to = target.fieldByIr.get(r.to)!;
  const fetch = async (): Promise<unknown> => {
    const value = key === null ? null : await target.objects.using(db).filter(target.column(to).eq(key as never) as never).first();
    if (value === null && r.kind === "belongsTo" && !from.nullable && !filtered) {
      throw new IntegrityError(`${meta.name}.${r.name}: required target ${r.target} is missing`);
    }
    return value;
  };
  const load = () => coalescedLoad(owner, r.name, [key, db, tx], fetch, (value) => {
    if (keyValue(owner, from.name, host.INTERNAL) === key) {
      host.related(owner)[r.name] = value;
      state.keys.set(r.name, key);
    }
  });
  // The loader may share one running fetch, so the call site is captured here.
  return debugging() ? relationLoad(meta.name, r.name, "load", load) : load();
}

/** Internal seam: caller supplies resolved dependency keys plus database/transaction context. */
export function coalescedLoad(owner: Row, name: string, context: readonly unknown[], fetch: () => Promise<unknown>, publish: (value: unknown) => void): Promise<unknown> {
  const state = referenceState(owner);
  const running = state.pending.get(name);
  if (running && running.context.length === context.length && running.context.every((value, i) => value === context[i])) return running.promise;
  const token = {};
  state.tokens.set(name, token);
  const promise = fetch().then((value) => {
    if (state.tokens.get(name) === token) publish(value);
    return value;
  }).finally(() => {
    if (state.pending.get(name)?.promise === promise) state.pending.delete(name);
  });
  state.pending.set(name, { context, promise });
  return promise;
}

export function invalidateReference(owner: Row, name: string): void {
  const state = (owner as ReferenceRow)[REFERENCE_STATE];
  state?.keys.delete(name);
  state?.tokens.delete(name);
  state?.pending.delete(name);
}

export function getter(meta: ModelMeta, r: RelationMeta, baseline: (this: Row) => unknown, internal: symbol, related: symbol): (this: Row) => unknown {
  if (r.kind !== "belongsTo") return baseline;
  const from = meta.fieldByIr.get(r.from)!.name;
  const filtered = targetFiltered(meta, r.target);
  return function (this: Row): unknown {
    const key = keyValue(this, from, internal);
    const loaded = this[related] as Record<string, unknown> | undefined;
    if (loaded && Object.hasOwn(loaded, r.name)) {
      const value = loaded[r.name] as Row | null;
      const state = (this as ReferenceRow)[REFERENCE_STATE];
      if (value === null && (key === null || filtered || (meta.fieldByIr.get(r.from)!.nullable && !state?.keys.has(r.name)) || (state?.keys.has(r.name) && state.keys.get(r.name) === key))) return null;
      if (value !== null) {
        const targetKey = peekKey(value, meta.registry.get(r.target).fieldByIr.get(r.to)!.name, internal);
        if (targetKey === key || (targetKey === MISSING && state?.keys.has(r.name) && state.keys.get(r.name) === key)) return value;
      }
    } else if (key === null) return null;
    throw new NotLoaded(`${meta.name}.${r.name} is not loaded; use ${referenceLoaderName(r.name)}() or load(${meta.name}.${r.name})`);
  };
}

export function install(meta: ModelMeta, proto: Record<string, unknown>, host: Host): void {
  const refs = [...meta.relations.values()].filter((r) => r.kind === "belongsTo" || r.kind === "hasOne");
  for (const r of refs) {
    const filtered = targetFiltered(meta, r.target);
    const method = referenceLoaderName(r.name);
    if (meta.fields.has(method) || meta.relations.has(method) || Object.hasOwn(proto, method)) {
      throw new TypeError(`${meta.name}.${method}: reference loader collides with an existing member`);
    }
    Object.defineProperty(proto, method, { value(this: Row, options: { readonly reload?: boolean } = {}) {
      return loadReference(this, meta, r, options.reload ?? false, host, filtered);
    }});
  }
  if (!refs.length) return;
  const dependencies = refs.map((relation) => ({ relation, source: meta.fieldByIr.get(relation.from)!.name }));
  const invalidate = (row: Row, before: Map<string, unknown>, refresh: boolean): void => {
    for (const { relation: r, source: from } of dependencies) {
      if (!(refresh && r.kind === "hasOne") && before.get(from) === peekKey(row, from, host.INTERNAL)) continue;
      const loaded = row[host.RELATED] as Record<string, unknown> | undefined;
      if (loaded) delete loaded[r.name];
      invalidateReference(row, r.name);
    }
  };
  const keys = (row: Row): Map<string, unknown> => new Map(dependencies.map(({ source }) => [source, peekKey(row, source, host.INTERNAL)]));
  Object.defineProperty(proto, "update", { value: async function (this: Row, values: object) {
    const before = keys(this);
    await host.instanceUpdate.call(this, values);
    invalidate(this, before, false);
  }});
  Object.defineProperty(proto, "refresh", { value: async function (this: Row, ...args: unknown[]) {
    const before = keys(this);
    if (!(await host.instanceRefresh.apply(this, args))) return false;
    invalidate(this, before, true);
    return true;
  }});
}
