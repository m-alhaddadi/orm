/**
 * Expressions for filters, ordering, `select()` and updates.
 *
 * `User.email` is a {@link Column}; `User.email.eq("a@b.c")` is a {@link Condition}.
 * `User.posts` is a {@link RelationPath} whose properties are the related model's
 * columns and relations, so `User.posts.createdAt.lt(yesterday)` is a condition on a
 * column reached through a relation. Nothing here talks to the database: expressions
 * compile to the IR the native engine plans (`core/src/ir.rs`).
 *
 * Three type parameters ride along every expression, checked at compile time:
 *
 * * `T`: the value type (`string | null` for a nullable column);
 * * `S`: what the expression reads: the model names (and CTE names) its columns belong
 *   to, `^User` for `outer(User.id)`, and `*many` when it reads a to-many relation path
 *   outside an aggregate (fine in filters, rejected where it would repeat rows);
 * * `P`: the `param()` placeholders it uses, with their value types, for `prepare()`.
 */

import { Decimal } from "./decimal.js";
import { QueryError } from "./errors.js";
import type { FieldMeta, Hop, In, JsonValue, ModelSpec } from "./meta.js";

export type IR = { [key: string]: unknown };

/** Marks expressions that read a to-many relation path outside an aggregate. */
export type Many = "*many";
/** `^User`: a column of an enclosing `User` query (`outer(User.id)`). */
export type OuterOf<S extends string> = `^${S}`;
/** `~User`: a column of the nearest `User` query, this one or an enclosing one: what an
 * `outer(User.id)` reference becomes one query up (`exists()`, `in()`, `asScalar()`). */
export type NearestOf<S extends string> = `~${S}`;
/** The scopes `outer()` references turn into one query up. */
export type ResolveOuter<X extends string> = X extends `^${infer R}` ? NearestOf<R> : never;

/** Values of numeric columns compare and combine with each other. */
type Numeric = number | bigint | Decimal;
/** The expression types a value of type `T` compares with. */
export type Compat<T> = [NonNullable<T>] extends [Numeric] ? Numeric | null : NonNullable<T> | null;

// -- compile context ------------------------------------------------------------------------

/** A source rows are read from: a model or a CTE. Compared by identity. */
export interface Source {
  readonly name: string;
}

/** Something that compiles to a CTE body: `QuerySet` / `Select`. */
export interface CteQuery {
  cteIr(params: unknown[], ctes: Ctes): IR;
}

/** A CTE as the compiler sees it. */
export interface CteLike extends Source {
  readonly cteKind: true;
  bodyIr(params: unknown[], ctes: Ctes): IR;
}

/** The CTEs one statement uses, in dependency order, compiled once each. */
export class Ctes {
  readonly items: [CteLike, IR][] = [];
  private readonly building: CteLike[] = [];

  use(cte: CteLike, params: unknown[]): void {
    for (const [c] of this.items) {
      if (c === cte) {
        return;
      }
      if (c.name === cte.name) {
        throw new QueryError(`two different CTEs are named ${JSON.stringify(cte.name)} in one query`);
      }
    }
    if (this.building.includes(cte)) {
      return; // the recursive part of `cte` reading `cte`
    }
    this.building.push(cte);
    try {
      this.items.push([cte, cte.bodyIr(params, this)]);
    } finally {
      this.building.pop();
    }
  }
}

/** A `param()` placeholder's slot in a prepared query's parameter list. */
export class Slot {
  constructor(
    readonly name: string,
    readonly transform: ((value: unknown) => unknown) | undefined,
  ) {}
}

/** The parameter list of a query compiled by `prepare()`: the only one that takes
 * `param()` placeholders. */
export class SlotParams extends Array<unknown> {}

/**
 * Collects literal parameters and CTEs while compiling, and checks that every column
 * belongs to the query's root model (or CTE). `outer` is the enclosing query's context.
 */
export class IRContext {
  readonly ctes: Ctes;
  readonly windows: [WindowDef<string, object>, IR][] = [];
  private readonly owner: boolean;

  constructor(
    readonly root: Source,
    readonly params: unknown[],
    readonly outer?: IRContext,
    ctes?: Ctes,
  ) {
    this.owner = outer === undefined && ctes === undefined;
    this.ctes = ctes ?? outer?.ctes ?? new Ctes();
  }

  param(value: unknown): IR {
    this.params.push(value);
    return { t: "param", i: this.params.length - 1 };
  }

  useCte(cte: CteLike): void {
    this.ctes.use(cte, this.params);
  }

  /** The name of `w` in this query's `WINDOW` clause (declared on first use). */
  windowName(w: WindowDef<string, object>): string {
    const i = this.windows.findIndex(([known]) => known === w);
    if (i >= 0) {
      return `w${i + 1}`;
    }
    const name = `w${this.windows.length + 1}`;
    this.windows.push([w, { name, ...w.specIr(this) }]);
    return name;
  }

  addWindows(ir: IR): void {
    if (this.windows.length) {
      ir["windows"] = this.windows.map(([, w]) => w);
    }
  }

  /** Declares the statement's CTEs (`WITH`), when this context compiles the whole
   * statement rather than a part of it. */
  finish(ir: IR): IR {
    if (this.owner && this.ctes.items.length) {
      ir["with"] = this.ctes.items.map(([, c]) => c);
    }
    return ir;
  }
}

// -- nodes --------------------------------------------------------------------------------------

export abstract class Node {
  /** @internal */
  abstract ir(ctx: IRContext): IR;
}

/** A plain value, or an expression of value type `T`. */
export type Operand<T, S extends string, P> = In<T> | Expression<Compat<T>, S, P>;

/** A `param()` placeholder, typed by what it is compared with or assigned to. */
export type ParamValues<N extends string, T> = { [K in N]: T };

/**
 * A SQL value expression whose value type is `T`. Comparisons are methods (`eq`, `lt`,
 * ...): `.eq(null)` is `IS NULL`.
 */
export abstract class Expression<T, S extends string = never, P = {}> extends Node {
  /** @internal Phantom: the value type, scopes and params of the expression. */
  declare readonly "~types"?: [T, S, P];

  // Comparisons ----------------------------------------------------------------------------

  eq<N extends string>(value: ParamRef<N>): Condition<S, P & ParamValues<N, In<NonNullable<T>>>>;
  eq<S2 extends string = never, P2 = {}>(value: Operand<T, S2, P2>): Condition<S | S2, P & P2>;
  eq(value: unknown): Condition<string, unknown> {
    return value === null ? new IsNull(this, false) : new Comparison("eq", this, wrap(value));
  }

  ne<N extends string>(value: ParamRef<N>): Condition<S, P & ParamValues<N, In<NonNullable<T>>>>;
  ne<S2 extends string = never, P2 = {}>(value: Operand<T, S2, P2>): Condition<S | S2, P & P2>;
  ne(value: unknown): Condition<string, unknown> {
    return value === null ? new IsNull(this, true) : new Comparison("ne", this, wrap(value));
  }

  lt<N extends string>(value: ParamRef<N>): Condition<S, P & ParamValues<N, In<NonNullable<T>>>>;
  lt<S2 extends string = never, P2 = {}>(value: Operand<NonNullable<T>, S2, P2>): Condition<S | S2, P & P2>;
  lt(value: unknown): Condition<string, unknown> {
    return new Comparison("lt", this, wrap(value));
  }

  lte<N extends string>(value: ParamRef<N>): Condition<S, P & ParamValues<N, In<NonNullable<T>>>>;
  lte<S2 extends string = never, P2 = {}>(value: Operand<NonNullable<T>, S2, P2>): Condition<S | S2, P & P2>;
  lte(value: unknown): Condition<string, unknown> {
    return new Comparison("le", this, wrap(value));
  }

  gt<N extends string>(value: ParamRef<N>): Condition<S, P & ParamValues<N, In<NonNullable<T>>>>;
  gt<S2 extends string = never, P2 = {}>(value: Operand<NonNullable<T>, S2, P2>): Condition<S | S2, P & P2>;
  gt(value: unknown): Condition<string, unknown> {
    return new Comparison("gt", this, wrap(value));
  }

