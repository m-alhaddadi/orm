/**
 * Query sets: immutable query builders. Nothing runs until a terminal method is called:
 * `all()`, `first()`, `get()`, `count()`, `insert()`, `update()`, ... (or `for await`).
 */

import { Builder } from "./build.js";
import { resolve, type Database } from "./db.js";
import {
  asCondition,
  Column,
  Condition,
  Ctes,
  Expression,
  IRContext,
  Node,
  orderings,
  Ordering,
  PATH,
  ParamRef,
  RelationPath,
  Slot,
  SlotParams,
  and,
  exists,
  not,
  outer,
  type CteLike,
  type ExistsQuery,
  type IR,
  type Many,
  type NearestOf,
  type OuterOf,
  type ParamValues,
  type Source,
  type Subquery,
} from "./expr.js";
import { NotLoaded, QueryError, TransactionRequired } from "./errors.js";
import type { Hop, HopKind, In, ModelSpec, RelationMeta } from "./meta.js";
import { DB, fieldValue, RELATED, registerQueries, type Instance, type ModelClass, type ModelMeta } from "./model.js";
import { call, wait, type NativeReturned, type NativeSelect } from "./native.js";
import { assignments, prepareRows, prepareUpdateRows, prepareAttach } from "./write.js";
import { after, decodeCursor, encodeCursor, fingerprint, keyset, type Page, type PageOptions } from "./pagination.js";
import { allowedWrites } from "./protection.js";
import { active as debugging, internalLoop, record, relationLoad } from "./debug.js";
import type { Cte, CteColumnsOf, CteSelf } from "./cte.js";
import type { Select, SelectItems, SelectRow, ItemsParams, ItemsOuter } from "./select.js";

// -- type helpers ---------------------------------------------------------------------------------

/** Ids per query in `inBulk()`, well below Postgres' 65535 parameters. */
const IN_BULK_CHUNK = 10_000;

export type UnionToIntersection<U> = (U extends unknown ? (x: U) => void : never) extends (x: infer I) => void ? I : never;
/** Flattens an intersection for readable hovers. */
export type Simplify<T> = { [K in keyof T]: T[K] } & {};

/** What a query's expressions may read: its own sources, to-many paths (in filters),
 * and columns of enclosing queries. */
export type Allowed<S extends string> = S | Many | OuterOf<string> | NearestOf<string>;
/** Expressions allowed where rows must not repeat (ordering, `select()`, grouping). */
export type AllowedOne<S extends string> = S | OuterOf<string> | NearestOf<string>;

/** A field name in `orderBy`: `"createdAt"` ascending, `"-createdAt"` descending. */
export type FieldOrder<M extends ModelSpec> = (keyof M["data"] & string) | `-${keyof M["data"] & string}`;

type ItemScope<I> = I extends Expression<unknown, infer S, unknown> ? S : I extends Ordering<infer S, unknown> ? S : never;
type ItemParams<I> = I extends Expression<unknown, string, infer P> ? P : I extends Ordering<string, infer P> ? P : {};
export type ScopesOf<C extends readonly unknown[]> = ItemScope<C[number]>;
export type ParamsOfAll<C extends readonly unknown[]> = UnionToIntersection<{ [K in keyof C]: ItemParams<C[K]> }[number]>;
/** The `outer()` references among scopes `S` (a wide `^${string}`, which only comes
 * from context-inferred type arguments, is none). */
export type OuterRefs<S extends string, Own extends string> = S extends `^${infer R}`
  ? string extends R
    ? never
    : S
  : S extends `~${infer R}`
    ? string extends R
      ? never
      : R extends Own
        ? never
        : OuterOf<R>
    : never;

/** `[]` when the query can run as is; otherwise an argument nobody can pass, whose name
 * says why. */
export type Runnable<P, X extends string> = [X] extends [never]
  ? [keyof P] extends [never]
    ? []
    : [error: "this query has param() placeholders: run it through .prepare()"]
  : [error: "this query reads outer() columns: use it inside exists(), in() or asScalar()"];

/** The `this` of `then()`: awaiting a query that can't run on its own is a type error
 * (TS1320) that names what is missing. */
export type Awaitable<P, X extends string> = [X] extends [never]
  ? [keyof P] extends [never]
    ? unknown
    : { readonly "~await": "this query has param() placeholders: run it through .prepare()" }
  : { readonly "~await": "this query reads outer() columns: use it inside exists(), in() or asScalar()" };

/**
 * The result cache: the rows of each query set's (and `select()`'s) first `await`.
 * Later awaits, concurrent ones included, share that run and get a copy of its rows.
 * Builders return new query sets with empty caches, and `.all()` always queries; a
 * failed run isn't kept, so awaiting again retries. `Model.objects` lives as long as
 * the model, so it never caches.
 */
const results = new WeakMap<object, Promise<readonly unknown[]>>();

/** @internal */
export async function cachedRows<T>(owner: object, fetch: () => Promise<T[]>, enabled = true): Promise<T[]> {
  if (!enabled) {
    return fetch();
  }
  let run = results.get(owner) as Promise<readonly T[]> | undefined;
  if (run === undefined) {
    run = fetch();
    results.set(owner, run);
    run.catch(() => {
      if (results.get(owner) === run) {
        results.delete(owner);
      }
    });
  }
  return [...(await run)];
}

type Last<H extends readonly Hop[]> = H extends readonly [...unknown[], infer L extends Hop] ? L : never;

type Wrap<K extends HopKind, T, Plain extends boolean> = K extends "one"
  ? T
  : K extends "opt"
    ? T | null
    : Plain extends true
      ? readonly T[]
      : { readonly cached: readonly T[] };

/**
 * What loading the relations of path `H` adds to a row: `Comment.post.author` gives
 * `{ post: Post & { author: User } }`; a to-many hop gives `{ posts: { cached: Post[] } }`
 * (`user.posts.cached`). `Last` replaces the last hop's rows (a `Prefetch` query set's
 * rows), `Attr` the attribute they go to (`toAttr`: a plain array).
 */
export type LoadOf<
  H extends readonly Hop[],
  Last = H extends readonly [...unknown[], infer L extends Hop] ? L["spec"]["row"] : never,
  Attr extends string | undefined = undefined,
> = H extends readonly [infer F extends Hop, ...infer Rest extends readonly Hop[]]
  ? Rest extends readonly []
    ? { readonly [K in Attr extends string ? Attr : F["name"]]: Wrap<F["kind"], Last, Attr extends string ? true : false> }
    : { readonly [K in F["name"]]: Wrap<F["kind"], F["spec"]["row"] & LoadOf<Rest, Last, Attr>, false> }
  : {};

type PathLoad<I> =
  I extends Prefetch<infer H, infer Row, infer A, string, unknown>
    ? LoadOf<H, Row, A>
    : I extends RelationPath<ModelSpec, string, infer H>
      ? LoadOf<H>
      : never;
export type LoadAll<C extends readonly unknown[]> = UnionToIntersection<{ [K in keyof C]: PathLoad<C[K]> }[number]>;
type PrefetchParams<C extends readonly unknown[]> = UnionToIntersection<
  { [K in keyof C]: C[K] extends Prefetch<readonly Hop[], unknown, string | undefined, string, infer P> ? P : {} }[number]
>;

/** Columns of model `M` (the root of its queries). */
export type OwnColumn<M extends ModelSpec> = Column<unknown, M["name"]>;

export interface LockOptions {
  /** `FOR UPDATE` (the default); `false`: `FOR SHARE`. */
  readonly exclusive?: boolean;
  /** Throw `LockNotAvailable` instead of waiting for rows locked by others. */
  readonly nowait?: boolean;
  /** Leave out rows locked by others (job queues). */
  readonly skipLocked?: boolean;
}

export interface ConflictOptions<M extends ModelSpec> {
  /** The column(s) of the unique constraint the rows may hit. */
  readonly onConflict: OwnColumn<M> | readonly OwnColumn<M>[];
}

/** `ON CONFLICT DO NOTHING`: keep the existing row. */
/** A checked single-row insert (`QuerySet.prepareInsert`). */
export interface PreparedInsert<M extends ModelSpec> {
  /** Inserts `values` (by default the checked values) and gives the new instance. */
  execute(values?: M["insert"]): Promise<M["row"]>;
}

/** A checked update (`QuerySet.prepareUpdate`). */
export interface PreparedUpdate<M extends ModelSpec> {
  /** The filters pin one row by a non-null primary key or unique field. */
  readonly unique: boolean;
  /** Updates to `values` (by default the checked values); `returning` gives the rows. */
  execute(values?: M["update"], options?: { readonly returning?: boolean }): Promise<number | M["row"][]>;
}

export interface DoNothing<M extends ModelSpec> extends ConflictOptions<M> {
  readonly doNothing: true;
}

