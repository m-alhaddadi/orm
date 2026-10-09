/**
 * `select()`: queries giving chosen columns and aggregates instead of instances.
 *
 * ```ts
 * const rows = await Post.objects
 *   .filter(Post.published)
 *   .select({ authorId: Post.authorId, posts: func.count(), views: func.sum(Post.views) })
 *   .groupBy(Post.authorId)
 *   .having(func.count().gt(2))
 *   .all();                                   // { authorId: bigint; posts: bigint; views: bigint | null }[]
 *
 * await Post.objects.select({ top: func.max(Post.views) }).scalar();   // number | null
 * await Post.objects.select({ id: Post.id }).scalars();                // bigint[]
 * await Post.objects.select({ post: Post, comments: func.count(Post.comments) }).all();
 * ```
 */

import { Builder } from "./build.js";
import { withScope, type Database } from "./db.js";
import { DoesNotExist, MultipleObjectsReturned, QueryError, TransactionRequired } from "./errors.js";
import {
  Column,
  Expression,
  IRContext,
  Ordering,
  ParamRef,
  ScalarSubquery,
  and,
  orderings,
  type Ctes,
  type IR,
  type Node,
  type ParamValues,
  type ResolveOuter,
} from "./expr.js";
import type { ModelSpec } from "./meta.js";
import { modelMeta, type ModelClass } from "./model.js";
import { call, type NativeSelect } from "./native.js";
import {
  QuerySet,
  registerSelect,
  type Allowed,
  type AllowedOne,
  type CteOptions,
  type FieldOrder,
  type OuterRefs,
  type ParamsOfAll,
  type Awaitable,
  type Runnable,
  cachedRows,
  type ScopesOf,
  type UnionToIntersection,
} from "./query.js";
import { makeCte, type Cte } from "./cte.js";

/** What `select()` takes: expressions over the query's sources, or the model itself. */
export type SelectItems<M extends ModelSpec, S extends string> = {
  readonly [key: string]: Expression<unknown, AllowedOne<S>, unknown> | ModelClass<M>;
};

/** The row an item set gives. */
export type SelectRow<I> = {
  -readonly [K in keyof I]: I[K] extends Expression<infer T, string, unknown> ? T : I[K] extends ModelClass<infer M> ? M["row"] : never;
};

export type ItemsParams<I> = UnionToIntersection<
  { [K in keyof I]: I[K] extends Expression<unknown, string, infer P> ? P : {} }[keyof I]
>;

export type ItemsOuter<I, Own extends string> = {
  [K in keyof I]: I[K] extends Expression<unknown, infer S, unknown> ? OuterRefs<S, Own> : never;
}[keyof I];

/** The columns of a CTE made from a `select()`: a selected model gives its fields. */
export type CteColumnsOf<I> = UnionToIntersection<
  {
    [K in keyof I]: I[K] extends Expression<infer T, string, unknown>
      ? { readonly [P in K]: T }
      : I[K] extends ModelClass<infer M>
        ? M["data"]
        : never;
  }[keyof I]
>;

/** The value of the only column of row type `R` (`never` if it has several). */
export type OnlyValue<R> = OnlyKey<R, keyof R>;
type OnlyKey<R, K extends keyof R> = K extends unknown ? ([Exclude<keyof R, K>] extends [never] ? R[K] : never) : never;

type OneColumn<R> = [OnlyValue<R>] extends [never] ? [error: "this needs a select() of exactly one column"] : [];

/**
 * A query giving rows of chosen columns. Built by `QuerySet.select()`; every method
 * returns a new `Select`, and nothing runs until a terminal method is called.
 *
 * `C` is the columns a CTE made from it has.
 */
export class Select<M extends ModelSpec, Row extends object, S extends string, P, X extends string, C = Row> {
  /** @internal */
  declare readonly "~exists"?: [X, P];
  /** @internal */
  declare readonly "~column"?: [OnlyValue<Row>] extends [never] ? "not one column" : [OnlyValue<Row>, ResolveOuter<X>, P];

