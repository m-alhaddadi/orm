/**
 * Models: the registry, models built from a compiled schema (`define()`), and the
 * instances queries return.
 *
 * Generated `models.ts` modules call `define()` with the schema IR and give the result
 * its static types (`User`, `UserModel`, `UserSpec`, ...). `load("schema.prisma")` does
 * the same at runtime without a generated file (untyped).
 */

import { DoesNotExist, MultipleObjectsReturned, NotLoaded } from "./errors.js";
import { Column, PATH, RelationPath, type PathState, type Source } from "./expr.js";
import { camel, type ColType, type FieldMeta, type ModelSpec, type RelationKind, type RelationMeta } from "./meta.js";
import { call, native, type NativeSchema } from "./native.js";
import { resolve, type Database } from "./db.js";
import type { ManyRelatedSet, QuerySet, RelatedSet } from "./query.js";

/** Where an instance keeps its loaded relations. */
export const RELATED: unique symbol = Symbol("orm.related");
/** The database an instance was read from (`using(db)`), which it writes back to. */
export const DB: unique symbol = Symbol("orm.db");
/** Hidden values (such as relation keys) that adapters read but ordinary property access does not show. */
export const INTERNAL: unique symbol = Symbol.for("orm.internal");
// Adapter selection happens once; baseline artifacts never import its runtime module.
const referenceAdapter = (JSON.parse(native().nativeArtifact()) as { capabilities: string[] }).capabilities.includes("reference-loading")
  ? await import("./references.js") : undefined;

export type IRField = {
  name: string;
  column: string;
  type: ColType;
  nullable?: boolean;
  array?: boolean;
  enum?: string;
  primary_key?: boolean;
  auto_increment?: boolean;
  hints?: Readonly<Record<string, string>>;
  unique?: boolean;
  default?: unknown;
  default_now?: boolean;
  default_sql?: string;
  [key: string]: unknown;
};

export type IRRelation = {
  name: string;
  kind: "one" | "many";
  target: string;
  from: string;
  to: string;
  foreign_key?: boolean;
  through?: { model: string; source: string; target: string };
  [key: string]: unknown;
};

export type IRModel = { name: string; table: string; fields: IRField[]; relations?: IRRelation[]; [key: string]: unknown };
export type IREnum = { name: string; storage?: string; values: { name: string; value: string | number }[]; [key: string]: unknown };
export type SchemaIR = { models: IRModel[]; enums?: IREnum[]; [key: string]: unknown };

/** A model, as the types see it: the generated `UserModel` adds its columns and
 * relation paths. */
export interface ModelClass<M extends ModelSpec> {
  /** The root query set of the model. */
  readonly objects: QuerySet<M>;
  /** Schema information about the model. */
  readonly _meta: ModelMeta;
  /** `get()` found no row; `instanceof User.DoesNotExist`. */
  readonly DoesNotExist: new (message: string) => DoesNotExist;
  readonly MultipleObjectsReturned: new (message: string) => MultipleObjectsReturned;
  /** @internal */
  readonly "~spec"?: M;
}

/** What every instance has, besides its columns and relations. Instances are snapshots
 * of a row: columns are read-only, writes are explicit statements. */
export interface Instance<M extends ModelSpec> {
  /** The primary key's value. */
  readonly pk: M["pk"];
  /**
   * `UPDATE ... SET <values> WHERE pk = ... RETURNING *`. Values may be expressions
   * (`{ views: Post.views.add(1) }`); the instance is refreshed from the returned row, so
   * it shows what the database stored.
   */
  update(values: M["update"]): Promise<void>;
  /** `DELETE ... WHERE pk = ...`. The instance keeps its last values. */
  delete(): Promise<void>;
  /** Reload column values from the database. */
  refresh(): Promise<void>;
}

export type Row = Record<PropertyKey, unknown> & { [RELATED]?: Record<string, unknown>; [DB]?: Database | undefined };

