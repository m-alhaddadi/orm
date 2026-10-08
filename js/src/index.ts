/**
 * TypeScript ORM with a Rust core: the same schema, engine and query semantics as the
 * Python package.
 *
 * Models come from a schema file (`schema.prisma`) through a generated module
 * (`orm generate typescript`), which types everything:
 *
 * ```ts
 * import { connect } from "orm";
 * import { User } from "./models.js";
 *
 * await connect("postgres://...");
 * const users = await User.objects.filter(User.posts.createdAt.lt(yesterday));
 * ```
 */

export { Decimal } from "./decimal.js";
export type { JsonValue, In, ModelSpec, Hop, HopKind, ColType, FieldMeta, RelationMeta } from "./meta.js";
export { camel } from "./meta.js";
export {
  Column,
  Condition,
  Expression,
  Func,
  Ordering,
  ParamRef,
  RelationPath,
  ScalarSubquery,
  Window,
  WindowDef,
  WindowFunc,
  and,
  excluded,
  exists,
  func,
  not,
  or,
  outer,
  param,
  window,
} from "./expr.js";
export type { Many, Compat, Operand, OrderOptions } from "./expr.js";
export { ModelMeta, Registry, define, load, loads, registry } from "./model.js";
export type { Instance, ModelClass, Row, SchemaIR, SoftDeletable } from "./model.js";
export { ManyRelatedSet, Prefetch, Prepared, QuerySet, RelatedSet } from "./query.js";
export type { Page, PageOptions } from "./pagination.js";
export type { DoNothing, DoUpdate, FieldOrder, LoadOf, LockOptions, PreparedInsert, PreparedUpdate, Simplify } from "./query.js";
export { Select } from "./select.js";
export { Cte, CteColumn } from "./cte.js";
export type { CteSelf } from "./cte.js";
export { Database, connect, getDatabase } from "./db.js";
export type { ConnectOptions } from "./db.js";
export {
  DatabaseError,
  DoesNotExist,
  IntegrityError,
  LockNotAvailable,
  MultipleObjectsReturned,
  NotConnected,
  NotLoaded,
  ORMError,
  QueryError,
  SchemaError,
  TransactionRequired,
  VersionConflict,
  WriteProtected,
} from "./errors.js";
export { allowWrites } from "./protection.js";
export * as debug from "./debug.js";
export { Migration, MigrationError, Migrations, Migrator } from "./migrations.js";
export type { Plan, Status, Step, SchemaSource } from "./migrations.js";
