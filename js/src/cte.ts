/**
 * Common table expressions: `WITH <name> AS (<query>)`.
 *
 * ```ts
 * const ranked = Post.objects
 *   .select({ post: Post, rank: func.rowNumber().over({ partitionBy: Post.authorId, orderBy: Post.views.desc() }) })
 *   .cte("ranked");
 * await Post.objects.from(ranked).filter(ranked.c.rank.lte(3)).all();      // instances, read from the CTE
 * await ranked.select({ author: ranked.c.authorId, best: func.max(ranked.c.views) }).groupBy(ranked.c.authorId).all();
 *
 * const chain = User.objects.filter(User.id.eq(1n)).cte("chain", {
 *   recursive: (c) => User.objects.filter(User.id.eq(c.c.id.add(1))),       // WITH RECURSIVE
 * });
 * ```
 *
 * A query that reads a CTE declares it, so every CTE a statement (or its subqueries)
 * reads ends up once in its `WITH` clause.
 */

import { QueryError } from "./errors.js";
import { Ctes, Expression, IRContext, type CteLike, type IR, type Source } from "./expr.js";
import type { ModelSpec } from "./meta.js";
import type { ModelMeta } from "./model.js";
import { QuerySet, registerCte, type CteOptions } from "./query.js";
import { makeSelect, type CteColumnsOf, type ItemsOuter, type ItemsParams, type Select, type SelectRow } from "./select.js";
import type { Simplify } from "./query.js";

/** `cte.c.<name>`: a column of a CTE. */
export class CteColumn<T, N extends string> extends Expression<T, N, {}> {
  /** @internal */
  constructor(
    readonly cte: Cte<N, unknown, ModelSpec | null>,
    /** The column's name in the CTE (a field's schema name, or a select() key). */
    readonly column: string,
    readonly label: string,
  ) {
    super();
  }

  ir(ctx: IRContext): IR {
    ctx.useCte(this.cte as unknown as CteLike);
    return { t: "cte_col", cte: this.cte.name, name: this.column };
  }

  override toString(): string {
    return `${this.cte.name}.c.${this.label}`;
  }
}

/** The columns of a CTE, typed. */
export type CteColumns<N extends string, C> = { readonly [K in keyof C]-?: CteColumn<C[K], N> };

/** The CTE as its own recursive part sees it: its columns read anywhere in that part
 * (they put the CTE in `FROM`). */
export interface CteSelf<N extends string, C> {
  readonly name: N;
  readonly c: { readonly [K in keyof C]-?: CteColumn<C[K], never> };
}

type CteSpec<N extends string> = { name: N; row: never; data: never; insert: never; update: never; updateRow: never; pk: never };

/**
 * A named query (`WITH`). Build it with `qs.cte(name)` or `qs.select({...}).cte(name)`;
 * read it with `Model.objects.from(cte)` (when it has the model's columns), `qs.join(cte,
 * on)` or `cte.select({...})`. `cte.c.<name>` are its columns.
 *
 * `C` is its columns' value types; `M` the model whose columns it has, if any.
 */
export class Cte<N extends string, C, M extends ModelSpec | null> implements Source {
  /** @internal */
  declare readonly "~cte"?: [N, C, M];
  readonly cteKind = true as const;
  readonly c: CteColumns<N, C>;
  /** The model whose fields it has (all of them), if any. */
  readonly model: ModelMeta | undefined;
  /** Column names: TypeScript name -> name in the CTE. */
  readonly columns: ReadonlyMap<string, string>;
  private readonly recursivePart: { cteIr(params: unknown[], ctes: Ctes): IR } | undefined;
  private readonly distinct: boolean;
  private readonly materialized: boolean | undefined;