  gte<N extends string>(value: ParamRef<N>): Condition<S, P & ParamValues<N, In<NonNullable<T>>>>;
  gte<S2 extends string = never, P2 = {}>(value: Operand<NonNullable<T>, S2, P2>): Condition<S | S2, P & P2>;
  gte(value: unknown): Condition<string, unknown> {
    return new Comparison("ge", this, wrap(value));
  }

  /** `IN (values)`, or `IN (SELECT ...)` for a one-column `select()`. An empty list is
   * false. */
  in<S2 extends string = never, P2 = {}>(
    values: readonly In<NonNullable<T>>[] | SingleColumn<Compat<T>, S2, P2>,
  ): Condition<S | S2, P & P2> {
    return inValues(this, values, false) as Condition<S | S2, P & P2>;
  }

  /** `NOT IN (values)` / `NOT IN (SELECT ...)`. An empty list is true. */
  notIn<S2 extends string = never, P2 = {}>(
    values: readonly In<NonNullable<T>>[] | SingleColumn<Compat<T>, S2, P2>,
  ): Condition<S | S2, P & P2> {
    return inValues(this, values, true) as Condition<S | S2, P & P2>;
  }

  isNull(): Condition<S, P> {
    return new IsNull(this, false);
  }

  isNotNull(): Condition<S, P> {
    return new IsNull(this, true);
  }

  between<S2 extends string = never, P2 = {}, S3 extends string = never, P3 = {}>(
    low: Operand<NonNullable<T>, S2, P2>,
    high: Operand<NonNullable<T>, S3, P3>,
  ): Condition<S | S2 | S3, P & P2 & P3> {
    return this.gte(low as never).and(this.lte(high as never)) as never;
  }

  // String matching ------------------------------------------------------------------------

  like<N extends string>(this: Expression<string | null, S, P>, pattern: ParamRef<N>): Condition<S, P & ParamValues<N, string>>;
  like(this: Expression<string | null, S, P>, pattern: string): Condition<S, P>;
  like(pattern: string | ParamRef<string>): Condition<S, P> {
    return new Like(this, pattern, false);
  }

  ilike<N extends string>(this: Expression<string | null, S, P>, pattern: ParamRef<N>): Condition<S, P & ParamValues<N, string>>;
  ilike(this: Expression<string | null, S, P>, pattern: string): Condition<S, P>;
  ilike(pattern: string | ParamRef<string>): Condition<S, P> {
    return new Like(this, pattern, true);
  }

  /** `LIKE '%text%'`, `text` escaped. */
  contains<N extends string>(this: Expression<string | null, S, P>, text: ParamRef<N>): Condition<S, P & ParamValues<N, string>>;
  contains(this: Expression<string | null, S, P>, text: string): Condition<S, P>;
  contains(text: string | ParamRef<string>): Condition<S, P> {
    return new Like(this, pattern(text, (t) => `%${escapeLike(t)}%`), false);
  }

  icontains<N extends string>(this: Expression<string | null, S, P>, text: ParamRef<N>): Condition<S, P & ParamValues<N, string>>;
  icontains(this: Expression<string | null, S, P>, text: string): Condition<S, P>;
  icontains(text: string | ParamRef<string>): Condition<S, P> {
    return new Like(this, pattern(text, (t) => `%${escapeLike(t)}%`), true);
  }

  startsWith<N extends string>(this: Expression<string | null, S, P>, text: ParamRef<N>): Condition<S, P & ParamValues<N, string>>;
  startsWith(this: Expression<string | null, S, P>, text: string): Condition<S, P>;
  startsWith(text: string | ParamRef<string>): Condition<S, P> {
    return new Like(this, pattern(text, (t) => `${escapeLike(t)}%`), false);
  }

  endsWith<N extends string>(this: Expression<string | null, S, P>, text: ParamRef<N>): Condition<S, P & ParamValues<N, string>>;
  endsWith(this: Expression<string | null, S, P>, text: string): Condition<S, P>;
  endsWith(text: string | ParamRef<string>): Condition<S, P> {
    return new Like(this, pattern(text, (t) => `%${escapeLike(t)}`), false);
  }

  // Arrays -----------------------------------------------------------------------------------

  /** Array columns: `value` is one of the elements (`col @> ARRAY[value]`). */
  has<E, N extends string>(this: Expression<readonly E[] | null, S, P>, value: ParamRef<N>): Condition<S, P & ParamValues<N, In<E>>>;
  has<E>(this: Expression<readonly E[] | null, S, P>, value: In<E>): Condition<S, P>;
  has(value: unknown): Condition<S, P> {
    if (value instanceof ParamRef) {
      return new Comparison("contains", this, new Bound(value, (v) => [v]));
    }
    return new Comparison("contains", this, new Literal([value]));
  }

  /** Array columns: every one of `values` is an element (`col @> values`). */
  hasAll<E>(this: Expression<readonly E[] | null, S, P>, values: readonly In<E>[]): Condition<S, P> {
    return new Comparison("contains", this, new Literal([...values]));
  }

  /** Array columns: at least one of `values` is an element (`col && values`). */
  hasAny<E>(this: Expression<readonly E[] | null, S, P>, values: readonly In<E>[]): Condition<S, P> {
    return new Comparison("overlaps", this, new Literal([...values]));
  }

  /** Array columns: every element is one of `values` (`col <@ values`). */
  containedBy<E>(this: Expression<readonly E[] | null, S, P>, values: readonly In<E>[]): Condition<S, P> {
    return new Comparison("contained_by", this, new Literal([...values]));
  }

  /** Array columns: the element at SQL's 1-based `index` (`col[1]` is the first), `null`
   * out of range. PostgreSQL only. */
  element<E>(this: Expression<readonly E[] | null, S, P>, index: number): Func<E | null, S, P> {
    return new Func("element", [this, new Int(index)]);
  }

  // JSON -------------------------------------------------------------------------------------

  /** JSON columns: the value under each key or 0-based array index in turn
   * (`meta.get("tags", 0)` is `meta -> 'tags' -> 0`), `null` when absent. It compares as
   * JSON; `.asText()` reads it as text. PostgreSQL only. */
  get(this: Expression<JsonValue | null, S, P>, ...path: readonly (string | number)[]): JsonPath<S, P> {
    if (!path.length) {
      throw new TypeError("get() needs at least one key or index");
    }
    return this instanceof JsonPath ? this.extend(path) : new JsonPath(this, path, false);
  }

  /** JSON: the value contains `value` at the top level (`col @> value`). PostgreSQL only. */
  jsonContains(this: Expression<JsonValue | null, S, P>, value: JsonValue): Condition<S, P> {
    return new Comparison("contains", this, new Literal(value));
  }

  /** JSON: `value` contains the value (`col <@ value`). PostgreSQL only. */
  jsonContainedBy(this: Expression<JsonValue | null, S, P>, value: JsonValue): Condition<S, P> {
    return new Comparison("contained_by", this, new Literal(value));
  }

  /** JSON: the object has the top-level key `key`, or the array the string element
   * (`col ? key`). PostgreSQL only. */
  hasKey(this: Expression<JsonValue | null, S, P>, key: string): Condition<S, P> {
    if (typeof key !== "string") {
      throw new TypeError(`hasKey() takes a string, got ${typeof key}`);
    }
    return new Comparison("has_key", this, new Literal(key));
  }

  /** JSON: `col || value`, the objects merged (the keys of `value` win) or the arrays
   * joined: `update({ meta: Post.meta.jsonMerge({ seen: true }) })`. PostgreSQL only. */
  jsonMerge<S2 extends string = never, P2 = {}>(
    this: Expression<JsonValue | null, S, P>,
    value: JsonValue | Expression<JsonValue | null, S2, P2>,
  ): Expression<JsonValue, S | S2, P & P2> {
    return new Arith("json_merge", this, value instanceof Node ? value : new Literal(value)) as never;
  }

  // Full-text search ---------------------------------------------------------------------------