/**
 * `ON CONFLICT DO UPDATE`: overwrite `doUpdate` columns with the proposed values (`true`:
 * every field given to the insert except the conflict columns), and apply `set`
 * (plain values or expressions such as `Post.views.add(excluded(Post.views))`).
 */
export interface DoUpdate<M extends ModelSpec> extends ConflictOptions<M> {
  readonly doUpdate?: true | readonly OwnColumn<M>[];
  readonly set?: M["update"];
}

type InsertOptions<M extends ModelSpec> = DoNothing<M> | DoUpdate<M>;

// -- prefetch -------------------------------------------------------------------------------------

/**
 * A `prefetchRelated` entry with its own query:
 *
 * ```ts
 * User.objects.prefetchRelated(
 *   new Prefetch(User.posts, Post.objects.filter(Post.published).orderBy(Post.views.desc()).limit(3)),
 * )
 * ```
 *
 * The query set filters, orders and slices the related rows (a slice applies per parent:
 * "the 3 most viewed posts of each user"); its own `selectRelated` / `prefetchRelated`
 * apply to them, and its row type is what the relation holds. The rows fill the relation
 * (`user.posts.cached`), or the plain array attribute `toAttr` when given.
 */
export class Prefetch<
  H extends readonly Hop[],
  Row = Last<H>["spec"]["row"],
  const A extends string | undefined = undefined,
  S extends string = string,
  P = {},
> {
  /** @internal */
  declare readonly "~prefetch"?: [H, Row, A, S, P];
  readonly toAttr: string | undefined;

  constructor(path: RelationPath<ModelSpec, S, H>);
  constructor(path: RelationPath<ModelSpec, S, H>, options: { readonly toAttr: A });
  constructor(path: RelationPath<ModelSpec, S, H>, queryset: QuerySet<Last<H>["spec"], Row, string, P, never>, options?: { readonly toAttr: A });
  constructor(
    readonly path: RelationPath<ModelSpec, S, H>,
    queryset?: QuerySet<ModelSpec, unknown, string, unknown, never> | { readonly toAttr: string },
    options?: { readonly toAttr: string },
  ) {
    if (!(path instanceof RelationPath) || !path[PATH].path.length) {
      throw new TypeError(`Prefetch() takes a relation such as User.posts, got ${String(path)}`);
    }
    let qs: QuerySet<ModelSpec, unknown, string, unknown, never> | undefined;
    if (queryset instanceof QuerySet) {
      qs = queryset;
    } else if (queryset !== undefined) {
      options = queryset;
    }
    if (qs && qs.meta.name !== path[PATH].target) {
      throw new TypeError(`Prefetch(${String(path)}) needs a query set of ${path[PATH].target}`);
    }
    const toAttr = options?.toAttr;
    if (toAttr !== undefined && (!/^[A-Za-z$][\w$]*$/.test(toAttr))) {
      throw new TypeError(`toAttr ${JSON.stringify(toAttr)} must be an identifier not starting with '_'`);
    }
    this.queryset = qs;
    this.toAttr = toAttr;
  }

  /** @internal */
  readonly queryset: QuerySet<ModelSpec, unknown, string, unknown, never> | undefined;
}

interface PrefetchNode {
  relation: string;
  attr: string;
  qs: QuerySet<ModelSpec, unknown, string, unknown, never> | undefined;
  children: [string[], Prefetch<readonly Hop[], unknown, string | undefined, string, unknown>][];
}

/** Nodes by attribute: paths sharing a prefix share its node, so `User.posts.comments`
 * nests under `User.posts`. Hops are IR relation names. */
function prefetchTree(
  meta: ModelMeta,
  items: Iterable<[string[], Prefetch<readonly Hop[], unknown, string | undefined, string, unknown>]>,
): Map<string, PrefetchNode> {
  const nodes = new Map<string, PrefetchNode>();
  for (const [hops, p] of items) {
    const hop = hops[0]!;
    const rel = meta.relationByIr.get(hop);
    if (!rel) {
      throw new QueryError(`${meta.name} has no relation ${JSON.stringify(hop)}`);
    }
    if (hops.length === 1 && p.toAttr !== undefined && (meta.fields.has(p.toAttr) || meta.relations.has(p.toAttr) || meta.fieldByIr.has(p.toAttr) || meta.relationByIr.has(p.toAttr))) {
      throw new QueryError(`toAttr ${JSON.stringify(p.toAttr)} is a field or relation of ${meta.name}`);
    }
    const attr = hops.length === 1 ? (p.toAttr ?? hop) : hop;
    let node = nodes.get(attr);
    if (!node) {
      node = { relation: hop, attr, qs: undefined, children: [] };
      nodes.set(attr, node);
    } else if (node.relation !== hop) {
      throw new QueryError(`prefetchRelated stores ${meta.name}.${node.relation} and .${hop} both in ${JSON.stringify(attr)}`);
    }
    if (hops.length === 1) {
      if (p.queryset) {
        if (node.qs && node.qs !== p.queryset) {
          throw new QueryError(`${String(p.path)} is prefetched twice with different query sets; use toAttr`);
        }
        node.qs = p.queryset;
      }
    } else {
      node.children.push([hops.slice(1), p]);
    }
  }
  return nodes;
}

function prefetchIr(
  meta: ModelMeta,
  items: Iterable<[string[], Prefetch<readonly Hop[], unknown, string | undefined, string, unknown>]>,
  params: unknown[],
): IR[] {
  const out: IR[] = [];
  for (const node of prefetchTree(meta, items).values()) {
    const target = meta.registry.get(meta.relationByIr.get(node.relation)!.target);
    const qs = node.qs ?? target.objects;
    if (qs.state.lock) {
      throw new QueryError("a prefetch query set can't lock rows");
    }
    const children: [string[], Prefetch<readonly Hop[], unknown, string | undefined, string, unknown>][] = [
      ...qs.state.prefetch.map((p) => [[...p.path[PATH].path], p] as [string[], typeof p]),
      ...node.children,
    ];
    const [ir, ctx] = qs.queryIr("select", params, undefined, undefined, false);
    if (children.length) {
      ir["prefetch"] = prefetchIr(target, children, params);
    }
    ctx.finish(ir);
    delete ir["op"];
    ir["relation"] = node.relation;
    if (node.attr !== node.relation) {
      ir["attr"] = node.attr;
    }
    out.push(ir);
  }
  return out;
}

// -- query sets -----------------------------------------------------------------------------------

/** @internal The state of a query set; copied, never changed. */
export interface QueryState {
  readonly modelHelpers?: readonly string[];
  readonly withoutDefaults: boolean;
  readonly withoutRelated: boolean;
  readonly modelFields: readonly string[] | undefined;
  readonly filters: readonly Node[];
  readonly order: readonly Ordering<string, unknown>[];
  readonly limit: number | ParamRef<string> | undefined;
  readonly offset: number | ParamRef<string> | undefined;
  /** `selectRelated` paths, IR relation names, prefixes first. */
  readonly related: readonly (readonly string[])[];
  readonly prefetch: readonly Prefetch<readonly Hop[], unknown, string | undefined, string, unknown>[];
  readonly lock: { exclusive: boolean; nowait: boolean; skip_locked: boolean } | undefined;
  readonly db: Database | undefined;
  readonly from: Cte<string, unknown, ModelSpec | null> | undefined;
  readonly joins: readonly [Cte<string, unknown, ModelSpec | null>, Node, boolean][];
}

const EMPTY: QueryState = {
  withoutDefaults: false,
  withoutRelated: false,
  modelFields: undefined,
  filters: [],
  order: [],
  limit: undefined,
  offset: undefined,
  related: [],
  prefetch: [],
  lock: undefined,
  db: undefined,
  from: undefined,
  joins: [],
};

function checkRunnable(meta: ModelMeta, state: QueryState): void {
  if (state.lock) {
    const db = resolve(state.db);
    if (db.tx() === null) {
      throw new TransactionRequired(
        "lock() outside a transaction would release the locks as soon as the query ends; run it inside `db.transaction(...)`",
      );
    }
  }
  void meta;
}

/**
 * A query over one model. Every builder method returns a new query set; nothing runs
 * until a terminal method is called (`all()`, `first()`, `count()`, `update()`, ...).
 *
 * Filters are expressions over the model's columns and relation paths:
 *
 * ```ts
 * await User.objects.filter(User.posts.createdAt.lt(yesterday)).all();
 * ```
 *
 * Conditions inside one `filter()` call that go through the same to-many relation must
 * hold for the same related row; separate `filter()` calls are independent.
 *
 * Type parameters: `M` the model, `R` the rows it gives (with loaded relations), `S` the
 * sources its expressions may read, `P` its `param()` placeholders, `X` the enclosing
 * queries it reads with `outer()`.
 */