  /** @internal */
  constructor(
    readonly name: N,
    private readonly query: QuerySet<ModelSpec, unknown, string, unknown, string> | Select<ModelSpec, object, string, unknown, string, unknown>,
    options: CteOptions<N, C, unknown, string>,
  ) {
    this.distinct = options.distinct ?? false;
    this.materialized = options.materialized;
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
      throw new TypeError(`CTE name ${JSON.stringify(name)} must be an identifier`);
    }
    const columns = new Map<string, string>();
    const addModel = (meta: ModelMeta): void => {
      for (const f of meta.fieldList) {
        add(f.name, f.ir);
      }
    };
    const add = (ts: string, col: string): void => {
      if (columns.has(ts)) {
        throw new QueryError(`CTE ${name} has several columns named ${ts}`);
      }
      columns.set(ts, col);
    };
    if (query instanceof QuerySet) {
      const s = query.state;
      if (s.prefetch.length || s.related.length || s.lock) {
        throw new QueryError("a CTE's query can't prefetch, selectRelated or lock");
      }
      this.model = query.meta;
      addModel(query.meta);
    } else {
      this.model = query.items.some(([, n]) => n === null) ? query.qs.meta : undefined;
      for (const [key, node] of query.items) {
        if (node === null) {
          addModel(query.qs.meta);
        } else {
          add(key, key);
        }
      }
    }
    this.columns = columns;
    const c: Record<string, CteColumn<unknown, N>> = {};
    for (const [ts, col] of columns) {
      c[ts] = new CteColumn(this as never, col, ts);
    }
    this.c = Object.freeze(c) as never;
    this.recursivePart = options.recursive?.(this as never);
  }

  /** @internal */
  bodyIr(params: unknown[], ctes: Ctes): IR {
    const ir: IR = { name: this.name, query: this.query.cteIr(params, ctes) };
    if (this.recursivePart) {
      ir["recursive"] = this.recursivePart.cteIr(params, ctes);
      if (this.distinct) {
        ir["distinct"] = true;
      }
    }
    if (this.materialized !== undefined) {
      ir["materialized"] = this.materialized;
    }
    return ir;
  }

  /**
   * Rows of this CTE's columns (and expressions over them): filter, group, order and
   * slice it like any `select()`, run it, or use it in `in()`, `exists()` and
   * `asScalar()`.
   */
  select<const I extends { readonly [key: string]: Expression<unknown, N | `^${string}` | `~${string}`, unknown> }>(
    items: I,
  ): Select<CteSpec<N>, Simplify<SelectRow<I>>, N, ItemsParams<I>, ItemsOuter<I, N>, CteColumnsOf<I>> {
    const base = this.query instanceof QuerySet ? this.query.meta : this.query.qs.meta;
    return makeSelect(new CteQuerySet(base, this as never) as never, items) as never;
  }

  toString(): string {
    return `Cte(${this.name}: ${[...this.columns.keys()].join(", ")})`;
  }
}

/** The rows of a CTE without a model, for `cte.select()`. */
class CteQuerySet extends QuerySet<ModelSpec> {
  constructor(
    base: ModelMeta,
    private readonly view: Cte<string, unknown, ModelSpec | null>,
  ) {
    super(base);
  }

  /** @internal */
  override context(params: unknown[], outer: IRContext | undefined, ctes: Ctes | undefined): IRContext {
    const ctx = new IRContext(this.view, params, outer, ctes);
    ctx.useCte(this.view as unknown as CteLike);
    return ctx;
  }

  protected override source(): Source {
    return this.view;
  }

  protected override rootName(): string {
    return this.view.name;
  }

  override toString(): string {
    return `CteQuerySet(${this.view.name})`;
  }
}

/** @internal */
export function makeCte(
  name: string,
  query: QuerySet<ModelSpec, unknown, string, unknown, string> | Select<ModelSpec, object, string, unknown, string, unknown>,
  options: CteOptions<string, object, unknown, string>,
): Cte<string, unknown, ModelSpec | null> {
  return new Cte(name, query, options as never);
}

registerCte(makeCte, (x): x is Cte<string, unknown, ModelSpec | null> => x instanceof Cte);

export type { CteColumnsOf };