  /** `vector @@ query`: the document matches the search. A plain string is
   * `plainto_tsquery(<config of the vector>, query)`. PostgreSQL only. */
  matches<S2 extends string = never, P2 = {}>(this: Expression<TsVector, S, P>, query: string | Expression<TsQuery, S2, P2>): Condition<S | S2, P & P2> {
    if (typeof query === "string") {
      const config = this instanceof Func && this.name === "to_tsvector" && this.args.length === 2 ? this.args[0]! : undefined;
      query = new Func("plainto_tsquery", config === undefined ? [new Literal(query)] : [config, new Literal(query)]);
    }
    return new Comparison("match", this, query) as never;
  }

  // Strings ----------------------------------------------------------------------------------

  /** `this || other`: `null` when either side is `null`. {@link Functions.concat}
   * reads `null` as an empty string instead. */
  concat<U extends string | null, S2 extends string = never, P2 = {}>(this: Expression<U, S, P>, other: string | Expression<string, S2, P2>): Expression<string | Extract<U, null>, S | S2, P & P2>;
  concat<S2 extends string = never, P2 = {}>(this: Expression<string | null, S, P>, other: Operand<string, S2, P2>): Expression<string | null, S | S2, P & P2>;
  concat(other: unknown): unknown {
    return new Arith("concat", this, wrap(other));
  }

  // Arithmetic -------------------------------------------------------------------------------

  add<N extends string>(value: ParamRef<N>): Expression<T, S, P & ParamValues<N, In<NonNullable<T>>>>;
  add<S2 extends string = never, P2 = {}>(value: Operand<NonNullable<T>, S2, P2>): Expression<T, S | S2, P & P2>;
  add(value: unknown): unknown {
    return new Arith("add", this, wrap(value));
  }

  sub<N extends string>(value: ParamRef<N>): Expression<T, S, P & ParamValues<N, In<NonNullable<T>>>>;
  sub<S2 extends string = never, P2 = {}>(value: Operand<NonNullable<T>, S2, P2>): Expression<T, S | S2, P & P2>;
  sub(value: unknown): unknown {
    return new Arith("sub", this, wrap(value));
  }

  mul<N extends string>(value: ParamRef<N>): Expression<T, S, P & ParamValues<N, In<NonNullable<T>>>>;
  mul<S2 extends string = never, P2 = {}>(value: Operand<NonNullable<T>, S2, P2>): Expression<T, S | S2, P & P2>;
  mul(value: unknown): unknown {
    return new Arith("mul", this, wrap(value));
  }

  div<N extends string>(value: ParamRef<N>): Expression<T, S, P & ParamValues<N, In<NonNullable<T>>>>;
  div<S2 extends string = never, P2 = {}>(value: Operand<NonNullable<T>, S2, P2>): Expression<T, S | S2, P & P2>;
  div(value: unknown): unknown {
    return new Arith("div", this, wrap(value));
  }

  // Ordering ---------------------------------------------------------------------------------

  /** Ascending; `{ nulls: "first" | "last" }` places NULLs (the database's default otherwise). */
  asc(options?: OrderOptions): Ordering<S, P> {
    return new Ordering(this, false, options?.nulls);
  }

  /** Descending; `{ nulls: "first" | "last" }` places NULLs (the database's default otherwise). */
  desc(options?: OrderOptions): Ordering<S, P> {
    return new Ordering(this, true, options?.nulls);
  }
}

/** A boolean condition. Combine with `.and()`, `.or()`, `.not()` or `and()` / `or()` /
 * `not()`. */
export abstract class Condition<S extends string = never, P = {}> extends Expression<boolean, S, P> {
  and<S2 extends string = never, P2 = {}>(other: Expression<boolean | null, S2, P2>): Condition<S | S2, P & P2> {
    return and(this, other) as never;
  }

  or<S2 extends string = never, P2 = {}>(other: Expression<boolean | null, S2, P2>): Condition<S | S2, P & P2> {
    return or(this, other) as never;
  }

  not(): Condition<S, P> {
    return not(this);
  }
}

/** A query used as a value: one column (`in()`, `asScalar()`). */
export interface Subquery {
  subqueryIr(ctx: IRContext, what: string): IR;
}

/** A `select()` of one column whose values have type `T`. */
export interface SingleColumn<T, S extends string, P> extends Subquery {
  /** @internal */
  readonly "~column"?: [T, S, P];
}

function inValues(item: Expression<unknown, string, unknown>, values: unknown, neg: boolean): Condition<string, unknown> {
  if (values instanceof ParamRef) {
    throw new TypeError(`${neg ? "notIn" : "in"}() can't take a param(); the number of values is part of the query`);
  }
  if (!Array.isArray(values)) {
    return new InSelect(item, values as Subquery, neg);
  }
  if (!values.length) {
    return new Const(neg);
  }
  return new InList(item, values.map(wrap), neg);
}

function escapeLike(text: string): string {
  return text.replace(/[\\%_]/g, (c) => `\\${c}`);
}

function pattern(text: string | ParamRef<string>, make: (text: string) => string): string | Bound {
  if (text instanceof ParamRef) {
    return new Bound(text, (v) => make(String(v)));
  }
  if (typeof text !== "string") {
    throw new TypeError(`expected a string, got ${typeof text}`);
  }
  return make(text);
}

/** A plain value or an expression, as a node. */
export function wrap(value: unknown): Node {
  if (value instanceof Node) {
    return value;
  }
  if (value instanceof ParamRef) {
    return new Bound(value, undefined);
  }
  if (value instanceof RelationPath) {
    throw new TypeError(`${String(value)} is a relation, not a value; compare one of its columns`);
  }
  return new Literal(value);
}

export class Literal extends Expression<unknown, never, {}> {
  constructor(readonly value: unknown) {
    super();
    if (value === undefined) {
      throw new TypeError("undefined is not a value; use null for NULL");
    }
  }

  ir(ctx: IRContext): IR {
    return ctx.param(this.value);
  }
}

/**
 * `param("name")`: a value given each time a prepared query runs. Its type comes from
 * what it is compared with, so `prepare()`'s calls are typed:
 *
 * ```ts
 * const byAuthor = Post.objects.filter(Post.authorId.eq(param("author"))).limit(param("n")).prepare();
 * await byAuthor.all({ author: 3n, n: 10 });
 * ```
 */
export class ParamRef<N extends string> {
  constructor(readonly name: N) {
    if (!/^[A-Za-z_$][\w$]*$/.test(name)) {
      throw new TypeError(`param() takes an identifier, got ${JSON.stringify(name)}`);
    }
  }

  /** @internal */
  slot(params: unknown[], transform?: (value: unknown) => unknown): IR {
    if (!(params instanceof SlotParams)) {
      throw new QueryError(`param(${JSON.stringify(this.name)}) is a placeholder of a prepared query; call .prepare() on the query`);
    }
    params.push(new Slot(this.name, transform));
    return { t: "param", i: params.length - 1 };
  }

  toString(): string {
    return `param(${JSON.stringify(this.name)})`;
  }
}

export function param<N extends string>(name: N): ParamRef<N> {
  return new ParamRef(name);
}

/** A `param()` whose value goes through `transform` when bound (LIKE patterns, ...). */
class Bound extends Expression<unknown, never, {}> {
  constructor(
    readonly param: ParamRef<string>,
    readonly transform: ((value: unknown) => unknown) | undefined,
  ) {
    super();
  }

  ir(ctx: IRContext): IR {
    return this.param.slot(ctx.params, this.transform);
  }
}

/** A column of `root`'s model, or of a model reached from it through `path`. */
export class Column<T, S extends string = never> extends Expression<T, S, {}> {
  /** @internal */
  constructor(
    readonly root: Source,
    readonly path: readonly string[],
    readonly field: FieldMeta,
    readonly label: string,
  ) {
    super();
  }