  /** @internal */
  constructor(
    readonly qs: QuerySet<M, unknown, string, unknown, string>,
    readonly items: readonly [string, Node | null][],
    private readonly state: {
      readonly group: readonly Node[];
      readonly having: readonly Node[];
      readonly distinct: boolean;
      readonly distinctOn: readonly Column<unknown, string>[];
    } = { group: [], having: [], distinct: false, distinctOn: [] },
  ) {}

  private with(qs: QuerySet<M, unknown, string, unknown, string>, state = this.state): never {
    return new Select(qs, this.items, state) as never;
  }

  // -- building (WHERE / ORDER BY / LIMIT go to the query set) --------------------------------

  filter<const Cs extends readonly Expression<boolean | null, Allowed<S>, unknown>[]>(
    ...conditions: Cs
  ): Select<M, Row, S, P & ParamsOfAll<Cs>, X | OuterRefs<ScopesOf<Cs>, S>, C> {
    return this.with(this.qs.filter(...(conditions as unknown as never[])) as never);
  }

  exclude<const Cs extends readonly Expression<boolean | null, Allowed<S>, unknown>[]>(
    ...conditions: Cs
  ): Select<M, Row, S, P & ParamsOfAll<Cs>, X | OuterRefs<ScopesOf<Cs>, S>, C> {
    return this.with(this.qs.exclude(...(conditions as unknown as never[])) as never);
  }

  orderBy<const Cs extends readonly (Expression<unknown, AllowedOne<S>, unknown> | Ordering<AllowedOne<S>, unknown> | FieldOrder<M>)[]>(
    ...items: Cs
  ): Select<M, Row, S, P & ParamsOfAll<Cs>, X | OuterRefs<ScopesOf<Cs>, S>, C> {
    return this.with(this.qs.orderBy(...(items as unknown as never[])) as never);
  }

  limit<N extends string>(n: ParamRef<N>): Select<M, Row, S, P & ParamValues<N, number | bigint>, X, C>;
  limit(n: number | null): Select<M, Row, S, P, X, C>;
  limit(n: number | null | ParamRef<string>): unknown {
    return this.with(this.qs.limit(n as never) as never);
  }

  offset<N extends string>(n: ParamRef<N>): Select<M, Row, S, P & ParamValues<N, number | bigint>, X, C>;
  offset(n: number | null): Select<M, Row, S, P, X, C>;
  offset(n: number | null | ParamRef<string>): unknown {
    return this.with(this.qs.offset(n as never) as never);
  }

  /** Rows `start` to `end` (exclusive), like `Array.slice`. */
  slice(start: number, end?: number): Select<M, Row, S, P, X, C> {
    return this.with(this.qs.slice(start, end) as never);
  }

  using(db: Database | undefined): Select<M, Row, S, P, X, C> {
    return this.with(this.qs.using(db) as never);
  }

  /** `GROUP BY`: aggregates in the select list then summarize each group. The model
   * itself (`groupBy(Post)`) groups by its primary key. */
  groupBy<const Cs extends readonly (Expression<unknown, AllowedOne<S>, unknown> | ModelClass<M>)[]>(
    ...exprs: Cs
  ): Select<M, Row, S, P & ParamsOfAll<Cs>, X | OuterRefs<ScopesOf<Cs>, S>, C> {
    const out = exprs.map((e) => {
      const meta = modelMeta(e);
      if (meta) {
        if (meta !== this.qs.meta) {
          throw new TypeError(`groupBy() takes ${this.qs.meta.name} itself or expressions`);
        }
        return meta.column(meta.pk);
      }
      if (!(e instanceof Expression)) {
        throw new TypeError(`groupBy() takes columns and expressions, got ${String(e)}`);
      }
      return e;
    });
    return this.with(this.qs, { ...this.state, group: [...this.state.group, ...out] });
  }