/** Schema information about one model (`User._meta`). */
export class ModelMeta implements Source {
  declare attachInputFields?: readonly FieldMeta[];
  readonly fields = new Map<string, FieldMeta>();
  readonly fieldByIr = new Map<string, FieldMeta>();
  readonly fieldList: FieldMeta[] = [];
  inputFields: ReadonlyMap<string, FieldMeta> = this.fields;
  inputFieldList: readonly FieldMeta[] = this.fieldList;
  readonly relations = new Map<string, RelationMeta>();
  readonly relationByIr = new Map<string, RelationMeta>();
  readonly pk: FieldMeta;
  /** Makes instances: `new meta.Row()`, then the fields are assigned in schema order. */
  readonly Row: new () => Row;
  /** The prototype of paths that reach this model (`User.posts` for `Post`). */
  readonly pathProto: object;
  readonly model: ModelClass<ModelSpec> & Record<string, unknown>;
  /** @internal */
  objects!: QuerySet<ModelSpec>;

  constructor(
    readonly ir: IRModel,
    readonly registry: Registry,
  ) {
    for (const f of ir.fields) {
      const fm: FieldMeta = {
        name: camel(f.name),
        ir: f.name,
        type: f.type,
        nullable: f.nullable ?? false,
        array: f.array ?? false,
        enumName: f.enum,
        primaryKey: f.primary_key ?? false,
        unique: f.unique ?? false,
        hasServerValue: !!(f.auto_increment || f.default !== undefined || f.default_now || f.default_sql || f.hints?.["composition.key-default"] === "true"),
      };
      checkName(ir.name, fm.name, this.fields);
      this.fields.set(fm.name, fm);
      this.fieldByIr.set(fm.ir, fm);
      this.fieldList.push(fm);
    }
    if (ir.fields.some((f) => f.primary_key && f.hints?.["composition.child"] === "true")) {
      this.attachInputFields = ir.fields.filter((f) => f.hints?.["composition.local"] === "true").map((f) => this.fieldByIr.get(f.name)!);
    }
    for (const r of ir.relations ?? []) {
      const kind: RelationKind = r.through
        ? "manyToMany"
        : r.kind === "many"
          ? "hasMany"
          : r.foreign_key
            ? "belongsTo"
            : "hasOne";
      const rm: RelationMeta = { name: camel(r.name), ir: r.name, kind, target: r.target, from: r.from, to: r.to, through: r.through };
      if (this.fields.has(rm.name)) {
        throw new TypeError(`${ir.name}.${rm.name} is both a field and a relation`);
      }
      checkName(ir.name, rm.name, this.relations);
      this.relations.set(rm.name, rm);
      this.relationByIr.set(rm.ir, rm);
    }
    const pks = this.fieldList.filter((f) => f.primaryKey);
    if (pks.length !== 1) {
      throw new TypeError(`model ${ir.name} must have exactly one primary key`);
    }
    this.pk = pks[0]!;
    this.pathProto = this.makePathProto();
    this.Row = this.makeRow();
    const model = Object.create(this.pathProto) as Record<PropertyKey, unknown>;
    model[PATH] = { root: this, path: [], names: [], target: ir.name } satisfies PathState;
    const name = ir.name;
    model["_meta"] = this;
    model["DoesNotExist"] = { [`${name}.DoesNotExist`]: class extends DoesNotExist {} }[`${name}.DoesNotExist`];
    model["MultipleObjectsReturned"] = {
      [`${name}.MultipleObjectsReturned`]: class extends MultipleObjectsReturned {},
    }[`${name}.MultipleObjectsReturned`];
    Object.defineProperty(model, "objects", { get: () => this.objects, enumerable: true });
    Object.defineProperty(model, Symbol.toStringTag, { value: name });
    this.model = model as never;
  }

  get name(): string {
    return this.ir.name;
  }

  get table(): string {
    return this.ir.table;
  }

  field(name: string): FieldMeta {
    const f = this.fields.get(name);
    if (!f) {
      throw new TypeError(`${this.name} has no field ${JSON.stringify(name)}`);
    }
    return f;
  }