  ir(ctx: IRContext): IR {
    if (this.root !== ctx.root) {
      let hint = `reach it through a relation of ${ctx.root.name} instead`;
      for (let c = ctx.outer; c; c = c.outer) {
        if (c.root === this.root) {
          hint = `use outer(${this.label}) for a column of the enclosing query`;
          break;
        }
      }
      throw new QueryError(`${this.label} belongs to ${this.root.name}, not to a ${ctx.root.name} query; ${hint}`);
    }
    return { t: "col", path: [...this.path], name: this.field.ir };
  }

  override toString(): string {
    return this.label;
  }
}

/** `excluded(Post.views)`: the value a conflicting insert proposed for a column, in
 * `insert(..., { onConflict, set: { views: Post.views.add(excluded(Post.views)) } })`. */
export class Excluded<T, S extends string> extends Expression<T, S, {}> {
  constructor(readonly column: Column<T, S>) {
    super();
    if (!(column instanceof Column) || column.path.length) {
      throw new TypeError(`excluded() takes a column of the inserted model, got ${String(column)}`);
    }
  }

  ir(ctx: IRContext): IR {
    this.column.ir(ctx); // checks the model
    return { t: "excluded", name: this.column.field.ir };
  }
}

export function excluded<T, S extends string>(column: Column<T, S>): Excluded<T, S> {
  return new Excluded(column);
}

/** `outer(User.id)`: a column of the enclosing `User` query, inside a subquery. */
export class Outer<T, S extends string> extends Expression<T, OuterOf<S>, {}> {
  constructor(readonly column: Column<T, S>) {
    super();
    if (!(column instanceof Column)) {
      throw new TypeError(`outer() takes a column of a model, got ${String(column)}`);
    }
  }

  ir(ctx: IRContext): IR {
    let depth = 1;
    for (let c = ctx.outer; c; c = c.outer, depth++) {
      if (c.root === this.column.root) {
        return { t: "outer", depth, path: [...this.column.path], name: this.column.field.ir };
      }
    }
    throw new QueryError(`outer(${this.column.label}) is not a column of an enclosing query`);
  }
}

/**
 * A column of the enclosing query, inside a subquery (Django's `OuterRef`):
 *
 * ```ts
 * await User.objects.filter(exists(Post.objects.filter(Post.authorId.eq(outer(User.id))))).all();
 * ```
 *
 * It refers to the nearest enclosing query over the column's model. A path of to-one
 * relations reads a related row: `outer(Post.author.name)`.
 */
export function outer<T, S extends string>(column: Column<T, S> & (string extends S ? unknown : Many extends S ? never : unknown)): Outer<T, S> {
  return new Outer(column);
}

/** `qs.select({ x }).asScalar()`: a one-column subquery used as a value. */
export class ScalarSubquery<T, S extends string, P> extends Expression<T, S, P> {
  constructor(readonly query: Subquery) {
    super();
  }

  ir(ctx: IRContext): IR {
    return { t: "subquery", select: this.query.subqueryIr(ctx, "asScalar()") };
  }
}

/** `EXISTS (<query>)`: true when the query has a row. */
class Exists extends Condition<any, any> {
  constructor(readonly query: Subquery) {
    super();
  }

  ir(ctx: IRContext): IR {
    return { t: "exists", select: this.query.subqueryIr(ctx, "exists()") };
  }
}

/** A query `exists()` takes: its outer references `X` become scopes of the condition. */
export interface ExistsQuery<X extends string, P> extends Subquery {
  /** @internal */
  readonly "~exists"?: [X, P];
}

/** `EXISTS (<query>)`, a condition: true when the query has a row. Correlate it with
 * {@link outer}; negate it with `.not()`. */
export function exists<X extends string, P>(query: ExistsQuery<X, P>): Condition<ResolveOuter<X>, P> {
  if (!query || typeof (query as Partial<Subquery>).subqueryIr !== "function") {
    throw new TypeError("exists() takes a query set or a select()");
  }
  return new Exists(query) as never;
}

// -- functions --------------------------------------------------------------------------------

/** A SQL function call; build it with {@link func}. */
export class Func<T, S extends string = never, P = {}> extends Expression<T, S, P> {
  constructor(
    readonly name: string,
    readonly args: readonly Node[] = [],
    readonly rel: RelationPath<ModelSpec, string, readonly Hop[]> | undefined = undefined,
    readonly distinct = false,
    filter: unknown = undefined,
    orderBy: unknown = undefined,
  ) {
    super();
    this.filter = filter === undefined ? undefined : asCondition(filter);
    this.order = orderings(orderBy);
  }

  /** @internal `ORDER BY` inside an `arrayAgg` call. */
  readonly order: readonly Ordering<string, unknown>[];

  /** @internal `FILTER (WHERE ...)` of an aggregate. */
  readonly filter: Node | undefined;

  ir(ctx: IRContext): IR {
    const ir: IR = { t: "func", name: this.name, args: this.args.map((a) => a.ir(ctx)) };
    if (this.rel) {
      if (this.rel[PATH].root !== ctx.root) {
        throw new QueryError(`${String(this.rel)} does not start at ${ctx.root.name}`);
      }
      ir["rel"] = [...this.rel[PATH].path];
    }
    if (this.distinct) {
      ir["distinct"] = true;
    }
    if (this.filter) {
      ir["filter"] = this.filter.ir(ctx);
    }
    if (this.order.length) {
      ir["order_by"] = this.order.map((o) => o.ir(ctx));
    }
    return ir;
  }

  /**
   * `<function> OVER (...)`: the function over a window of rows related to each row,
   * without grouping them.
   *
   * ```ts
   * func.rowNumber().over({ partitionBy: Post.authorId, orderBy: Post.views.desc() })
   * func.sum(Post.views).over({ orderBy: Post.createdAt, rows: [null, 0] })  // running total
   * const w = window({ partitionBy: Post.authorId, orderBy: Post.createdAt });   // shared
   * func.sum(Post.views).over(w)
   * ```
   *
   * Frames are `[start, end]`: `null` unbounded, `0` the current row, `-n` n preceding,
   * `n` n following. Over a {@link window}, `orderBy` and `rows` / `range` extend it
   * when it has none of its own; its partitioning is fixed.
   */
  over<S2 extends string = never, P2 = {}>(spec?: OverSpec<S2, P2>): Window<T, S | Exclude<S2, Many>, P & P2>;
  over<S2 extends string, P2, S3 extends string = never, P3 = {}>(
    window: WindowDef<S2, P2>,
    extend?: WindowExtend<S3, P3>,
  ): Window<T, S | S2 | S3, P & P2 & P3>;
  over(a?: unknown, b?: unknown): Window<T, string, unknown> {
    if (a instanceof WindowDef) {
      const ext = (b ?? {}) as WindowExtend<string, unknown>;
      const frame = frameIr(ext);
      if (ext.orderBy !== undefined && a.order.length) {
        throw new QueryError("over(window, { orderBy }) needs a window without its own orderBy");
      }
      if (frame && a.frame) {
        throw new QueryError("over(window, { rows / range }) needs a window without its own frame");
      }
      return new Window(this, a, [], orderings(ext.orderBy), frame);
    }
    const spec = (a ?? {}) as OverSpec<string, unknown>;
    return new Window(this, undefined, many(spec.partitionBy), orderings(spec.orderBy), frameIr(spec));
  }
}

/** `Post.meta.get("a", "b")`: a `jsonb` value inside a JSON column. */
export class JsonPath<S extends string = never, P = {}> extends Expression<JsonValue | null, S, P> {
  constructor(
    readonly item: Node,
    readonly path: readonly (string | number)[],
    readonly text: boolean,
  ) {
    super();
    for (const key of path) {
      if (typeof key !== "string" && !Number.isSafeInteger(key)) {
        throw new TypeError(`JSON path steps are string keys or integer indexes, got ${String(key)}`);
      }
    }
  }

  /** @internal */
  extend(path: readonly (string | number)[]): JsonPath<S, P> {
    if (this.text) {
      throw new TypeError("asText() ends a JSON path");
    }
    return new JsonPath(this.item, [...this.path, ...path], false);
  }