export class QuerySet<M extends ModelSpec, R = M["row"], S extends string = M["name"], P = {}, X extends string = never>
  implements Subquery
{
  /** @internal */
  declare readonly "~exists"?: [X, P];
  /** @internal */
  declare readonly "~qs"?: [M, R, S, P, X];

  /** @internal */
  constructor(
    readonly meta: ModelMeta,
    /** @internal */
    readonly state: QueryState = EMPTY,
  ) {}

  /** @internal */
  protected clone(changes: Partial<QueryState>): this {
    const o = Object.create(Object.getPrototypeOf(this) as object) as this;
    Object.assign(o, this, { state: { ...this.state, ...changes } });
    return o;
  }

  /** @internal The query's source in IR (`ModelMeta`, or a CTE for `cte.select()`). */
  protected source(): Source {
    return this.meta;
  }

  /** @internal */
  protected rootName(): string {
    return this.meta.name;
  }

  // -- building -------------------------------------------------------------------------------

  /** @internal Preserve an exact public shape, including zero fields. */
  onlyFields(names: readonly string[]): this { return this.clone({ modelFields: names }); }

  /** Bypass schema filter, selection and loading defaults; keep caller filters. */
  withoutDefaults(): this { return this.clone({ withoutDefaults: true }); }

  /** Clear default and explicitly requested eager reference loading. */
  withoutRelated(): this { return this.clone({ withoutRelated: true, related: [] }); }

  /** Partial model instances; no arguments restores all public fields. */
  only(): QuerySet<M, M["row"], S, P, X>;
  only(...fields: readonly Column<unknown, string>[]): QuerySet<M, Partial<M["row"]> & Instance<M>, S, P, X>;
  only(...fields: readonly Column<unknown, string>[]): QuerySet<M, Partial<M["row"]> & Instance<M>, S, P, X> {
    const names = fields.map((f) => {
      if (!(f instanceof Column) || f.root !== this.meta || f.path.length) throw new TypeError("only() takes root model columns");
      return f.field.ir;
    });
    if (new Set(names).size !== names.length) throw new TypeError("duplicate model field");
    return this.clone({ modelFields: fields.length ? names : this.meta.fieldList.map((f) => f.ir) }) as never;
  }

  /** Keep rows matching all `conditions`. */
  filter<const C extends readonly Expression<boolean | null, Allowed<S>, unknown>[]>(
    ...conditions: C
  ): QuerySet<M, R, S, P & ParamsOfAll<C>, X | OuterRefs<ScopesOf<C>, S>> {
    if (!conditions.length) {
      return this as never;
    }
    return this.clone({ filters: [...this.state.filters, and(...conditions)] }) as never;
  }

  /** Drop rows matching all `conditions`. */
  exclude<const C extends readonly Expression<boolean | null, Allowed<S>, unknown>[]>(
    ...conditions: C
  ): QuerySet<M, R, S, P & ParamsOfAll<C>, X | OuterRefs<ScopesOf<C>, S>> {
    if (!conditions.length) {
      return this as never;
    }
    return this.clone({ filters: [...this.state.filters, not(and(...conditions))] }) as never;
  }

  /** Replace the ordering: `orderBy(Post.createdAt.desc(), Post.id)`, or by field name
   * with `-` for descending: `orderBy("-createdAt", "id")`. Columns through to-one
   * relations are joined. */
  orderBy<const C extends readonly (Expression<unknown, AllowedOne<S>, unknown> | Ordering<AllowedOne<S>, unknown> | FieldOrder<M>)[]>(
    ...items: C
  ): QuerySet<M, R, S, P & ParamsOfAll<C>, X | OuterRefs<ScopesOf<C>, S>> {
    const named = items.map((i) => {
      if (typeof i !== "string") return i;
      const column = this.meta.column(this.meta.field(i.startsWith("-") ? i.slice(1) : i));
      return i.startsWith("-") ? column.desc() : column.asc();
    });
    return this.clone({ order: orderings(named) }) as never;
  }

  /** At most `n` rows; `n` may be a `param()` in a prepared query. */
  limit<N extends string>(n: ParamRef<N>): QuerySet<M, R, S, P & ParamValues<N, number | bigint>, X>;
  limit(n: number | null): QuerySet<M, R, S, P, X>;
  limit(n: number | null | ParamRef<string>): unknown {
    return this.clone({ limit: count(n, "limit") });
  }

  /** Skip `n` rows; `n` may be a `param()` in a prepared query. */
  offset<N extends string>(n: ParamRef<N>): QuerySet<M, R, S, P & ParamValues<N, number | bigint>, X>;
  offset(n: number | null): QuerySet<M, R, S, P, X>;
  offset(n: number | null | ParamRef<string>): unknown {
    const v = count(n, "offset");
    return this.clone({ offset: v === 0 ? undefined : v });
  }

  /** Rows `start` (inclusive) to `end` (exclusive) of the current ordering, like
   * `Array.slice`: `slice(10, 20)` is `offset(10).limit(10)`. Non-negative only. */
  slice(start: number, end?: number): QuerySet<M, R, S, P, X> {
    const { limit, offset } = this.state;
    if (limit instanceof ParamRef || offset instanceof ParamRef) {
      throw new QueryError("a query set limited by param() can't be sliced; use limit() and offset()");
    }
    if (!Number.isInteger(start) || start < 0 || (end !== undefined && (!Number.isInteger(end) || end < 0))) {
      throw new RangeError("slice() takes non-negative integers");
    }
    const off = (offset ?? 0) + start;
    let lim = limit;
    if (end !== undefined) {
      const n = Math.max(end - start, 0);
      lim = lim === undefined ? n : Math.max(Math.min(n, lim - start), 0);
    } else if (lim !== undefined) {
      lim = Math.max(lim - start, 0);
    }
    return this.clone({ offset: off || undefined, limit: lim }) as never;
  }

  /**
   * Loads to-one relations in the same query with LEFT JOINs; the rows' type has them:
   * `Comment.objects.selectRelated(Comment.post.author)` gives
   * `Comment & { post: Post & { author: User } }`.
   */
  selectRelated<const C extends readonly RelationPath<ModelSpec, M["name"], readonly Hop[]>[]>(
    ...paths: C
  ): QuerySet<M, R & LoadAll<C>, S, P, X> {
    const related = [...this.state.related];
    for (const p of paths) {
      this.checkPath(p);
      const hops = p[PATH].path;
      for (let i = 1; i <= hops.length; i++) {
        const prefix = hops.slice(0, i);
        if (!related.some((r) => r.length === prefix.length && r.every((h, k) => h === prefix[k]))) {
          related.push(prefix);
        }
      }
    }
    return this.clone({ related }) as never;
  }

  /**
   * Loads relations with one extra `IN (...)` query each, in the same call; the rows'
   * type has them:
   *
   * ```ts
   * User.objects.prefetchRelated(User.posts)            // user.posts.cached: Post[]
   * User.objects.prefetchRelated(User.posts.comments)   // and each post's comments
   * Comment.objects.prefetchRelated(Comment.post)       // to-one: comment.post
   * User.objects.prefetchRelated(new Prefetch(User.posts, Post.objects.filter(...)))
   * ```
   */
  prefetchRelated<
    const C extends readonly (
      | RelationPath<ModelSpec, M["name"] | Many, readonly Hop[]>
      // eslint-disable-next-line @typescript-eslint/no-explicit-any -- `any`, not a contextual type the toAttr literal would widen to
      | Prefetch<any, any, any, M["name"] | Many, any>
    )[],
  >(...relations: C): QuerySet<M, R & LoadAll<C>, S, P & PrefetchParams<C>, X> {
    const prefetch = [...this.state.prefetch];
    for (const item of relations) {
      const p = item instanceof Prefetch ? item : new Prefetch(item);
      this.checkPath(p.path);
      prefetch.push(p);
    }
    prefetchTree(this.meta, prefetch.map((p) => [[...p.path[PATH].path], p])); // validates
    return this.clone({ prefetch }) as never;
  }

  /**
   * Locks the rows this query reads until the transaction ends: `FOR UPDATE`, or
   * `FOR SHARE` with `exclusive: false`. Only this model's rows are locked, not rows joined
   * by `selectRelated`. Must run inside `db.transaction()`.
   */
  lock(options: LockOptions & ({ readonly nowait?: false } | { readonly skipLocked?: false }) = {}): QuerySet<M, R, S, P, X> {
    const { exclusive = true, nowait = false, skipLocked = false } = options as LockOptions;
    if (nowait && skipLocked) {
      throw new TypeError("lock() takes nowait or skipLocked, not both");
    }
    return this.clone({ lock: { exclusive, nowait, skip_locked: skipLocked } }) as never;
  }

  /** Run on `db` instead of the default database. */
  using(db: Database | undefined): this {
    return this.clone({ db });
  }

  // -- CTEs -----------------------------------------------------------------------------------

  /**
   * This query as a CTE (`WITH <name> AS (...)`) with the model's columns: read it with
   * `Model.objects.from(cte)`. `recursive` builds the recursive part from the CTE
   * (`WITH RECURSIVE`; `UNION ALL`, or `UNION` with `distinct`).
   */
  cte<const N extends string>(
    name: N,
    options: CteOptions<N, M["data"], P, X> = {},
  ): Cte<N, M["data"], M> {
    return makeCte(name, this as never, options as never) as never;
  }

  /**
   * `JOIN <cte> ON <on>` (`LEFT JOIN` with `outer`): the CTE's columns (`cte.c.<name>`)
   * come along with each row, for filters, ordering and `select()`.
   */
  join<N extends string, C, const On extends Expression<boolean | null, Allowed<S | N>, unknown>>(
    cte: Cte<N, C, ModelSpec | null>,
    on: On,
    options: { readonly outer?: boolean } = {},
  ): QuerySet<M, R, S | N, P & ParamsOfAll<[On]>, X | OuterRefs<ScopesOf<[On]>, S | N>> {
    if (!isCte(cte)) {
      throw new TypeError(`join() takes a CTE, got ${String(cte)}`);
    }
    if (this.state.joins.some(([c]) => c === cte) || cte === this.state.from) {
      throw new QueryError(`${cte.name} is already read by this query`);
    }
    return this.clone({ joins: [...this.state.joins, [cte, asCondition(on), options.outer ?? false]] }) as never;
  }

  /**
   * Reads the rows from `cte` instead of the model's table (a subquery in `FROM`). The
   * CTE must have the model's columns; its other columns are `cte.c.<name>`.
   */
  from<N extends string>(cte: Cte<N, unknown, M>): QuerySet<M, R, S | N, P, X> {
    if (!isCte(cte)) {
      throw new TypeError(`from() takes a CTE, got ${String(cte)}`);
    }
    if (cte.model !== this.meta) {
      throw new TypeError(`from(${cte.name}) needs a CTE with the columns of ${this.meta.name}`);
    }
    return this.clone({ from: cte }) as never;
  }

  // -- select ---------------------------------------------------------------------------------

  /**
   * Rows of chosen columns and aggregates instead of instances, keyed like `items`:
   *
   * ```ts
   * await Post.objects.select({ id: Post.id, author: Post.author.name }).all();   // { id: bigint; author: string }[]
   * await User.objects.select({ user: User, posts: func.count(User.posts) }).all();
   * ```
   */
  select<const I extends SelectItems<M, S>>(
    items: I,
  ): Select<M, Simplify<SelectRow<I>>, S, P & ItemsParams<I>, X | ItemsOuter<I, S>, CteColumnsOf<I>> {
    return makeSelect(this as never, items) as never;
  }

  // -- batches --------------------------------------------------------------------------------

  /**
   * The rows in arrays of `size`, walking the primary key (`WHERE pk > last ORDER BY pk
   * LIMIT size`), so memory stays flat and each batch is an index range scan.
   * `selectRelated`, `prefetchRelated` and `lock()` apply per batch; a custom `orderBy`
   * or slicing is refused.
   */
  async *batches(size = 1000, ...check: Runnable<P, X>): AsyncGenerator<R[], void, undefined> {
    void check;
    if (!Number.isInteger(size) || size < 1) {
      throw new RangeError("batch size must be at least 1");
    }
    if (this.state.order.length) {
      throw new QueryError("batches() walk the primary key in order; drop orderBy()");
    }
    if (this.state.limit !== undefined || this.state.offset !== undefined) {
      throw new QueryError("batches() can't be used on a sliced query set");
    }
    const pk = this.meta.column(this.meta.pk);
    let last: unknown = undefined;
    const seen = new Set<string>();
    for (;;) {
      const page = last === undefined ? this : this.clone({ filters: [...this.state.filters, pk.gt(last as never)] });
      const objs = (await internalLoop(seen, () => page.clone({ order: [pk.asc()], limit: size }).fetch())) as R[];
      if (objs.length) {
        yield objs;
      }
      if (objs.length < size) {
        return;
      }
      last = fieldValue(objs[objs.length - 1] as object, this.meta.pk.name);
    }
  }

  /** Every row, fetched `batchSize` at a time (see {@link batches}). */
  async *iterate(batchSize = 1000, ...check: Runnable<P, X>): AsyncGenerator<R, void, undefined> {
    for await (const batch of this.batches(batchSize, ...check)) {
      yield* batch;
    }
  }

  private checkPath(p: RelationPath<ModelSpec, string, readonly Hop[]>): void {
    if (!(p instanceof RelationPath) || modelOf(p)) {
      throw new TypeError(`expected a relation such as ${this.meta.name}.<relation>, got ${String(p)}`);
    }
    if (p[PATH].root !== this.meta) {
      throw new QueryError(`${String(p)} does not start at ${this.meta.name}`);
    }
  }

  // -- IR -------------------------------------------------------------------------------------

  /** @internal */
  context(params: unknown[], outer: IRContext | undefined, ctes: Ctes | undefined): IRContext {
    const ctx = new IRContext(this.source(), params, outer, ctes);
    if (this.state.from) {
      ctx.useCte(this.state.from as unknown as CteLike);
    }
    return ctx;
  }

  /**
   * @internal The query's IR and the context it compiled in (`finish()` it to declare
   * its CTEs). In a subquery (`outer`) or a CTE (`ctes`) related loading is left out.
   */
  queryIr(op: string, params: unknown[], outer?: IRContext, ctes?: Ctes, prefetch = true): [IR, IRContext] {
    const s = this.state;
    const ctx = this.context(params, outer, ctes);
    const ir: IR = { op, model: this.rootName() };
    if (s.withoutDefaults) ir["without_defaults"] = true;
    if (s.withoutRelated) ir["without_related"] = true;
    if (s.modelFields !== undefined) ir["model_fields"] = s.modelFields;
    if (s.modelHelpers?.length) ir["model_helpers"] = s.modelHelpers;
    if (s.from) {
      ir["from"] = s.from.name;
    }
    if (s.joins.length) {
      ir["joins"] = s.joins.map(([cte, on, isOuter]) => {
        ctx.useCte(cte as unknown as CteLike);
        return { cte: cte.name, on: on.ir(ctx), outer: isOuter };
      });
    }
    if (s.filters.length) {
      ir["filters"] = s.filters.map((f) => f.ir(ctx));
    }
    if (s.order.length) {
      ir["order"] = s.order.map((o) => o.ir(ctx));
    }
    if (s.limit !== undefined) {
      ir["limit"] = s.limit instanceof ParamRef ? s.limit.slot(ctx.params) : ctx.param(s.limit);
    }
    if (s.offset !== undefined) {
      ir["offset"] = s.offset instanceof ParamRef ? s.offset.slot(ctx.params) : ctx.param(s.offset);
    }
    const top = outer === undefined && ctes === undefined;
    if (s.related.length && op === "select" && top) {
      ir["select_related"] = s.related.map((p) => [...p]);
    }
    if (s.prefetch.length && op === "select" && top && prefetch) {
      ir["prefetch"] = prefetchIr(this.meta, s.prefetch.map((p) => [[...p.path[PATH].path], p]), params);
    }
    if (s.lock) {
      if (op !== "select") {
        throw new QueryError(`${op}() can't lock rows; lock() applies to reading rows`);
      }
      ir["lock"] = s.lock;
    }
    ctx.addWindows(ir);
    return [ir, ctx];
  }

  /** @internal */
  selectIr(op: string, params: unknown[]): IR {
    const [ir, ctx] = this.queryIr(op, params);
    return ctx.finish(ir);
  }

  /** @internal */
  subqueryIr(ctx: IRContext, what: string): IR {
    if (this.state.lock) {
      throw new QueryError("a subquery can't lock rows");
    }
    void what;
    return this.queryIr("select", ctx.params, ctx)[0];
  }

  /** @internal */
  cteIr(params: unknown[], ctes: Ctes): IR {
    return this.queryIr("select", params, undefined, ctes)[0];
  }

  /** @internal */
  mutationIr(op: string, params: unknown[], values?: object): IR {
    const s = this.state;
    if (s.limit !== undefined || s.offset !== undefined) {
      throw new QueryError(`${op}() is not supported on a sliced query set`);
    }
    if (s.lock) {
      throw new QueryError(`${op}() locks the rows it changes; drop lock()`);
    }
    if (s.from || s.joins.length) {
      throw new QueryError(`${op}() writes the model's table; it can't run on a query set with from() or join()`);
    }
    const ctx = new IRContext(this.meta, params);
    const ir: IR = { op, model: this.meta.name, filters: s.filters.map((f) => f.ir(ctx)) };
    if (s.withoutDefaults) ir["without_defaults"] = true;
    if (s.modelFields !== undefined) ir["model_fields"] = s.modelFields;
    if (values !== undefined) {
      ir["set"] = assignments(this.meta, values, ctx);
    }
    return ctx.finish(ir);
  }

  /**
   * This query compiled once, to run many times with `param()` values (typed by what
   * each `param()` is compared with):
   *
   * ```ts
   * const byAuthor = Post.objects.filter(Post.authorId.eq(param("author"))).prepare();
   * const posts = await byAuthor.all({ author: 3n });
   * ```
   */
  prepare(...check: [X] extends [never] ? [] : [error: "a prepared query can't read outer() columns"]): Prepared<M, R, P> {
    void check;
    return new Prepared(this as never);
  }

  /** The SELECT this query runs, parameters inlined (for debugging). */
  sql(...check: Runnable<P, X>): string {
    void check;
    const params: unknown[] = [];
    const ir = this.selectIr("select", params);
    return call(() => this.meta.registry.native().sql(JSON.stringify(ir), params));
  }

  // -- execution ------------------------------------------------------------------------------

  /** @internal */
  db(): Database {
    return resolve(this.state.db);
  }

  /** @internal */
  async fetch(): Promise<R[]> {
    const params: unknown[] = [];
    const ir = this.selectIr("select", params);
    checkRunnable(this.meta, this.state);
    const db = this.db();
    const res = (await db.run(ir, params)) as NativeSelect;
    return new Builder(db.registry, this.state.db).select(res) as R[];
  }

  /** The rows, queried afresh (unlike `await qs`, which reuses its first result). */
  all(...check: Runnable<P, X>): Promise<R[]> {
    void check;
    return this.fetch();
  }

  /**
   * `await qs`: the rows. The query runs on the first `await` of this query set; later
   * ones give the same rows again (see {@link cachedRows}). Nothing runs before.
   */
  then<A = R[], B = never>(
    this: QuerySet<M, R, S, P, X> & Awaitable<P, X>,
    onfulfilled?: ((rows: R[]) => A | PromiseLike<A>) | null,
    onrejected?: ((reason: unknown) => B | PromiseLike<B>) | null,
  ): Promise<A | B> {
    return this.fromCache().then(onfulfilled, onrejected);
  }

  /** @internal */
  fromCache(): Promise<R[]> {
    return cachedRows(this, () => this.fetch(), this !== (this.meta.objects as unknown));
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<R, void, undefined> {
    yield* await this.fromCache();
  }

  /** @internal The order of this read: `orderBy()`, else the schema default order, else the pk. */
  defaultOrder(): readonly Ordering<string, unknown>[] {
    if (this.state.order.length) return this.state.order;
    const keys = this.meta.defaultOrder;
    if (keys.length && !this.state.withoutDefaults && !this.state.from) {
      return keys.map((k) => new Ordering(this.meta.column(this.meta.fieldByIr.get(k.field)!), k.desc ?? false, k.nulls));
    }
    return [this.meta.column(this.meta.pk).asc()];
  }

  /**
   * A page of rows by keyset: `paginate({ first: 20, after })` reads on from a cursor,
   * `paginate({ last: 20, before })` reads back.
   *
   * The order is `orderBy()`, else the schema default order, else the primary key; the
   * primary key is added when the order is not unique. Order columns are columns of the
   * model itself, and a nullable one needs `{ nulls }`. `selectRelated`,
   * `prefetchRelated`, `only()` and query defaults apply to each page.
   */
  async paginate(options: PageOptions, ...check: Runnable<P, X>): Promise<Page<R>> {
    void check;
    const forward = options.first !== undefined;
    if (forward === (options.last !== undefined)) throw new TypeError("paginate() takes first or last");
    if ((forward && options.before != null) || (!forward && options.after != null)) {
      throw new TypeError("paginate() takes first with after, or last with before");
    }
    const size = forward ? options.first : options.last;
    if (!Number.isInteger(size) || size! < 1) throw new RangeError("a page size is an integer of at least 1");
    if (this.state.limit !== undefined || this.state.offset !== undefined) {
      throw new QueryError("paginate() can't be used on a sliced query set");
    }
    const keys = keyset(this.meta, this.defaultOrder());
    const fp = fingerprint(this.meta, keys);
    const order = keys.map(([, o]) => (forward ? o : o.reversed()));
    const helpers = [...(this.state.modelHelpers ?? [])];
    for (const [f] of keys) if (!helpers.includes(f.ir)) helpers.push(f.ir);
    const cursor = (forward ? options.after : options.before) ?? null;
    const filters = cursor === null ? this.state.filters : [...this.state.filters, after(this.meta, order, decodeCursor(this.meta, cursor, fp, keys))];
    const fetched = (await this.clone({ modelHelpers: helpers, filters, order, limit: size! + 1, offset: undefined }).fetch()) as R[];
    const more = fetched.length > size!;
    const items = forward ? fetched.slice(0, size!) : fetched.slice(0, size!).reverse();
    return {
      items,
      hasNext: forward ? more : cursor !== null,
      hasPrevious: forward ? cursor !== null : more,
      nextCursor: items.length ? encodeCursor(fp, keys, items[items.length - 1] as object) : null,
      previousCursor: items.length ? encodeCursor(fp, keys, items[0] as object) : null,
    };
  }

  /** The first row by the current ordering (the primary key if none), or `null`. */
  async first(...check: Runnable<P, X>): Promise<R | null> {
    void check;
    const objs = await this.clone({ order: this.defaultOrder() }).slice(0, 1).fetch();
    return objs[0] ?? null;
  }

  /** The last row by the current ordering (the primary key if none), or `null`. */
  async last(...check: Runnable<P, X>): Promise<R | null> {
    void check;
    const objs = await this.clone({ order: this.defaultOrder().map((o) => o.reversed()) }).slice(0, 1).fetch();
    return objs[0] ?? null;
  }

  /** The single row matching `conditions`; throws `Model.DoesNotExist` or
   * `Model.MultipleObjectsReturned` otherwise. */
  async get<const C extends readonly Expression<boolean | null, Allowed<S>, {}>[]>(
    ...conditions: C & ([X] extends [never] ? ([keyof P] extends [never] ? unknown : never) : never)
  ): Promise<R> {
    const objs = await (this.filter(...(conditions as unknown as never[])) as unknown as QuerySet<M, R>).limit(2).fetch();
    return one(this.meta, objs);
  }

  /** The number of rows. */
  async count(...check: Runnable<P, X>): Promise<number> {
    void check;
    const params: unknown[] = [];
    const ir = this.selectIr("count", params);
    return (await this.db().run(ir, params)) as number;
  }

  /** Whether any row matches. For `EXISTS` inside another query use `exists()`. */
  async exists(...check: Runnable<P, X>): Promise<boolean> {
    void check;
    const params: unknown[] = [];
    const ir = this.selectIr("exists", params);
    return (await this.db().run(ir, params)) as boolean;
  }

  /**
   * The rows whose `field` (the primary key by default, else a unique field) is one of
   * `ids`, by that value. Missing ids are left out; without `ids`, every row. Big id
   * lists run as several queries.
   */
  async inBulk<T = M["pk"]>(
    ids?: readonly In<T>[] | null,
    options: { readonly field?: Column<T, M["name"]> } = {},
  ): Promise<Map<T, R>> {
    const col = options.field ?? (this.meta.column(this.meta.pk) as Column<T, M["name"]>);
    if (!(col instanceof Column) || col.root !== this.meta || col.path.length) {
      throw new TypeError(`inBulk({ field }) takes a column of ${this.meta.name}, got ${String(col)}`);
    }
    if (!(col.field.primaryKey || col.field.unique)) {
      throw new QueryError(`inBulk({ field: ${String(col)} }) needs a unique field`);
    }
    if (this.state.limit !== undefined || this.state.offset !== undefined) {
      throw new QueryError("inBulk() can't be used on a sliced query set");
    }
    const name = col.field.name;
    const out = new Map<T, R>();
    const add = (objs: R[]): void => {
      for (const o of objs) {
        out.set(fieldValue(o as object, name) as T, o);
      }
    };
    if (ids === undefined || ids === null) {
      add(await this.clone({ modelHelpers: [...(this.state.modelHelpers ?? []), col.field.ir] }).fetch());
      return out;
    }
    const keys = [...new Set(ids)];
    const seen = new Set<string>();
    for (let i = 0; i < keys.length; i += IN_BULK_CHUNK) {
      add(await internalLoop(seen, () => this.clone({ modelHelpers: [...(this.state.modelHelpers ?? []), col.field.ir], filters: [...this.state.filters, col.in(keys.slice(i, i + IN_BULK_CHUNK) as never)] }).fetch()));
    }
    return out;
  }

  // -- writes ---------------------------------------------------------------------------------

  /**
   * `UPDATE` every matching row; values may be expressions (`{ views: Post.views.add(1) }`).
   * Gives the number of rows updated, or with `{ returning: true }` the updated rows.
   */
  update(values: M["update"]): Promise<number>;
  update(values: M["update"], options: { readonly returning: true }): Promise<M["row"][]>;
  update(values: M["update"], options?: { readonly returning?: boolean }): Promise<number | M["row"][]>;
  async update(values: M["update"], options: { readonly returning?: boolean } = {}): Promise<number | M["row"][]> {
    return this.updateValues(values, options.returning ?? false);
  }

  private async updateValues(values: M["update"], returning: boolean): Promise<number | M["row"][]> {
    const params: unknown[] = [];
    const ir = this.mutationIr("update", params, values);
    if (!(ir["set"] as unknown[]).length) {
      return returning ? [] : 0;
    }
    return this.write(ir, params, returning);
  }

  /**
   * Checks `update(values)` as `update()` does, without SQL or I/O, for a package that
   * must check a write before its own I/O. `unique`: the filters pin one row by a
   * non-null primary key or unique field. `execute()` runs the update.
   */
  prepareUpdate(values: M["update"]): PreparedUpdate<M> {
    this.db();
    const params: unknown[] = [];
    const ir = this.mutationIr("update", params, values);
    const unique = call(() => this.meta.registry.native().uniqueRowUpdate(JSON.stringify(ir), params, allowedWrites()));
    return { unique, execute: (data = values, options = {}) => this.updateValues(data, options.returning ?? false) };
  }

  /**
   * `DELETE` every matching row. Gives the number of rows deleted, or with
   * `{ returning: true }` the deleted rows.
   */
  delete(): Promise<number>;
  delete(options: { readonly returning: true }): Promise<M["row"][]>;
  delete(options?: { readonly returning?: boolean }): Promise<number | M["row"][]>;
  async delete(options: { readonly returning?: boolean } = {}): Promise<number | M["row"][]> {
    const params: unknown[] = [];
    return this.write(this.mutationIr("delete", params), params, options.returning ?? false);
  }

  private async write(ir: IR, params: unknown[], returning: boolean): Promise<number | M["row"][]> {
    if (returning) {
      ir["returning"] = true;
    }
    const db = this.db();
    const res = await db.run(ir, params);
    return returning ? (new Builder(db.registry, this.state.db).returned(res as NativeReturned) as M["row"][]) : (res as number);
  }

  /**
   * `UPDATE` each row to its own values: `rows` have the primary key and the fields to
   * set (the same fields in every row). Rows outside this query set's filters are left
   * alone. One statement per `batchSize` rows (by default as many as fit), all in one
   * transaction. Gives the number of rows updated, or the rows with `returning`.
   */
  updateMany(rows: readonly M["updateRow"][], options?: { readonly batchSize?: number; readonly returning?: false }): Promise<number>;
  updateMany(rows: readonly M["updateRow"][], options: { readonly batchSize?: number; readonly returning: true }): Promise<M["row"][]>;
  async updateMany(
    rows: readonly M["updateRow"][],
    options: { readonly batchSize?: number; readonly returning?: boolean } = {},
  ): Promise<number | M["row"][]> {
    const { batchSize, returning = false } = options;
    if (batchSize !== undefined && (!Number.isInteger(batchSize) || batchSize < 1)) {
      throw new RangeError("batchSize must be at least 1");
    }
    this.mutationIr("update", []); // refuses sliced and locked query sets
    const prepared = prepareUpdateRows(this.meta, rows);
    if (!prepared.rows.length) {
      return returning ? [] : 0;
    }
    const params: unknown[] = [];
    const ir = this.mutationIr("update", params);
    if ("with" in ir) {
      throw new QueryError("updateMany() filters can't read CTEs");
    }
    const db = this.db();
    if (debugging()) record(`updateMany:${this.meta.name}:${prepared.fields}:${JSON.stringify(ir["filters"])}`, () => `UPDATE ${this.meta.name} SET ${prepared.fields.join(", ")} ... (updateMany)`);
    const res = await db_wait(db, (tx) =>
      db.engine.updateMany(this.meta.name, prepared.fields, prepared.rows, JSON.stringify(ir["filters"]), params, returning, batchSize ?? null, tx, this.state.withoutDefaults, allowedWrites()),
    );
    return returning ? (new Builder(db.registry, this.state.db).returned(res as NativeReturned) as M["row"][]) : (res as number);
  }

  /**
   * `INSERT` one row; gives the new instance with database defaults (ids, timestamps)
   * filled in. With `onConflict`, an upsert: `doUpdate` / `set` update the existing row,
   * `doNothing` keeps it (and gives `null`).
   */
  insert(values: M["insert"]): Promise<M["row"]>;
  insert(values: M["insert"], options: DoNothing<M>): Promise<M["row"] | null>;
  insert(values: M["insert"], options: DoUpdate<M>): Promise<M["row"]>;
  async insert(values: M["insert"], options?: InsertOptions<M>): Promise<M["row"] | null> {
    const rows = await this.insertRows([values], options);
    return rows[0] ?? null;
  }

  /** Attaches local child values to an existing parent, without a change to the parent.
   * Only composed child models accept it. */
  async attach(parentId: In<M["pk"]>, values: M extends { readonly attach: infer A extends object } ? A : never): Promise<M["row"]> {
    const prepared = prepareAttach(this.meta, values);
    const db = this.db();
    const res = await db_wait(db, (tx) => db.engine.attach(this.meta.name, parentId, prepared.fields, prepared.rows, tx, allowedWrites()));
    return (new Builder(db.registry, this.state.db).returned(res as NativeReturned) as M["row"][])[0]!;
  }

  /** `INSERT` many rows with one statement; gives the new instances in input order (rows
   * skipped by `doNothing` are left out). */
  insertMany(rows: readonly M["insert"][], options?: InsertOptions<M>): Promise<M["row"][]> {
    return this.insertRows(rows, options);
  }

  /**
   * Checks one `insert(values)` as `insert()` does, without SQL or I/O, for a package
   * that must check a write before its own I/O. `execute()` inserts the row.
   */
  prepareInsert(values: M["insert"]): PreparedInsert<M> {
    this.db();
    const prepared = prepareRows(this.meta, [values]);
    call(() => this.meta.registry.native().validateInsert(this.meta.name, prepared.fields, prepared.rows, allowedWrites()));
    return { execute: async (data = values) => (await this.insertRows([data], undefined))[0]! };
  }

  /** @internal */
  protected async insertRows(rows: readonly object[], options: InsertOptions<M> | undefined): Promise<M["row"][]> {
    const prepared = prepareRows(this.meta, rows);
    let conflict: string[] | null = null;
    let update: string[] | null = null;
    let set: string | null = null;
    const params: unknown[] = [];
    if (options) {
      conflict = ownFields(this.meta, toArray(options.onConflict), "onConflict");
      if (!conflict.length) {
        throw new TypeError("onConflict needs the column(s) of a unique constraint");
      }
      if ("doNothing" in options && options.doNothing) {
        if ("doUpdate" in options || "set" in options) {
          throw new TypeError("doNothing can't be combined with doUpdate / set");
        }
      } else {
        const o = options as DoUpdate<M>;
        const target = conflict;
        update =
          Array.isArray(o.doUpdate)
            ? ownFields(this.meta, o.doUpdate, "doUpdate")
            : o.doUpdate === true || o.set === undefined
              ? prepared.fields.filter((f) => prepared.provided.has(f) && !target.includes(f) && f !== this.meta.pk.ir)
              : [];
        if (o.set !== undefined) {
          const items = assignments(this.meta, o.set, new IRContext(this.meta, params));
          const twice = items.map((a) => a["field"] as string).filter((f) => update!.includes(f));
          if (twice.length) {
            throw new TypeError(`the upsert sets ${twice.join(", ")} twice`);
          }
          set = JSON.stringify(items);
        }
      }
    }
    if (!prepared.rows.length) {
      return [];
    }
    const db = this.db();
    if (debugging()) record(`insert:${this.meta.name}:${prepared.fields}:${conflict}`, () => `INSERT INTO ${this.meta.name} (${prepared.fields.join(", ")}) ...`);
    const res = await db_wait(db, (tx) =>
      db.engine.insert(this.meta.name, prepared.fields, prepared.rows, conflict, update, set, params, tx, allowedWrites()),
    );
    return new Builder(db.registry, this.state.db).returned(res as NativeReturned) as M["row"][];
  }

  toString(): string {
    return `QuerySet(${this.meta.name})`;
  }
}

function db_wait(db: Database, f: (tx: ReturnType<Database["tx"]>) => Promise<unknown>): Promise<unknown> {
  return wait(() => f(db.tx()));
}

function toArray<T>(x: T | readonly T[]): readonly T[] {
  return Array.isArray(x) ? (x as readonly T[]) : [x as T];
}

function ownFields(meta: ModelMeta, cols: readonly unknown[], what: string): string[] {
  return cols.map((c) => {
    if (!(c instanceof Column) || c.root !== meta || c.path.length) {
      throw new TypeError(`${what} takes columns of ${meta.name}, got ${String(c)}`);
    }
    return c.field.ir;
  });
}

function count(n: number | null | ParamRef<string>, what: string): number | ParamRef<string> | undefined {
  if (n === null) {
    return undefined;
  }
  if (n instanceof ParamRef) {
    return n;
  }
  if (!Number.isSafeInteger(n) || n < 0) {
    throw new RangeError(`${what}() takes a non-negative integer, got ${String(n)}`);
  }
  return n;
}

function one<R>(meta: ModelMeta, objs: R[]): R {
  if (!objs.length) {
    throw new (meta.model.DoesNotExist)(`no ${meta.name} matches the query`);
  }
  if (objs.length > 1) {
    throw new (meta.model.MultipleObjectsReturned)(`more than one ${meta.name} matches the query`);
  }
  return objs[0]!;
}

/** The model object a value is, if any. */
function modelOf(x: unknown): boolean {
  return (x as { _meta?: { model?: unknown } })._meta?.model === x;
}

// -- prepared queries -------------------------------------------------------------------------------

/** One statement of a prepared query: its IR JSON and parameters, with the `param()`
 * slots to fill per call. */
class Compiled {
  readonly json: string;
  readonly params: unknown[];
  readonly slots: [number, string, ((v: unknown) => unknown) | undefined][];
  readonly names: Set<string>;

  constructor(ir: IR, params: SlotParams) {
    this.json = JSON.stringify(ir);
    this.params = [...params];
    this.slots = [];
    params.forEach((p, i) => {
      if (p instanceof Slot) {
        this.slots.push([i, p.name, p.transform]);
      }
    });
    this.names = new Set(this.slots.map(([, n]) => n));
  }

  bind(values: Record<string, unknown>, known: Set<string>): unknown[] {
    const missing = [...this.names].filter((n) => !(n in values));
    if (missing.length) {
      throw new TypeError(`missing values for ${missing.sort().join(", ")}`);
    }
    const unknown = Object.keys(values).filter((n) => !known.has(n));
    if (unknown.length) {
      throw new TypeError(`the prepared query has no param ${unknown.sort().join(", ")}`);
    }
    const params = [...this.params];
    for (const [i, name, transform] of this.slots) {
      const v = values[name];
      if (v === null || v === undefined) {
        throw new TypeError(`param(${JSON.stringify(name)}) can't be null; compare with null (IS NULL) in the query itself`);
      }
      params[i] = transform ? transform(v) : v;
    }
    return params;
  }
}

type Values<P> = [keyof P] extends [never] ? [values?: Record<string, never>] : [values: { readonly [K in keyof P]: P[K] }];

/**
 * A query compiled by `QuerySet.prepare()`. Each call skips building the query (the
 * expression tree, the IR and its JSON) and only binds the values.
 */
export class Prepared<M extends ModelSpec, R, P> {
  private readonly compiled = new Map<string, Compiled>();
  /** The names of the values each call takes. */
  readonly params: ReadonlySet<string>;

  /** @internal */
  constructor(private readonly qs: QuerySet<M, R>) {
    this.params = this.statement("select").names;
  }

  private statement(kind: string): Compiled {
    let c = this.compiled.get(kind);
    if (!c) {
      let qs = this.qs as QuerySet<M, R> & { clone: QuerySet<M, R>["clone"] };
      let op = "select";
      if (kind === "get") {
        qs = qs.limit(2) as never;
      } else if (kind === "first") {
        qs = (qs as never as { clone(c: Partial<QueryState>): typeof qs }).clone({ order: qs.defaultOrder() });
        qs = (qs.state.limit instanceof ParamRef ? qs.limit(1) : qs.slice(0, 1)) as never;
      } else if (kind === "count" || kind === "exists") {
        op = kind;
      }
      const params = new SlotParams();
      c = new Compiled(qs.selectIr(op, params), params);
      this.compiled.set(kind, c);
    }
    return c;
  }

  private start(kind: string, values: Record<string, unknown> | undefined): Promise<unknown> {
    const c = this.statement(kind);
    const params = c.bind(values ?? {}, this.params as Set<string>);
    checkRunnable(this.qs.meta, this.qs.state);
    return this.qs.db().runJson(c.json, params);
  }

  /** The rows. */
  async all(...[values]: Values<P>): Promise<R[]> {
    const res = (await this.start("select", values)) as NativeSelect;
    return new Builder(this.qs.db().registry, this.qs.state.db).select(res) as R[];
  }

  async get(...[values]: Values<P>): Promise<R> {
    const res = (await this.start("get", values)) as NativeSelect;
    return one(this.qs.meta, new Builder(this.qs.db().registry, this.qs.state.db).select(res) as R[]);
  }

  async first(...[values]: Values<P>): Promise<R | null> {
    const res = (await this.start("first", values)) as NativeSelect;
    return (new Builder(this.qs.db().registry, this.qs.state.db).select(res)[0] as R | undefined) ?? null;
  }

  async count(...[values]: Values<P>): Promise<number> {
    return (await this.start("count", values)) as number;
  }

  async exists(...[values]: Values<P>): Promise<boolean> {
    return (await this.start("exists", values)) as boolean;
  }

  /** The SELECT for these values, parameters inlined (for debugging). */
  sql(...[values]: Values<P>): string {
    const c = this.statement("select");
    const params = c.bind(values ?? {}, this.params as Set<string>);
    return call(() => this.qs.meta.registry.native().sql(c.json, params));
  }
}

// -- related sets -------------------------------------------------------------------------------------

type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;

/**
 * `user.posts`: the rows of a to-many relation of one instance, as a query set (filter,
 * order, count, ...). `insert()` fills in the key. After `prefetchRelated(User.posts)`,
 * `user.posts.cached` holds the loaded rows (the type has it only then), and `all()` on
 * the unchanged set gives them without a query.
 */
export class RelatedSet<M extends ModelSpec, L extends string = never> extends QuerySet<M> {
  /** @internal */
  readonly relation!: RelationMeta;
  /** @internal */
  readonly instance!: Record<PropertyKey, unknown>;

  /** @internal */
  pristine(): boolean {
    const s = this.state;
    return s.filters.length === 1 && !s.order.length && s.limit === undefined && s.offset === undefined && !s.prefetch.length && !s.related.length;
  }

  /** @internal */
  override async fetch(): Promise<M["row"][]> {
    const rows = loadedRows(this.instance, this.relation);
    if (rows !== undefined && this.pristine()) {
      return [...rows] as M["row"][];
    }
    if (debugging() && this.pristine()) {
      const owner = (this.instance.constructor as unknown as { meta: { name: string } }).meta.name;
      return relationLoad(owner, this.relation.name, "prefetchRelated", () => super.fetch());
    }
    return super.fetch();
  }

  private link(): Record<string, unknown> {
    const meta = this.meta;
    const via = meta.fieldByIr.get(this.relation.to)!.name;
    const owner = (this.instance.constructor as unknown as { meta: ModelMeta }).meta;
    return { [via]: fieldValue(this.instance, owner.fieldByIr.get(this.relation.from)!.name) };
  }

  /** Inserts a related row pointing at this instance. */
  override insert(values: DistributiveOmit<M["insert"], L>): Promise<M["row"]>;
  override insert(values: DistributiveOmit<M["insert"], L>, options: DoNothing<M>): Promise<M["row"] | null>;
  override insert(values: DistributiveOmit<M["insert"], L>, options: DoUpdate<M>): Promise<M["row"]>;
  override async insert(values: object, options?: InsertOptions<M>): Promise<M["row"] | null> {
    const rows = await this.insertRows([{ ...values, ...this.link() }], options);
    return rows[0] ?? null;
  }

  /** Inserts related rows pointing at this instance. */
  override insertMany(rows: readonly DistributiveOmit<M["insert"], L>[], options?: InsertOptions<M>): Promise<M["row"][]> {
    const link = this.link();
    return this.insertRows(rows.map((r) => ({ ...r, ...link })), options);
  }
}

/**
 * `post.tags`: the rows a many-to-many relation links to one instance: a query set over
 * the related model (through the join model) that also changes the links: `add()`,
 * `remove()`, `set()`, `clear()` and `insert()` write rows of the join model.
 */
export class ManyRelatedSet<M extends ModelSpec> extends QuerySet<M> {
  /** @internal */
  readonly relation!: RelationMeta;
  /** @internal */
  readonly instance!: Record<PropertyKey, unknown>;

  /** @internal */
  override async fetch(): Promise<M["row"][]> {
    const rows = loadedRows(this.instance, this.relation);
    const s = this.state;
    const pristine = s.filters.length === 1 && !s.order.length && s.limit === undefined && s.offset === undefined && !s.prefetch.length && !s.related.length;
    if (rows !== undefined && pristine) {
      return [...rows] as M["row"][];
    }
    if (debugging() && pristine) {
      const owner = (this.instance.constructor as unknown as { meta: { name: string } }).meta.name;
      return relationLoad(owner, this.relation.name, "prefetchRelated", () => super.fetch());
    }
    return super.fetch();
  }

  private get through(): ModelMeta {
    return this.meta.registry.get(this.relation.through!.model);
  }

  private key(): unknown {
    const owner = (this.instance.constructor as unknown as { meta: ModelMeta }).meta;
    return fieldValue(this.instance, owner.fieldByIr.get(this.relation.from)!.name);
  }

  /** The join rows of this instance. */
  private links(): QuerySet<ModelSpec> {
    const join = this.through;
    const source = join.column(join.fieldByIr.get(this.relation.through!.source)!);
    const db = this.state.db ?? (this.instance[DB] as Database | undefined);
    return join.objects.using(db).filter(source.eq(this.key() as never)) as never;
  }

  private targetColumn(): Column<unknown, string> {
    const join = this.through;
    return join.column(join.fieldByIr.get(this.relation.through!.target)!);
  }

  /** Keys of related instances (or the keys themselves). */
  private targetKeys(objs: readonly unknown[]): unknown[] {
    const to = this.meta.fieldByIr.get(this.relation.to)!.name;
    const keys: unknown[] = [];
    for (const o of objs) {
      if (o !== null && typeof o === "object" && !(o instanceof Date)) {
        const m = (o.constructor as { meta?: ModelMeta }).meta;
        if (m !== this.meta) {
          throw new TypeError(`${this.relation.name} links ${this.meta.name} rows, not ${String(o)}`);
        }
        keys.push(fieldValue(o, to));
      } else {
        keys.push(o);
      }
    }
    const seen = new Set<unknown>();
    return keys.filter((k) => {
      const id = typeof k === "number" ? BigInt(k) : k;
      if (seen.has(id)) {
        return false;
      }
      seen.add(id);
      return true;
    });
  }

  private forget(): void {
    // prefetched rows no longer match the links
    const loaded = this.instance[RELATED] as Record<string, unknown> | undefined;
    if (loaded) {
      delete loaded[this.relation.name];
    }
  }

  /** Links the given instances (or keys); links that exist are left alone. */
  async add(...objs: readonly (M["data"] | In<M["pk"]>)[]): Promise<void> {
    const keys = this.targetKeys(objs);
    if (!keys.length) {
      return;
    }
    const col = this.targetColumn();
    const join = this.through;
    const have = (await this.links().filter(col.in(keys as never) as never).all()) as Record<string, unknown>[];
    const known = new Set(have.map((r) => String(r[col.field.name])));
    const source = join.fieldByIr.get(this.relation.through!.source)!.name;
    const rows = keys.filter((k) => !known.has(String(k))).map((k) => ({ [source]: this.key(), [col.field.name]: k }));
    if (rows.length) {
      await this.links().insertMany(rows as never);
    }
    this.forget();
  }

  /** Unlinks the given instances (or keys); gives the number of links removed. */
  async remove(...objs: readonly (M["data"] | In<M["pk"]>)[]): Promise<number> {
    const keys = this.targetKeys(objs);
    if (!keys.length) {
      return 0;
    }
    const n = await this.links().filter(this.targetColumn().in(keys as never) as never).delete();
    this.forget();
    return n;
  }

  /** Removes every link of this instance; gives how many there were. */
  async clear(): Promise<number> {
    const n = await this.links().delete();
    this.forget();
    return n;
  }

  /** Makes the given instances (or keys) exactly the linked ones. */
  async set(objs: readonly (M["data"] | In<M["pk"]>)[]): Promise<void> {
    const keys = this.targetKeys(objs);
    await this.links().filter(this.targetColumn().notIn(keys as never) as never).delete();
    await this.add(...(keys as never[]));
  }

  /** Inserts a related row and links it, in one transaction. */
  override insert(values: M["insert"]): Promise<M["row"]>;
  override insert(values: M["insert"], options: DoNothing<M>): Promise<M["row"] | null>;
  override insert(values: M["insert"], options: DoUpdate<M>): Promise<M["row"]>;
  override async insert(values: M["insert"], options?: InsertOptions<M>): Promise<M["row"] | null> {
    const db = resolve(this.state.db ?? (this.instance[DB] as Database | undefined));
    return db.transaction(async () => {
      const qs = new QuerySet<M>(this.meta).using(this.state.db ?? (this.instance[DB] as Database | undefined));
      const [obj] = await (qs as unknown as { insertRows: QuerySet<M>["insertRows"] }).insertRows([values], options);
      if (obj) {
        await this.add(obj as never);
      }
      return obj ?? null;
    });
  }

  /** Not supported: insert the rows, then link them with `add()`. */
  override insertMany(): never {
    throw new TypeError(`${this.relation.name}.insertMany() isn't supported; insert the rows, then link them with add()`);
  }
}

function loadedRows(instance: Record<PropertyKey, unknown>, rel: RelationMeta): unknown[] | undefined {
  const loaded = instance[RELATED] as Record<string, unknown> | undefined;
  return loaded && rel.name in loaded ? (loaded[rel.name] as unknown[]) : undefined;
}

/** `cached` is on the prototypes but not in the types: it exists only after
 * `prefetchRelated`, whose row type adds it. */
for (const cls of [RelatedSet, ManyRelatedSet]) {
  Object.defineProperty(cls.prototype, "cached", {
    get(this: RelatedSet<ModelSpec>) {
      const rows = loadedRows(this.instance, this.relation);
      if (rows === undefined) {
        const owner = (this.instance.constructor as unknown as { meta: ModelMeta }).meta;
        throw new NotLoaded(`${owner.name}.${this.relation.name} was not prefetched`);
      }
      return [...rows];
    },
  });
}

function makeRelatedSet(rel: RelationMeta, instance: object): RelatedSet<ModelSpec, never> {
  const inst = instance as Record<PropertyKey, unknown>;
  const owner = (inst.constructor as unknown as { meta: ModelMeta }).meta;
  const target = owner.registry.get(rel.target);
  const via = target.column(target.fieldByIr.get(rel.to)!);
  const key = fieldValue(inst, owner.fieldByIr.get(rel.from)!.name);
  const qs = new RelatedSet<ModelSpec>(target, { ...EMPTY, filters: [asCondition(via.eq(key as never))] });
  Object.assign(qs, { relation: rel, instance: inst });
  return qs as never;
}

function makeManyRelatedSet(rel: RelationMeta, instance: object): ManyRelatedSet<ModelSpec> {
  const inst = instance as Record<PropertyKey, unknown>;
  const owner = (inst.constructor as unknown as { meta: ModelMeta }).meta;
  const target = owner.registry.get(rel.target);
  const join = owner.registry.get(rel.through!.model);
  const source = join.column(join.fieldByIr.get(rel.through!.source)!);
  const tcol = join.column(join.fieldByIr.get(rel.through!.target)!);
  const to = target.column(target.fieldByIr.get(rel.to)!);
  const key = fieldValue(inst, owner.fieldByIr.get(rel.from)!.name);
  const cond = exists(join.objects.filter(source.eq(key as never) as never, tcol.eq(outer(to) as never) as never) as never);
  const qs = new ManyRelatedSet<ModelSpec>(target, { ...EMPTY, filters: [cond] });
  Object.assign(qs, { relation: rel, instance: inst });
  return qs;
}

registerQueries((meta) => new QuerySet(meta), makeRelatedSet, makeManyRelatedSet);

// -- CTE / select hooks (cte.ts and select.ts register themselves) ---------------------------------

export interface CteOptions<N extends string, C, P, X extends string> {
  /** The recursive part, built from the CTE itself (`WITH RECURSIVE`). */
  readonly recursive?: (self: CteSelf<N, C>) => Subquery & CteQuery<P, X>;
  /** `UNION` instead of `UNION ALL` between the parts. */
  readonly distinct?: boolean;
  /** `MATERIALIZED` / `NOT MATERIALIZED`. */
  readonly materialized?: boolean;
}

/** A query usable as a CTE body. */
export interface CteQuery<P, X extends string> {
  /** @internal */
  readonly "~exists"?: [X, P];
  cteIr(params: unknown[], ctes: Ctes): IR;
}

let makeCte: (name: string, query: QuerySet<ModelSpec, unknown, string, unknown, string> | Select<ModelSpec, object, string, unknown, string, unknown>, options: CteOptions<string, unknown, unknown, string>) => Cte<string, unknown, ModelSpec | null>;
let makeSelect: (qs: QuerySet<ModelSpec, unknown, string, unknown, string>, items: object) => Select<ModelSpec, object, string, unknown, string, unknown>;
let isCte: (x: unknown) => x is Cte<string, unknown, ModelSpec | null>;

/** @internal */
export function registerCte(make: typeof makeCte, check: typeof isCte): void {
  makeCte = make;
  isCte = check;
}

/** @internal */
export function registerSelect(make: typeof makeSelect): void {
  makeSelect = make;
}

export type { ModelClass };
export { Condition };
