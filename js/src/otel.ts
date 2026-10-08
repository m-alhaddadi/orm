/**
 * OpenTelemetry spans for the queries of a database: `instrument(db)`.
 *
 * Loads `@opentelemetry/api` (an optional peer dependency) only when no tracer is
 * given; the package imports it nowhere else.
 */

import type { Database, QueryEvent } from "./db.js";

/** The span the ORM uses: a subset of the OpenTelemetry `Span`. */
export interface Span {
  setStatus(status: { code: number; message?: string }): unknown;
  end(endTime?: number): void;
}

/** The tracer the ORM uses: a subset of the OpenTelemetry `Tracer`. */
export interface Tracer {
  startSpan(name: string, options?: { kind?: number; attributes?: Record<string, string | number>; startTime?: number }): Span;
}

export interface InstrumentOptions {
  /** The tracer of the spans (default: `trace.getTracer("orm")` of `@opentelemetry/api`). */
  readonly tracer?: Tracer;
}

// The values of the OpenTelemetry API's SpanKind.CLIENT and SpanStatusCode.ERROR.
const CLIENT = 2;
const ERROR = 2;

/**
 * Gives each statement of `db` a client span; gives a function that stops it.
 *
 * The span starts and ends at the statement's own times and has the active span of the
 * calling async context as its parent. Its name is the SQL operation (`SELECT`), and it
 * has `db.system.name`, `db.operation.name`, `db.query.text` (the SQL with placeholders,
 * never the values) and `db.response.returned_rows`. A failed statement gets the error
 * status and the database's message.
 */
export async function instrument(db: Database, options: InstrumentOptions = {}): Promise<() => void> {
  const tracer = options.tracer ?? (await defaultTracer());
  const system = db.url.startsWith("sqlite:") ? "sqlite" : "postgresql";
  return db.onQuery((e: QueryEvent) => {
    const operation = /^[\s(]*(\w+)/.exec(e.sql)?.[1]?.toUpperCase() ?? "SQL";
    const span = tracer.startSpan(operation, {
      kind: CLIENT,
      attributes: {
        "db.system.name": system,
        "db.operation.name": operation,
        "db.query.text": e.sql,
        "db.response.returned_rows": e.rows,
      },
      startTime: e.start,
    });
    if (e.error !== null) span.setStatus({ code: ERROR, message: e.error });
    span.end(e.start + e.duration);
  });
}

async function defaultTracer(): Promise<Tracer> {
  // A variable specifier: the type check and the build work without the package.
  const name = "@opentelemetry/api";
  let api: { trace: { getTracer(name: string): Tracer } };
  try {
    api = (await import(name)) as typeof api;
  } catch (e) {
    throw new Error("orm/otel needs @opentelemetry/api: npm install @opentelemetry/api", { cause: e });
  }
  return api.trace.getTracer("orm");
}