  /** The value as text (the last step is `->>`): a JSON string without quotes, so `like`,
   * `contains` and string functions work on it. */
  asText(): Expression<string | null, S, P> {
    return new JsonPath(this.item, this.path, true) as never;
  }

  ir(ctx: IRContext): IR {
    const ir: IR = { t: "json_path", item: this.item.ir(ctx), path: [...this.path] };
    if (this.text) {
      ir["text"] = true;
    }
    return ir;
  }
}

/** `func.case([cond, value], ..., { default })`: `CASE WHEN ... END`. */
export class Case<T, S extends string = never, P = {}> extends Expression<T, S, P> {
  readonly whens: readonly (readonly [Node, Node])[];
  readonly fallback: Node | undefined;

  constructor(branches: readonly unknown[], fallback: unknown) {
    super();
    if (!branches.length) {
      throw new TypeError("case() needs at least one [condition, value] branch");
    }
    this.whens = branches.map((b) => {
      if (!Array.isArray(b) || b.length !== 2) {
        throw new TypeError(`case() branches are [condition, value] pairs, got ${String(b)}`);
      }
      return [asCondition(b[0]), wrap(b[1])] as const;
    });
    this.fallback = fallback === undefined ? undefined : wrap(fallback);
  }

  ir(ctx: IRContext): IR {
    const ir: IR = { t: "case", whens: this.whens.map(([c, v]) => ({ cond: c.ir(ctx), value: v.ir(ctx) })) };
    if (this.fallback !== undefined) {
      ir["default"] = this.fallback.ir(ctx);
    }
    return ir;
  }
}

/** One `[condition, value]` branch of {@link Functions.case}. The condition is `unknown`
 * here so it gives no contextual type (that would widen its scope to `string`);
 * {@link CaseOf} checks it. */
export type CaseBranch = readonly [unknown, unknown];
/** The `Case` of these branches, or `never` when a condition is not a boolean expression. */
type CaseOf<Br extends CaseBranch, T, S extends string, P> = [Exclude<Br[0], Expression<boolean | null, string, unknown>>] extends [never]
  ? Case<T, S, P>
  : never;
/** The options of {@link Functions.case}: the value when no condition holds. */
export type CaseDefault = { readonly default: unknown };
/** The value type of a branch value: an expression's type, or a plain value's widened type. */
type CaseValue<V> = V extends Expression<infer T, string, unknown>
  ? T
  : V extends number
    ? number
    : V extends string
      ? string
      : V extends boolean
        ? boolean
        : V;
type ScopeOfNode<V> = V extends Expression<unknown, infer S, unknown> ? S : never;
type ParamsOfNode<V> = V extends Expression<unknown, string, infer P> ? P : {};
type CaseScope<B extends CaseBranch> = B extends unknown ? ScopeOfNode<B[0]> | ScopeOfNode<B[1]> : never;
type CaseParams<B extends CaseBranch> = UnionToIntersection<B extends unknown ? ParamsOfNode<B[0]> & ParamsOfNode<B[1]> : never>;

/** A window-only function (`rowNumber()`, `lag()`, ...): usable only with `.over()`. */
export class WindowFunc<T, S extends string = never, P = {}> {
  /** @internal */
  readonly func: Func<T, S, P>;

  constructor(name: string, args: readonly Node[] = []) {
    this.func = new Func(name, args);
  }

  over<S2 extends string = never, P2 = {}>(spec?: OverSpec<S2, P2>): Window<T, S | Exclude<S2, Many>, P & P2>;
  over<S2 extends string, P2, S3 extends string = never, P3 = {}>(
    window: WindowDef<S2, P2>,
    extend?: WindowExtend<S3, P3>,
  ): Window<T, S | S2 | S3, P & P2 & P3>;
  over(a?: unknown, b?: unknown): unknown {
    return (this.func.over as (a?: unknown, b?: unknown) => unknown).call(this.func, a, b);
  }
}

type Frame = readonly [number | null, number | null];
type OrderItems<S extends string, P> = Expression<unknown, S, P> | Ordering<S, P> | readonly (Expression<unknown, S, P> | Ordering<S, P>)[];

export interface WindowExtend<S extends string, P> {
  readonly orderBy?: OrderItems<S, P>;
  readonly rows?: Frame;
  readonly range?: Frame;
}

export interface OverSpec<S extends string, P> extends WindowExtend<S, P> {
  readonly partitionBy?: Expression<unknown, S, P> | readonly Expression<unknown, S, P>[];
}

function frameIr(spec: { readonly rows?: Frame; readonly range?: Frame }): IR | undefined {
  if (spec.rows && spec.range) {
    throw new QueryError("over() takes rows or range, not both");
  }
  const kind = spec.rows ? "rows" : spec.range ? "range" : undefined;
  if (!kind) {
    return undefined;
  }
  const [start, end] = (spec.rows ?? spec.range)!;
  for (const b of [start, end]) {
    if (b !== null && !Number.isInteger(b)) {
      throw new TypeError(`frame bounds are integers or null, got ${String(b)}`);
    }
  }
  return { kind, start, end };
}

function many<T>(items: T | readonly T[] | undefined): T[] {
  return items === undefined ? [] : Array.isArray(items) ? [...(items as readonly T[])] : [items as T];
}

/** Orderings from expressions (ascending) and orderings. */
export function orderings(items: unknown): Ordering<string, unknown>[] {
  return many(items).map((i) => {
    if (i instanceof Ordering) {
      return i;
    }
    if (i instanceof Expression) {
      return new Ordering(i, false);
    }
    throw new TypeError(`expected a column, an expression or an ordering, got ${String(i)}`);
  });
}

/**
 * A window definition several window functions of a query share: `WINDOW w1 AS (...)`.
 * Build it with {@link window}, use it with `func.sum(x).over(w)`.
 */
export class WindowDef<S extends string, P> {
  /** @internal */
  declare readonly "~types"?: [S, P];

  constructor(
    readonly partition: Node[],
    readonly order: Ordering<string, unknown>[],
    readonly frame: IR | undefined,
  ) {}

  /** @internal */
  specIr(ctx: IRContext): IR {
    const ir: IR = {};
    if (this.partition.length) {
      ir["partition_by"] = this.partition.map((p) => p.ir(ctx));
    }
    if (this.order.length) {
      ir["order_by"] = this.order.map((o) => o.ir(ctx));
    }
    if (this.frame) {
      ir["frame"] = this.frame;
    }
    return ir;
  }
}

/** A named window (`WINDOW w AS (PARTITION BY ... ORDER BY ...)`) for several window
 * functions to share. */
export function window<S extends string = never, P = {}>(spec: OverSpec<S, P>): WindowDef<Exclude<S, Many>, P> {
  return new WindowDef(many(spec.partitionBy), orderings(spec.orderBy), frameIr(spec));
}

/** `func.<name>(...).over(...)`: a window function call. */
export class Window<T, S extends string, P> extends Expression<T, S, P> {
  constructor(
    readonly func: Func<unknown, string, unknown>,
    readonly base: WindowDef<string, unknown> | undefined,
    readonly partition: Node[],
    readonly order: Ordering<string, unknown>[],
    readonly frame: IR | undefined,
  ) {
    super();
  }

  ir(ctx: IRContext): IR {
    const ir: IR = { t: "window", func: this.func.ir(ctx) };
    if (this.base) {
      ir["base"] = ctx.windowName(this.base as WindowDef<string, object>);
    }
    if (this.partition.length) {
      ir["partition_by"] = this.partition.map((p) => p.ir(ctx));
    }
    if (this.order.length) {
      ir["order_by"] = this.order.map((o) => o.ir(ctx));
    }
    if (this.frame) {
      ir["frame"] = this.frame;
    }
    return ir;
  }
}

/** An integer written into the SQL text (window function arguments). */
class Int extends Expression<number, never, {}> {
  constructor(readonly value: number) {
    super();
    if (!Number.isSafeInteger(value)) {
      throw new TypeError(`expected an integer, got ${String(value)}`);
    }
  }

  ir(): IR {
    return { t: "int", value: this.value };
  }
}

