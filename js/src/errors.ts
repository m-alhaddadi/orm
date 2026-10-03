/** Errors thrown by the ORM. */

/** Base class of every error the ORM throws (database errors included). */
export class ORMError extends Error {
  override name = "ORMError";
}

/** Reported by the database or the driver. */
export class DatabaseError extends ORMError {
  override name = "DatabaseError";
}

/** Unique, foreign key, check, exclusion or not-null violation. */
export class IntegrityError extends DatabaseError {
  override name = "IntegrityError";
}

/** A row lock taken with `nowait` (or a lock timeout) found the rows locked. */
export class LockNotAvailable extends DatabaseError {
  override name = "LockNotAvailable";
}

/** The query does not match the schema, or asks for something unsupported. */
export class QueryError extends ORMError {
  override name = "QueryError";
}

/** The schema file or IR is invalid. */
export class SchemaError extends ORMError {
  override name = "SchemaError";
}

/** `get()` matched no row. Each model has a subclass: `User.DoesNotExist`. */
export class DoesNotExist extends ORMError {
  override name = "DoesNotExist";
}

/** `get()` matched more than one row. */
export class MultipleObjectsReturned extends ORMError {
  override name = "MultipleObjectsReturned";
}

/**
 * A relation was read on an instance without being loaded first. The types only allow
 * reading relations a query loaded (`selectRelated` / `prefetchRelated`), so this is
 * thrown only past a cast.
 */
export class NotLoaded extends ORMError {
  override name = "NotLoaded";
}

/** No database: call `await connect(url)` first. */
export class NotConnected extends ORMError {
  override name = "NotConnected";
}

/** A lock was asked for outside `db.transaction()`, where it would be released as soon
 * as the statement ends. */
export class TransactionRequired extends ORMError {
  override name = "TransactionRequired";
}

/** The migrations directory and the database's migration history disagree. */
export class MigrationError extends ORMError {
  override name = "MigrationError";
}

const KINDS: Record<string, new (message: string) => Error> = {
  MigrationError,
  DatabaseError,
  IntegrityError,
  LockNotAvailable,
  QueryError,
  SchemaError,
  TypeError,
};

/** The error class for an error the native engine threw (`[orm:<Kind>] message`). */
export function fromNative(e: unknown): unknown {
  if (!(e instanceof Error)) {
    return e;
  }
  const m = /^\[orm:(\w+)\] ([\s\S]*)$/.exec(e.message);
  const cls = m && KINDS[m[1]!];
  if (!m || !cls) {
    return e;
  }
  const out = new cls(m[2]!);
  if (e.stack) {
    out.stack = `${out.name}: ${m[2]}\n${e.stack.split("\n").slice(1).join("\n")}`;
  }
  return out;
}