  /** A column of this model, at the root of its own queries. */
  column(f: FieldMeta): Column<unknown, string> {
    return new Column(this, [], f, `${this.name}.${f.name}`);
  }

  private makePathProto(): object {
    const proto = Object.create(RelationPath.prototype) as object;
    for (const f of this.fieldList) {
      Object.defineProperty(proto, f.name, {
        get(this: RelationPath<ModelSpec, string, never>) {
          const s = this[PATH];
          return new Column(s.root, s.path, f, [s.root.name, ...s.names, f.name].join("."));
        },
        enumerable: true,
      });
    }
    const registry = this.registry;
    for (const r of this.relations.values()) {
      Object.defineProperty(proto, r.name, {
        get(this: RelationPath<ModelSpec, string, never>) {
          const s = this[PATH];
          const p = Object.create(registry.get(r.target).pathProto) as Record<PropertyKey, unknown>;
          p[PATH] = { root: s.root, path: [...s.path, r.ir], names: [...s.names, r.name], target: r.target } satisfies PathState;
          return p;
        },
        enumerable: true,
      });
    }
    return proto;
  }

  private makeRow(): new () => Row {
    const meta = this;
    const proto: Record<string, unknown> = {};
    for (const r of this.relations.values()) {
      const baseline = relationGetter(meta, r);
      Object.defineProperty(proto, r.name, {
        get: referenceAdapter ? referenceAdapter.getter(meta, r, baseline, INTERNAL, RELATED) : baseline,
        enumerable: false,
      });
    }
    Object.defineProperties(proto, {
      pk: {
        get(this: Row) {
          return this[meta.pk.name];
        },
      },
      update: { value: instanceUpdate, writable: true, configurable: true },
      delete: { value: instanceDelete, writable: true },
      refresh: { value: instanceRefresh, writable: true, configurable: true },
      toJSON: {
        value(this: Row) {
          const out: Record<string, unknown> = {};
          for (const f of meta.fieldList) {
            out[f.name] = this[f.name];
          }
          return out;
        },
        writable: true,
      },
      [Symbol.for("nodejs.util.inspect.custom")]: {
        value(this: Row) {
          const shown = meta.fieldList.map((f) => `${f.name}: ${show(this[f.name])}`).join(", ");
          return `${meta.name} { ${shown} }`;
        },
      },
      [Symbol.toStringTag]: { value: meta.name },
    });
    referenceAdapter?.install(meta, proto, { DB, RELATED, INTERNAL, resolve, related, instanceUpdate, instanceRefresh });
    // A plain constructor: V8 gives every instance of a model the same shape.
    const Row = function (this: Row) {} as unknown as new () => Row;
    (Row as unknown as { prototype: object }).prototype = proto;
    Object.defineProperty(Row, "name", { value: meta.name });
    (proto as { constructor?: unknown }).constructor = Row;
    return Row;
  }

  /** The instance for the row in `values` starting at `start`. */
  instance(values: unknown[], start: number, db: Database | undefined): Row {
    const o = new this.Row();
    const fields = this.fieldList;
    for (let i = 0; i < fields.length; i++) {
      o[fields[i]!.name] = values[start + i];
    }
    if (db !== undefined) {
      o[DB] = db;
    }
    return o;
  }

  toString(): string {
    return this.name;
  }
}

/** Names the runtime gives meaning to on models / instances. */
const RESERVED = new Set([
  "objects", "_meta", "DoesNotExist", "MultipleObjectsReturned", // models
  "pk", "update", "delete", "refresh", "toJSON", "constructor", "toString", "then", // instances
]);

function checkName(model: string, name: string, seen: Map<string, unknown>): void {
  if (RESERVED.has(name)) {
    throw new TypeError(`${model}.${name}: the name ${name} is reserved in TypeScript models; rename it in the schema`);
  }
  if (seen.has(name)) {
    throw new TypeError(`${model} has two members named ${name} in TypeScript (camelCase)`);
  }
}