  /** Keep groups matching all `conditions`: `having(func.count().gt(2))`. */
  having<const Cs extends readonly Expression<boolean | null, Allowed<S>, unknown>[]>(
    ...conditions: Cs
  ): Select<M, Row, S, P & ParamsOfAll<Cs>, X | OuterRefs<ScopesOf<Cs>, S>, C> {
    if (!conditions.length) {
      return this as never;
    }
    return this.with(this.qs, { ...this.state, having: [...this.state.having, and(...conditions)] });
  }

  /** `SELECT DISTINCT`; with columns, Postgres' `DISTINCT ON (...)`: the first row (by
   * `orderBy`) of each distinct value. */
  distinct(...on: readonly Column<unknown, S>[]): Select<M, Row, S, P, X, C> {
    return this.with(this.qs, { ...this.state, distinct: true, distinctOn: on });
  }

  // -- IR -------------------------------------------------------------------------------------

  private build(params: unknown[], outer?: IRContext, ctes?: Ctes): [IR, IRContext] {
    const [ir, ctx] = this.qs.queryIr("select", params, outer, ctes);
    ir["columns"] = this.items.map(([name, node]) => (node === null ? { t: "model" } : { t: "expr", expr: node.ir(ctx), name }));
    if (this.state.group.length) {
      ir["group_by"] = this.state.group.map((g) => g.ir(ctx));
    }
    if (this.state.having.length) {
      ir["having"] = this.state.having.map((h) => h.ir(ctx));
    }
    if (this.state.distinctOn.length) {
      ir["distinct_on"] = this.state.distinctOn.map((c) => c.ir(ctx));
    } else if (this.state.distinct) {
      ir["distinct"] = true;
    }
    ctx.addWindows(ir);
    return [ir, ctx];
  }

  /** @internal */
  ir(params: unknown[]): IR {
    const [ir, ctx] = this.build(params);
    return ctx.finish(ir);
  }

  /** @internal */
  subqueryIr(ctx: IRContext, what: string): IR {
    if (what !== "exists()" && (this.items.length !== 1 || this.items[0]![1] === null)) {
      throw new QueryError(`${what} takes a query that selects exactly one column`);
    }
    if (this.qs.state.lock) {
      throw new QueryError("a subquery can't lock rows");
    }
    return this.build(ctx.params, ctx)[0];
  }

  /** @internal */
  cteIr(params: unknown[], ctes: Ctes): IR {
    return this.build(params, undefined, ctes)[0];
  }

  /**
   * This one-column query as a value in another query: `(SELECT ...)`. It must give at
   * most one row (slice it with `.limit(1)`); no row is `NULL`. Correlate it with
   * `outer()`.
   */
  asScalar(...check: OneColumn<Row>): ScalarSubquery<OnlyValue<Row> | null, ResolveOuter<X>, P> {
    void check;
    this.oneColumn("asScalar");
    return new ScalarSubquery(this);
  }

  /** This query as a CTE (`WITH <name> AS (...)`) whose columns are the selected items
   * (`cte.c.<key>`; a selected model gives its fields). */
  cte<const N extends string>(
    name: N,
    options: CteOptions<N, C, P, X> = {},
  ): Cte<N, C, [Extract<Row[keyof Row], M["row"]>] extends [never] ? null : M> {
    return makeCte(name, this as never, options as never) as never;
  }

  /** The SELECT this runs, parameters inlined (for debugging). */
  sql(...check: Runnable<P, X>): string {
    void check;
    const params: unknown[] = [];
    const ir = this.ir(params);
    return call(() => this.qs.meta.registry.native().sql(...withScope(JSON.stringify(ir), params)));
  }

  // -- execution ------------------------------------------------------------------------------