type Num = number | bigint | Decimal;
/** The type of a `tsvector` expression (`func.toTsvector`): only for typing. */
export interface TsVector {
  readonly "~tsvector": true;
}
/** The type of a `tsquery` expression (`func.toTsquery`, ...): only for typing. */
export interface TsQuery {
  readonly "~tsquery": true;
}

/** A text search configuration (`'english'::regconfig`), written into the SQL so the
 * expression matches an expression index. */
class Config extends Expression<never, never, {}> {
  constructor(readonly value: string) {
    super();
    if (typeof value !== "string") {
      throw new TypeError(`a text search configuration is a name, got ${typeof value}`);
    }
  }

  ir(): IR {
    return { t: "text", value: this.value };
  }
}

type SearchArgs<S extends string, P> = [text: string | Expression<string | null, S, P>] | [config: string, text: string | Expression<string | null, S, P>];
function search(args: readonly unknown[]): Node[] {
  return args.length === 2 ? [new Config(args[0] as string), wrap(args[1])] : [wrap(args[0])];
}
/** Options of an aggregate: `DISTINCT`, and `FILTER (WHERE filter)` (only the rows
 * where `filter` holds). */
export interface AggregateOptions<S extends string, P> {
  readonly distinct?: boolean;
  readonly filter?: Expression<boolean | null, S, P>;
}
/** Options of `arrayAgg`: {@link AggregateOptions} and the order of the elements. */
export interface ArrayAggOptions<S extends string, P, S2 extends string, P2> extends AggregateOptions<S, P> {
  readonly orderBy?: Expression<unknown, S2, P2> | Ordering<S2, P2> | readonly (Expression<unknown, S2, P2> | Ordering<S2, P2>)[];
}
type AnyExpr<T, S extends string, P> = Expression<T, S, P>;
/** `SUM` of integers is a `bigint` (cast so); of floats a number; of decimals a Decimal. */
type SumOf<T> = [NonNullable<T>] extends [number | bigint] ? (number extends NonNullable<T> ? number | bigint : bigint) : NonNullable<T>;

/**
 * `func.count(...)`, `func.sum(...)`, ...: SQL functions as expressions.
 *
 * An aggregate over a relation path is computed per row of the query in a correlated
 * subquery: `func.count(User.posts)` is each user's number of posts. Over the model's
 * own columns, aggregates summarize the rows of each `groupBy()` group (or all rows).
 */
class Functions {
  /** `COUNT(*)` without argument, `COUNT(expr)` (non-null values), or the rows of a
   * relation: `func.count(User.posts)`. */
  count(): Func<bigint>;
  count<S2 extends string, P2>(options: AggregateOptions<S2, P2>): Func<bigint, Exclude<S2, Many>, P2>;
  count<S extends string, P, S2 extends string = never, P2 = {}>(
    what: Expression<unknown, S, P> | RelationPath<ModelSpec, S, readonly Hop[]>,
    options?: AggregateOptions<S2, P2>,
  ): Func<bigint, Exclude<S | S2, Many>, P & P2>;
  count(what?: unknown, options?: AggregateOptions<string, unknown>): Func<bigint, string, unknown> {
    if (what !== undefined && !(what instanceof Node) && !(what instanceof RelationPath)) {
      return this.count(undefined as never, what as AggregateOptions<string, unknown>) as never;
    }
    if (what instanceof RelationPath) {
      return new Func("count", [], what, false, options?.filter);
    }
    return new Func("count", what === undefined ? [] : [wrap(what)], undefined, options?.distinct ?? false, options?.filter) as never;
  }

  /** `SUM`; integer sums come back as `bigint` (cast to bigint). */
  sum<T extends Num | null, S extends string, P, S2 extends string = never, P2 = {}>(
    expr: AnyExpr<T, S, P>,
    options?: AggregateOptions<S2, P2>,
  ): Func<SumOf<T> | null, Exclude<S | S2, Many>, P & P2> {
    return new Func("sum", [expr], undefined, options?.distinct ?? false, options?.filter);
  }

  /** `AVG`: a number, or a `Decimal` for decimal columns (exact). */
  avg<T extends Num | null, S extends string, P, S2 extends string = never, P2 = {}>(
    expr: AnyExpr<T, S, P>,
    options?: AggregateOptions<S2, P2>,
  ): Func<([NonNullable<T>] extends [Decimal] ? Decimal : number) | null, Exclude<S | S2, Many>, P & P2> {
    return new Func("avg", [expr], undefined, options?.distinct ?? false, options?.filter);
  }

  /**
   * `ARRAY_AGG(expr [ORDER BY ...])`: the values as an array, `null` values included.
   * `null` (not `[]`) over no rows. `orderBy` fixes the order of the elements; with
   * `distinct` it must use the same expression. Not for array columns; PostgreSQL only.
   */
  arrayAgg<T, S extends string, P, S2 extends string = never, P2 = {}, S3 extends string = never, P3 = {}>(
    expr: AnyExpr<T, S, P>,
    options?: ArrayAggOptions<S2, P2, S3, P3>,
  ): Func<T[] | null, Exclude<S | S2 | S3, Many>, P & P2 & P3> {
    return new Func("array_agg", [expr], undefined, options?.distinct ?? false, options?.filter, options?.orderBy);
  }

  min<T, S extends string, P, S2 extends string = never, P2 = {}>(
    expr: AnyExpr<T, S, P>,
    options?: { readonly filter?: Expression<boolean | null, S2, P2> },
  ): Func<T | null, Exclude<S | S2, Many>, P & P2> {
    return new Func("min", [expr], undefined, false, options?.filter);
  }

  max<T, S extends string, P, S2 extends string = never, P2 = {}>(
    expr: AnyExpr<T, S, P>,
    options?: { readonly filter?: Expression<boolean | null, S2, P2> },
  ): Func<T | null, Exclude<S | S2, Many>, P & P2> {
    return new Func("max", [expr], undefined, false, options?.filter);
  }

  lower<T extends string | null, S extends string, P>(expr: AnyExpr<T, S, P>): Func<T, S, P> {
    return new Func("lower", [expr]);
  }

  upper<T extends string | null, S extends string, P>(expr: AnyExpr<T, S, P>): Func<T, S, P> {
    return new Func("upper", [expr]);
  }

  length<T extends string | null, S extends string, P>(expr: AnyExpr<T, S, P>): Func<number | Extract<T, null>, S, P> {
    return new Func("length", [expr]);
  }

  /** The number of elements of an array. */
  cardinality<T extends readonly unknown[] | null, S extends string, P>(expr: AnyExpr<T, S, P>): Func<number | Extract<T, null>, S, P> {
    return new Func("cardinality", [expr]);
  }

  /** One row for each element of an array. Only a `select()` column; PostgreSQL only. */
  unnest<E, S extends string, P>(expr: AnyExpr<readonly E[] | null, S, P>): Func<E, S, P> {
    return new Func("unnest", [expr]);
  }

  /** `CONCAT(...)`: the parts as text, a `null` part as an empty string.
   * `a.concat(b)` (`a || b`) is `null` when either side is `null`. */
  concat<S extends string = never, P = {}>(...parts: readonly (string | Expression<unknown, S, P>)[]): Func<string, S, P> {
    if (parts.length === 0) {
      throw new TypeError("concat() needs at least one part");
    }
    return new Func("concat", parts.map((p) => wrap(p)));
  }

  /** Without leading and trailing spaces. */
  trim<T extends string | null, S extends string, P>(expr: AnyExpr<T, S, P>): Func<T, S, P> {
    return new Func("trim", [expr]);
  }

  /** Without leading spaces. */
  ltrim<T extends string | null, S extends string, P>(expr: AnyExpr<T, S, P>): Func<T, S, P> {
    return new Func("ltrim", [expr]);
  }

  /** Without trailing spaces. */
  rtrim<T extends string | null, S extends string, P>(expr: AnyExpr<T, S, P>): Func<T, S, P> {
    return new Func("rtrim", [expr]);
  }