function show(v: unknown): string {
  if (typeof v === "bigint") {
    return `${v}n`;
  }
  if (v instanceof Date) {
    return v.toISOString();
  }
  return typeof v === "string" ? JSON.stringify(v) : String(v);
}

/** Loaded relations of an instance. */
export function related(o: object): Record<string, unknown> {
  const r = o as Row;
  return (r[RELATED] ??= {});
}

function relationGetter(meta: ModelMeta, r: RelationMeta): (this: Row) => unknown {
  const registry = meta.registry;
  const loadHint = `select it with selectRelated(${meta.name}.${r.name}) or prefetchRelated(${meta.name}.${r.name})`;
  switch (r.kind) {
    case "belongsTo": {
      const via = meta.fieldByIr.get(r.from)!.name;
      return function (this) {
        const key = this[via];
        const loaded = this[RELATED];
        if (loaded && r.name in loaded) {
          const value = loaded[r.name] as Row | null;
          const to = registry.get(r.target).fieldByIr.get(r.to)!.name;
          if (value === null ? key === null : value[to] === key) {
            return value;
          }
        } else if (key === null) {
          return null;
        }
        throw new NotLoaded(`${meta.name}.${r.name} is not loaded; ${loadHint}`);
      };
    }
    case "hasOne":
      return function (this) {
        const loaded = this[RELATED];
        if (loaded && r.name in loaded) {
          return loaded[r.name];
        }
        throw new NotLoaded(`${meta.name}.${r.name} is not loaded; ${loadHint}`);
      };
    case "hasMany":
      return function (this) {
        return relatedSet(r, this);
      };
    case "manyToMany":
      return function (this) {
        return manyRelatedSet(r, this);
      };
  }
}

// query.ts registers these (it imports this module, not the other way round).
let relatedSet: (r: RelationMeta, instance: object) => RelatedSet<ModelSpec, never>;
let manyRelatedSet: (r: RelationMeta, instance: object) => ManyRelatedSet<ModelSpec>;
let makeQuerySet: (meta: ModelMeta) => QuerySet<ModelSpec>;

/** @internal */
export function registerQueries(
  qs: typeof makeQuerySet,
  rs: typeof relatedSet,
  ms: typeof manyRelatedSet,
): void {
  makeQuerySet = qs;
  relatedSet = rs;
  manyRelatedSet = ms;
}

// -- instance methods ---------------------------------------------------------------------------

function metaOf(o: Row): ModelMeta {
  return (o.constructor as unknown as { meta: ModelMeta }).meta;
}

function rowQuery(o: Row): QuerySet<ModelSpec> {
  const meta = metaOf(o);
  return meta.objects.using(o[DB]).filter(meta.column(meta.pk).eq(o[meta.pk.name] as never) as never) as never;
}

function apply(o: Row, fresh: Row): void {
  for (const f of metaOf(o).fieldList) {
    o[f.name] = fresh[f.name];
  }
}

async function instanceUpdate(this: Row, values: object): Promise<void> {
  if (!Object.keys(values).length) {
    return;
  }
  const rows = (await rowQuery(this).update(values as never, { returning: true })) as Row[];
  if (!rows.length) {
    const meta = metaOf(this);
    throw new (meta.model.DoesNotExist)(`${meta.name} ${show(this[meta.pk.name])} no longer exists`);
  }
  apply(this, rows[0]!);
}

async function instanceDelete(this: Row): Promise<void> {
  await rowQuery(this).delete();
}

async function instanceRefresh(this: Row): Promise<void> {
  apply(this, (await rowQuery(this).get()) as Row);
}

// -- registry -------------------------------------------------------------------------------------

/**
 * A set of models compiled together into one native schema. Models join the default
 * {@link registry} unless `define(..., { registry })` says otherwise, which is how one
 * process can hold two versions of a schema (e.g. in migration tests).
 */