  private async rows(sel: Select<M, Row, S, P, X, C> = this): Promise<Row[]> {
    const params: unknown[] = [];
    const ir = sel.ir(params);
    const db = sel.qs.db();
    if (sel.qs.state.lock && db.tx() === null) {
      throw new TransactionRequired(
        "lock() outside a transaction would release the locks as soon as the query ends; run it inside `db.transaction(...)`",
      );
    }
    const res = (await db.run(ir, params)) as NativeSelect;
    return new Builder(db.registry, sel.qs.state.db).select(res, this.items.map(([k]) => k)) as Row[];
  }

  /** The rows, queried afresh (unlike `await sel`, which reuses its first result). */
  all(...check: Runnable<P, X>): Promise<Row[]> {
    void check;
    return this.rows();
  }

  /** `await sel`: the rows, queried on the first `await` and reused by later ones. */
  then<A = Row[], B = never>(
    this: Select<M, Row, S, P, X, C> & Awaitable<P, X>,
    onfulfilled?: ((rows: Row[]) => A | PromiseLike<A>) | null,
    onrejected?: ((reason: unknown) => B | PromiseLike<B>) | null,
  ): Promise<A | B> {
    return cachedRows(this, () => this.rows()).then(onfulfilled, onrejected);
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<Row, void, undefined> {
    yield* await cachedRows(this, () => this.rows());
  }

  /** The first row (by `orderBy`; unordered otherwise), or `null`. */
  async first(...check: Runnable<P, X>): Promise<Row | null> {
    void check;
    return (await this.rows(this.slice(0, 1)))[0] ?? null;
  }

  /** The only row; throws `DoesNotExist` / `MultipleObjectsReturned` otherwise. */
  async one(...check: Runnable<P, X>): Promise<Row> {
    void check;
    const rows = await this.rows(this.limit(2));
    if (!rows.length) {
      throw new DoesNotExist("the query returned no row");
    }
    if (rows.length > 1) {
      throw new MultipleObjectsReturned("the query returned more than one row");
    }
    return rows[0]!;
  }

  /** The single column of the first row, or `null` without rows. */
  async scalar(...check: [...OneColumn<Row>, ...Runnable<P, X>]): Promise<OnlyValue<Row> | null> {
    void check;
    this.oneColumn("scalar");
    const rows = await this.rows(this.slice(0, 1));
    return rows.length ? ((rows[0] as Record<string, unknown>)[this.items[0]![0]] as OnlyValue<Row>) : null;
  }

  /** The single column of every row. */
  async scalars(...check: [...OneColumn<Row>, ...Runnable<P, X>]): Promise<OnlyValue<Row>[]> {
    void check;
    this.oneColumn("scalars");
    const key = this.items[0]![0];
    return (await this.rows()).map((r) => (r as Record<string, unknown>)[key] as OnlyValue<Row>);
  }

  private oneColumn(what: string): void {
    if (this.items.length !== 1) {
      throw new QueryError(`${what}() needs a query selecting one column, this one selects ${this.items.length}`);
    }
  }

  toString(): string {
    return `Select(${this.qs.meta.name}: ${this.items.map(([k]) => k).join(", ")})`;
  }
}

/** @internal */
export function makeSelect(qs: QuerySet<ModelSpec, unknown, string, unknown, string>, items: object): Select<ModelSpec, object, string, unknown, string, unknown> {
  const entries = Object.entries(items);
  if (!entries.length) {
    throw new TypeError("select() needs at least one column");
  }
  if (qs.state.related.length || qs.state.prefetch.length) {
    throw new QueryError("select() can't be combined with selectRelated / prefetchRelated");
  }
  const out: [string, Node | null][] = entries.map(([key, item]) => {
    const meta = modelMeta(item);
    if (meta) {
      if (meta !== qs.meta) {
        throw new TypeError(`select() takes ${qs.meta.name} itself or expressions, not ${meta.name}`);
      }
      return [key, null];
    }
    if (!(item instanceof Expression)) {
      throw new TypeError(`select() takes columns and expressions, got ${String(item)} for ${key}`);
    }
    return [key, item];
  });
  return new Select(qs, out);
}

registerSelect(makeSelect);

export { orderings };
