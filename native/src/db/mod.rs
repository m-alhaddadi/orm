//! Database drivers.
//!
//! The planner produces sea-query statements; [`build`] renders them for the
//! connection's [`Dialect`] into SQL text plus [`Value`]s, and a driver runs them.
//! Everything above this module is driver-neutral: a driver implements [`Driver`],
//! [`Executor`], [`Transaction`] and [`RowSet`] for one database, and [`connect`] picks
//! it from the URL scheme. Postgres (tokio-postgres) is the only driver today.

mod numeric;
mod postgres;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use pyo3::prelude::*;
use sea_query::{PostgresQueryBuilder, QueryStatementWriter, Value};

use orm_core::dialect::Dialect;
use orm_core::ir::ValueType;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type DbResult<T> = Result<T, DbError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Unique, foreign key, check, exclusion or not-null violation.
    Integrity,
    /// A `NOWAIT` lock (or a lock timeout) found the rows locked.
    LockNotAvailable,
    Other,
}

#[derive(Debug)]
pub struct DbError {
    pub kind: ErrorKind,
    pub message: String,
}

impl DbError {
    pub fn other(message: impl Into<String>) -> Self {
        DbError { kind: ErrorKind::Other, message: message.into() }
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The rows one statement returned, decoded by the schema's column types.
pub trait RowSet: Send + Sync {
    fn len(&self) -> usize;
    /// One cell as a Python value (enum values as stored: the frontend maps them).
    fn cell(&self, py: Python<'_>, row: usize, col: usize, ty: ValueType) -> PyResult<Py<PyAny>>;
    /// One cell as a bind parameter (prefetch keys are read back this way).
    fn value(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Value>;
    fn get_i64(&self, row: usize, col: usize) -> DbResult<i64>;
    fn get_bool(&self, row: usize, col: usize) -> DbResult<bool>;
}

/// Something statements run on: the pool (each call takes a connection) or an open
/// transaction (every call uses its connection).
pub trait Executor: Send + Sync {
    fn query(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>>;
    /// Rows affected.
    fn execute(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<u64>>;
    /// One or more statements without parameters (DDL scripts, raw SQL).
    fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>>;
    /// A query whose columns are all read as text (tooling such as the migration runner).
    fn query_text(&self, sql: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>>;
    /// A transaction, or a savepoint when called on a transaction.
    fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>>;
}

pub trait Transaction: Executor {
    fn commit(&self) -> BoxFuture<'_, DbResult<()>>;
    fn rollback(&self) -> BoxFuture<'_, DbResult<()>>;
}

pub trait Driver: Executor {
    fn dialect(&self) -> Dialect;
    fn close(&self) -> BoxFuture<'_, ()>;
}

/// Opens a connection pool for `url` (`postgres://` / `postgresql://`).
pub async fn connect(url: &str, max_connections: usize) -> DbResult<Arc<dyn Driver>> {
    let scheme = url.split_once("://").map(|(s, _)| s).unwrap_or("");
    match scheme {
        "postgres" | "postgresql" => Ok(Arc::new(postgres::PgDriver::connect(url, max_connections).await?)),
        other => Err(DbError::other(format!("unsupported database URL scheme {other:?}"))),
    }
}

/// SQL text and parameters of a statement in `dialect`.
pub fn build<S: QueryStatementWriter>(dialect: Dialect, stmt: &S) -> (String, Vec<Value>) {
    let (sql, values) = match dialect {
        Dialect::Postgres => stmt.build(PostgresQueryBuilder),
    };
    (sql, values.0)
}

/// `stmt` with parameters inlined, for debugging.
pub fn to_string<S: QueryStatementWriter>(dialect: Dialect, stmt: &S) -> String {
    match dialect {
        Dialect::Postgres => stmt.to_string(PostgresQueryBuilder),
    }
}