export class Registry {
  private readonly models = new Map<string, ModelMeta>();
  private readonly enums = new Map<string, IREnum>();
  private dialect: string | undefined;
  private readonly extra: Record<string, unknown[]> = {};
  private behavior: Record<string, unknown> = {};
  private nativeSchema: NativeSchema | undefined;

  get(name: string): ModelMeta {
    const m = this.models.get(name);
    if (!m) {
      throw new TypeError(`unknown model ${JSON.stringify(name)}; is its module imported?`);
    }
    return m;
  }

  has(name: string): boolean {
    return this.models.has(name);
  }

  [Symbol.iterator](): IterableIterator<ModelMeta> {
    return this.models.values();
  }

  /** @internal */
  add(meta: ModelMeta): void {
    if (this.models.has(meta.name)) {
      throw new TypeError(`a model named ${meta.name} is already registered`);
    }
    this.models.set(meta.name, meta);
    this.nativeSchema = undefined;
  }

  /** @internal */
  addEnum(e: IREnum): void {
    const known = this.enums.get(e.name);
    if (known && JSON.stringify(known) !== JSON.stringify(e)) {
      throw new TypeError(`an enum named ${e.name} is already registered`);
    }
    this.enums.set(e.name, e);
    this.nativeSchema = undefined;
  }

  /** @internal */
  addExtra(ir: SchemaIR): void {
    const dialect = (ir.dialect as string | undefined) ?? "postgres";
    if (this.dialect !== undefined && this.dialect !== dialect) {
      throw new TypeError("schemas in one registry must target the same database");
    }
    this.dialect = dialect;
    for (const key of ["extensions", "functions", "catalog"]) {
      const items = (ir[key] as unknown[] | undefined) ?? [];
      const known = (this.extra[key] ??= []);
      for (const x of items) {
        if (!known.some((k) => JSON.stringify(k) === JSON.stringify(x))) {
          known.push(x);
        }
      }
      if (!known.length) {
        delete this.extra[key];
      }
    }
    this.nativeSchema = undefined;
  }

  /** @internal: normalized context is owned by this candidate. */
  setBehavior(value: unknown): void { this.behavior = structuredClone((value ?? {}) as Record<string, unknown>); }

  /** The schema IR of every registered model. */
  ir(): SchemaIR {
    const out: SchemaIR = { models: [...this.models.values()].map((m) => m.ir) };
    if (this.enums.size) {
      out.enums = [...this.enums.values()];
    }
    return JSON.parse(JSON.stringify({ ...out, ...(this.dialect === undefined ? {} : { dialect: this.dialect }), ...this.extra, ...(Object.keys(this.behavior).length ? { behavior: this.behavior } : {}) })) as SchemaIR;
  }

  /** Prepare the entire registered dependency batch. */
  prepare(): NativeSchema {
    return this.native();
  }

  /** @internal */
  candidate(): Registry {
    const next = new Registry();
    for (const [k, v] of this.models) next.models.set(k, v);
    for (const [k, v] of this.enums) next.enums.set(k, v);
    next.dialect = this.dialect;
    next.behavior = structuredClone(this.behavior);
    for (const [k, v] of Object.entries(this.extra)) next.extra[k] = [...v];
    return next;
  }

  /** @internal: called only after preparation succeeds. */
  publish(next: Registry): void {
    this.models.clear();
    for (const [k, v] of next.models) this.models.set(k, v);
    this.enums.clear();
    for (const [k, v] of next.enums) this.enums.set(k, v);
    this.dialect = next.dialect;
    this.behavior = structuredClone(next.behavior);
    for (const k of Object.keys(this.extra)) delete this.extra[k];
    for (const [k, v] of Object.entries(next.extra)) this.extra[k] = [...v];
    this.nativeSchema = next.nativeSchema;
  }

  /** The compiled native schema (cached until models change). */
  native(): NativeSchema {
    this.nativeSchema ??= call(() => new (native().Schema)(JSON.stringify(this.ir())));
    return this.nativeSchema;
  }
}

/** The default registry. */
export const registry = new Registry();