  /** Every `old` in `expr` replaced by `replacement`. */
  replace<T extends string | null, S extends string, P, S2 extends string = never, P2 = {}, S3 extends string = never, P3 = {}>(
    expr: AnyExpr<T, S, P>,
    old: Operand<string, S2, P2>,
    replacement: Operand<string, S3, P3>,
  ): Func<T, S | S2 | S3, P & P2 & P3> {
    return new Func("replace", [expr, wrap(old), wrap(replacement)]);
  }

  /** The characters from the 1-based `start` (at least 1), `length` of them (at least 0; default: all). */
  substr<T extends string | null, S extends string, P>(expr: AnyExpr<T, S, P>, start: number, length?: number): Func<T, S, P> {
    return new Func("substr", length === undefined ? [expr, new Int(start)] : [expr, new Int(start), new Int(length)]);
  }

  /** The 1-based position of the first `part` in `expr`, 0 if absent (`STRPOS`; `INSTR`
   * on SQLite). */
  strpos<T extends string | null, S extends string, P, S2 extends string = never, P2 = {}>(
    expr: AnyExpr<T, S, P>,
    part: Operand<string, S2, P2>,
  ): Func<number | Extract<T, null>, S | S2, P & P2> {
    return new Func("strpos", [expr, wrap(part)]);
  }

  abs<T extends Num | null, S extends string, P>(expr: AnyExpr<T, S, P>): Func<T, S, P> {
    return new Func("abs", [expr]);
  }

  /** The first non-null of `expr` and `fallback`. */
  coalesce<T, S extends string, P, S2 extends string = never, P2 = {}>(
    expr: AnyExpr<T, S, P>,
    fallback: Operand<NonNullable<T>, S2, P2>,
  ): Func<NonNullable<T> | Extract<typeof fallback, null>, S | S2, P & P2> {
    return new Func("coalesce", [expr, wrap(fallback)]);
  }

  now(): Func<Date> {
    return new Func("now");
  }

  // Full-text search, PostgreSQL only. An optional first argument names the text search
  // configuration ("english"); without it the server's default applies.

  /** `to_tsvector([config,] document)`: the document's normalized words. Index it with
   * `@@index([sql("to_tsvector('english', title)")], type: Gin)`. */
  toTsvector<S extends string = never, P = {}>(...args: SearchArgs<S, P>): Func<TsVector, S, P> {
    return new Func("to_tsvector", search(args));
  }

  /** `to_tsquery([config,] query)`: a query in tsquery syntax (`"cat & !dog"`). */
  toTsquery<S extends string = never, P = {}>(...args: SearchArgs<S, P>): Func<TsQuery, S, P> {
    return new Func("to_tsquery", search(args));
  }

  /** `plainto_tsquery([config,] text)`: every word of plain text. */
  plaintoTsquery<S extends string = never, P = {}>(...args: SearchArgs<S, P>): Func<TsQuery, S, P> {
    return new Func("plainto_tsquery", search(args));
  }

  /** `websearch_to_tsquery([config,] text)`: search-engine syntax (`"cat -dog"`, `or`,
   * quoted phrases). */
  websearchToTsquery<S extends string = never, P = {}>(...args: SearchArgs<S, P>): Func<TsQuery, S, P> {
    return new Func("websearch_to_tsquery", search(args));
  }

  /** `ts_rank(vector, query)`: how well the document matches, for `orderBy`. */
  tsRank<S extends string, P, S2 extends string = never, P2 = {}>(vector: Expression<TsVector, S, P>, query: Expression<TsQuery, S2, P2>): Func<number, S | S2, P & P2> {
    return new Func("ts_rank", [vector, query]);
  }

  /**
   * `CASE WHEN cond THEN value ... ELSE default END`: the value of the first true
   * condition, else `default` (`null` when not given).
   *
   * ```ts
   * func.case([Post.views.gt(100), "hot"], [Post.views.gt(10), "warm"], { default: "cold" })
   * func.sum(func.case([Post.published, 1], { default: 0 }))
   * ```
   */
  case<const B extends readonly CaseBranch[]>(...branches: B): CaseOf<B[number], CaseValue<B[number][1]> | null, CaseScope<B[number]>, CaseParams<B[number]>>;
  case<const A extends readonly [...CaseBranch[], CaseDefault]>(
    ...args: A
  ): CaseOf<
    Extract<A[number], CaseBranch>,
    CaseValue<Extract<A[number], CaseBranch>[1]> | CaseValue<Extract<A[number], CaseDefault>["default"]>,
    CaseScope<Extract<A[number], CaseBranch>> | ScopeOfNode<Extract<A[number], CaseDefault>["default"]>,
    CaseParams<Extract<A[number], CaseBranch>> & ParamsOfNode<Extract<A[number], CaseDefault>["default"]>
  >;
  case(...args: unknown[]): Case<unknown, string, unknown> {
    const last = args[args.length - 1];
    if (last !== undefined && !Array.isArray(last) && !(last instanceof Node) && typeof last === "object" && last !== null) {
      return new Case(args.slice(0, -1), (last as { default?: unknown }).default);
    }
    return new Case(args, undefined);
  }

  // Window functions: only valid with .over(...).

  /** 1, 2, 3, ... in the window's order. */
  rowNumber(): WindowFunc<bigint> {
    return new WindowFunc("row_number");
  }

  /** Rank with gaps: ties share a rank, the next rank skips (1, 1, 3). */
  rank(): WindowFunc<bigint> {
    return new WindowFunc("rank");
  }

  /** Rank without gaps (1, 1, 2). */
  denseRank(): WindowFunc<bigint> {
    return new WindowFunc("dense_rank");
  }

  percentRank(): WindowFunc<number> {
    return new WindowFunc("percent_rank");
  }

  cumeDist(): WindowFunc<number> {
    return new WindowFunc("cume_dist");
  }

  /** The bucket (1..buckets) of the row when the window is split evenly. */
  ntile(buckets: number): WindowFunc<number> {
    return new WindowFunc("ntile", [new Int(buckets)]);
  }

  /** `expr` on the row `offset` rows before this one, `fallback` if none. */
  lag<T, S extends string, P>(expr: AnyExpr<T, S, P>, offset = 1, fallback?: In<NonNullable<T>>): WindowFunc<T | null, S, P> {
    return new WindowFunc("lag", fallback === undefined ? [expr, new Int(offset)] : [expr, new Int(offset), wrap(fallback)]);
  }

  /** `expr` on the row `offset` rows after this one, `fallback` if none. */
  lead<T, S extends string, P>(expr: AnyExpr<T, S, P>, offset = 1, fallback?: In<NonNullable<T>>): WindowFunc<T | null, S, P> {
    return new WindowFunc("lead", fallback === undefined ? [expr, new Int(offset)] : [expr, new Int(offset), wrap(fallback)]);
  }

  firstValue<T, S extends string, P>(expr: AnyExpr<T, S, P>): WindowFunc<T, S, P> {
    return new WindowFunc("first_value", [expr]);
  }

  lastValue<T, S extends string, P>(expr: AnyExpr<T, S, P>): WindowFunc<T, S, P> {
    return new WindowFunc("last_value", [expr]);
  }

  nthValue<T, S extends string, P>(expr: AnyExpr<T, S, P>, n: number): WindowFunc<T | null, S, P> {
    return new WindowFunc("nth_value", [expr, new Int(n)]);
  }
}

export const func = new Functions();

// -- relation paths ---------------------------------------------------------------------------

/**
 * `User.posts`: a relation reached from a root model. Its properties continue the path:
 * columns of the related model are {@link Column}s, its relations longer paths (the
 * generated `PostPath` types list them). Used in filters, `selectRelated`,
 * `prefetchRelated` and `func.count(User.posts)`.
 */
/** Where a relation path keeps its state (a symbol: path objects' string keys are the
 * related model's columns and relations). */
export const PATH: unique symbol = Symbol("orm.path");

export interface PathState {
  readonly root: Source;
  /** IR relation names from the root. */
  readonly path: readonly string[];
  /** TypeScript relation names from the root. */
  readonly names: readonly string[];
  readonly target: string;
  /** A path that ends in a `belongsTo` relation: its foreign key column, and how to read
   * the referenced key of a target instance. */
  readonly belongsTo?: { readonly key: Column<unknown, string>; readonly read: (o: object) => unknown };
}