/**
 * Builds the models of a compiled schema (the JSON IR `orm compile` and {@link load}
 * produce). Generated `models.ts` modules call this and type the result.
 */
export function define(
  schema: string | SchemaIR,
  options: { readonly registry?: Registry; readonly requiredCapabilities?: readonly string[] } = {},
): Record<string, ModelClass<ModelSpec> & Record<string, unknown>> {
  const available = (JSON.parse(native().nativeArtifact()) as { capabilities: string[] }).capabilities;
  for (const capability of options.requiredCapabilities ?? []) {
    if (!available.includes(capability)) throw new TypeError(`generated models require ${capability}; rebuild/select a compatible native artifact`);
  }
  let ir: SchemaIR = JSON.parse(typeof schema === "string" ? schema : JSON.stringify(schema)) as SchemaIR;
  const destination = options.registry ?? registry;
  const context = [...destination].length ? JSON.stringify(destination.ir()) : undefined;
  ir = JSON.parse(call(() => native().prepareSchema(JSON.stringify(ir), context))) as SchemaIR;
  const reg = destination.candidate();
  reg.addExtra(ir);
  reg.setBehavior(ir.behavior);
  for (const e of ir.enums ?? []) {
    reg.addEnum(e);
  }
  const out: Record<string, ModelClass<ModelSpec> & Record<string, unknown>> = {};
  for (const m of ir.models) {
    if (destination.has(m.name)) {
      if (JSON.stringify(m) !== JSON.stringify(destination.get(m.name).ir)) throw new TypeError(`extension changed existing model ${m.name}; define dependent schemas together in a new registry`);
      continue;
    }
    const meta = new ModelMeta(m, reg);
    const computed = new Set(((ir.behavior as { result_fields?: { model: string; field: string }[] } | undefined)?.result_fields ?? []).filter((f) => f.model === m.name).map((f) => f.field));
    if (computed.size) {
      meta.inputFieldList = meta.fieldList.filter((f) => !computed.has(f.ir));
      meta.inputFields = new Map(meta.inputFieldList.map((f) => [f.name, f]));
    }
    for (const method of ((ir.behavior as { methods?: { model: string; name: string; native_function: string }[] } | undefined)?.methods ?? [])) {
      if (method.model === m.name) {
        const name = camel(method.name);
        checkName(m.name, name, meta.fields);
        if (meta.relations.has(name) || Object.prototype.hasOwnProperty.call(meta.model, name) || Object.prototype.hasOwnProperty.call(meta.Row.prototype, name)) throw new TypeError(`${m.name}.${name}: model method collision`);
        const fn = (native() as unknown as Record<string, (value: string) => unknown>)[method.native_function];
        if (typeof fn !== "function") throw new TypeError(`${m.name}.${name}: missing native method; rebuild`);
        Object.defineProperty(meta.model, name, { value: (value: string) => call(() => fn(value)), enumerable: true });
      }
    }
    meta.objects = makeQuerySet(meta);
    Object.defineProperty(meta.Row, "meta", { value: meta });
    reg.add(meta);
    out[m.name] = meta.model;
  }
  reg.prepare();
  destination.publish(reg);
  return out;
}

/**
 * Compiles a schema file (`schema.prisma`) and builds its models, without generated code
 * (and so without static types: generate `models.ts` for those).
 */
export function load(path: string, options: { readonly registry?: Registry } = {}): Record<string, ModelClass<ModelSpec> & Record<string, unknown>> {
  return define(call(() => native().compileSchemaFile(path)), options);
}

/** Like {@link load}, for schema source text. */
export function loads(source: string, options: { readonly registry?: Registry } = {}): Record<string, ModelClass<ModelSpec> & Record<string, unknown>> {
  return define(call(() => native().compileSchema(source, null)), options);
}

/** The metadata of a model object, if `x` is one. */
export function modelMeta(x: unknown): ModelMeta | undefined {
  const meta = (x as { _meta?: unknown } | null)?._meta;
  return meta instanceof ModelMeta && meta.model === x ? meta : undefined;
}