export class RelationPath<M extends ModelSpec, S extends string, H extends readonly Hop[]> {
  /** @internal Phantom: the target model, the root's scope and the hops. */
  declare readonly "~path"?: [M, S, H];
  /** @internal */
  declare readonly [PATH]: PathState;

  /**
   * `Post.author.eq(alice)` is `Post.authorId.eq(alice.id)`: it compares the foreign key
   * column, so the related table is not joined. `.eq(null)` is `IS NULL`. Only a
   * `belongsTo` relation compares with an instance.
   */
  eq(this: RelationPath<M, S, ToOne>, value: M["row"] | null): Condition<S, {}> {
    return compareRelation(this, value, false);
  }

  /** `Post.author.ne(alice)` is `Post.authorId.ne(alice.id)`; `.ne(null)` is `IS NOT NULL`. */
  ne(this: RelationPath<M, S, ToOne>, value: M["row"] | null): Condition<S, {}> {
    return compareRelation(this, value, true);
  }

  toString(): string {
    const s = this[PATH];
    return [s.root.name, ...s.names].join(".");
  }
}

/** Hops that end in a to-one relation. */
type ToOne = readonly [...Hop[], Hop<string, "one" | "opt">];

function compareRelation(path: RelationPath<ModelSpec, string, readonly Hop[]>, value: unknown, neg: boolean): Condition<any, any> {
  const s = path[PATH];
  if (!s.belongsTo) {
    throw new TypeError(`${String(path)} is not a belongsTo relation; only a belongsTo relation compares with an instance (compare a key column instead)`);
  }
  if (value === null) return new IsNull(s.belongsTo.key, neg);
  const key = s.belongsTo.read(value as object);
  if (key === null || key === undefined) {
    throw new TypeError(`${String(path)} can't compare with an instance whose key is ${String(key)}`);
  }
  return new Comparison(neg ? "ne" : "eq", s.belongsTo.key, wrap(key));
}

/** Where NULLs go in an ordering. */
export interface OrderOptions {
  readonly nulls?: "first" | "last" | undefined;
}

export class Ordering<S extends string = never, P = {}> {
  /** @internal */
  declare readonly "~types"?: [S, P];

  constructor(
    readonly expr: Node,
    readonly descending: boolean,
    readonly nulls?: "first" | "last",
  ) {
    if (nulls !== undefined && nulls !== "first" && nulls !== "last") {
      throw new TypeError(`nulls is "first" or "last", not ${JSON.stringify(nulls)}`);
    }
  }

  /** The opposite order, NULLs included. */
  reversed(): Ordering<S, P> {
    return new Ordering(this.expr, !this.descending, this.nulls === undefined ? undefined : this.nulls === "first" ? "last" : "first");
  }

  /** @internal */
  ir(ctx: IRContext): IR {
    const ir: IR = { expr: this.expr.ir(ctx), desc: this.descending };
    if (this.nulls !== undefined) ir["nulls"] = this.nulls;
    return ir;
  }
}

// -- conditions ---------------------------------------------------------------------------------

class Comparison extends Condition<any, any> {
  constructor(
    readonly op: string,
    readonly left: Node,
    readonly right: Node,
  ) {
    super();
  }

  ir(ctx: IRContext): IR {
    return { t: "cmp", op: this.op, l: this.left.ir(ctx), r: this.right.ir(ctx) };
  }
}

class Arith extends Expression<any, any, any> {
  constructor(
    readonly op: string,
    readonly left: Node,
    readonly right: Node,
  ) {
    super();
  }

  ir(ctx: IRContext): IR {
    return { t: "arith", op: this.op, l: this.left.ir(ctx), r: this.right.ir(ctx) };
  }
}

class InSelect extends Condition<any, any> {
  constructor(
    readonly item: Node,
    readonly query: Subquery,
    readonly neg: boolean,
  ) {
    super();
  }

  ir(ctx: IRContext): IR {
    return { t: "in_select", item: this.item.ir(ctx), select: this.query.subqueryIr(ctx, "in()"), neg: this.neg };
  }
}

class InList extends Condition<any, any> {
  constructor(
    readonly item: Node,
    readonly values: Node[],
    readonly neg: boolean,
  ) {
    super();
  }

  ir(ctx: IRContext): IR {
    return { t: "in", item: this.item.ir(ctx), values: this.values.map((v) => v.ir(ctx)), neg: this.neg };
  }
}

class IsNull extends Condition<any, any> {
  constructor(
    readonly item: Node,
    readonly neg: boolean,
  ) {
    super();
  }

  ir(ctx: IRContext): IR {
    return { t: "is_null", item: this.item.ir(ctx), neg: this.neg };
  }
}

class Like extends Condition<any, any> {
  constructor(
    readonly item: Node,
    readonly pattern: string | Bound | ParamRef<string>,
    readonly ci: boolean,
  ) {
    super();
  }

  ir(ctx: IRContext): IR {
    const p = this.pattern;
    const pattern = p instanceof Node ? p.ir(ctx) : p instanceof ParamRef ? p.slot(ctx.params) : ctx.param(p);
    return { t: "like", item: this.item.ir(ctx), pattern, ci: this.ci, neg: false };
  }
}

class Const extends Condition<never, {}> {
  constructor(readonly value: boolean) {
    super();
  }

  ir(): IR {
    return { t: "const", value: this.value };
  }
}

class BoolOp extends Condition<any, any> {
  constructor(
    readonly op: "and" | "or",
    readonly items: Node[],
  ) {
    super();
  }

  ir(ctx: IRContext): IR {
    return { t: this.op, items: this.items.map((i) => i.ir(ctx)) };
  }
}

class Not extends Condition<any, any> {
  constructor(readonly item: Node) {
    super();
  }

  ir(ctx: IRContext): IR {
    return { t: "not", item: this.item.ir(ctx) };
  }
}

/** A condition from a boolean expression (`Post.published` as `published = TRUE`). */
export function asCondition(value: unknown): Node {
  if (value instanceof Condition) {
    return value;
  }
  if (value instanceof Expression) {
    return new Comparison("eq", value, new Literal(true));
  }
  throw new TypeError(`expected a condition such as User.email.eq("a@b.c"), got ${String(value)}`);
}

type CondArgs = readonly Expression<boolean | null, string, unknown>[];
type ScopeOf<C extends CondArgs> = C[number] extends Expression<unknown, infer S, unknown> ? S : never;
/** The intersection of the params of several expressions. */
export type ParamsOf<C extends readonly unknown[]> = UnionToIntersection<
  { [K in keyof C]: C[K] extends Expression<unknown, string, infer P> ? P : {} }[number]
>;
type UnionToIntersection<U> = (U extends unknown ? (x: U) => void : never) extends (x: infer I) => void ? I : never;

function combine(op: "and" | "or", items: readonly unknown[]): Node {
  const flat: Node[] = [];
  for (const item of items.map(asCondition)) {
    if (item instanceof BoolOp && item.op === op) {
      flat.push(...item.items);
    } else {
      flat.push(item);
    }
  }
  return flat.length === 1 ? flat[0]! : new BoolOp(op, flat);
}

/** All of `conditions`; `and()` is true. */
export function and<const C extends CondArgs>(...conditions: C): Condition<ScopeOf<C>, ParamsOf<C>> {
  return (conditions.length ? combine("and", conditions) : new Const(true)) as never;
}

/** Any of `conditions`; `or()` is false. */
export function or<const C extends CondArgs>(...conditions: C): Condition<ScopeOf<C>, ParamsOf<C>> {
  return (conditions.length ? combine("or", conditions) : new Const(false)) as never;
}

export function not<S extends string, P>(condition: Expression<boolean | null, S, P>): Condition<S, P> {
  return new Not(asCondition(condition)) as never;
}

/** `Decimal` instances are values, not expressions; re-exported for `instanceof`. */
export { Decimal };
