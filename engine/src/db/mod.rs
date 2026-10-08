//! Database drivers.
//!
//! The planner produces sea-query statements; [`build`] renders them for the
//! connection's [`Dialect`] into SQL text plus [`Value`]s, and a driver runs them.
//! Everything above this module is driver-neutral: a driver implements [`Driver`],
//! [`Executor`], [`Transaction`] and [`RowSet`] for one database, and [`connect`] picks
//! it from the URL scheme. A build compiles PostgreSQL (tokio-postgres), SQLite
//! (rusqlite), or both.

#[cfg(feature = "postgres")]
mod numeric;
#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "sqlite")]
mod sqlite;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use sea_query::{QueryStatementWriter, Value};
#[cfg(feature = "postgres")]
use sea_query::PostgresQueryBuilder;
#[cfg(feature = "sqlite")]
use sea_query::SqliteQueryBuilder;

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

/// One decoded cell, borrowing from its row set. Bindings turn it into their own value.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell<'a> {
    Null,
    Bool(bool),
    Int(i32),
    BigInt(i64),
    Float(f64),
    /// Text, or the label of a database enum.
    Text(&'a str),
    /// timestamptz, in UTC as on the wire.
    DateTime(DateTime<Utc>),
    Date(NaiveDate),
    Uuid(uuid::Uuid),
    Json(serde_json::Value),
    /// A `numeric` as decimal text: `-12.50`, `NaN`.
    Decimal(String),
    Array(Vec<Cell<'a>>),
}

/// The rows one statement returned, decoded by the schema's column types.
pub trait RowSet: Send + Sync {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// One cell (enum values as stored: the binding maps them).
    fn cell(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Cell<'_>>;
    /// One cell as a bind parameter (prefetch keys are read back this way).
    fn value(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Value>;
    fn get_i64(&self, row: usize, col: usize) -> DbResult<i64>;
    fn get_bool(&self, row: usize, col: usize) -> DbResult<bool>;
}

/// Something statements run on: the pool (each call takes a connection) or an open
/// transaction (every call uses its connection).
pub trait Executor: Send + Sync {
    fn dialect(&self) -> Dialect { Dialect::Postgres }
    /// A migration may rebuild SQLite tables with FK checks deferred until commit.
    fn begin_migration(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> { self.begin() }
    fn query(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>>;
    /// Rows affected.
    fn execute(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<u64>>;
    /// One or more statements without parameters (DDL scripts, raw SQL).
    fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>>;
    /// A query whose columns are all read as text (tooling such as the migration runner).
    fn query_text(&self, sql: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>>;
    /// A single advisory-lock result, without materializing text rows in the binding.
    fn advisory_lock(&self, key: i64, exclusive: bool, nowait: bool) -> BoxFuture<'_, DbResult<bool>> {
        Box::pin(async move {
            let rows = self.query_text(crate::advisory::sql(key, exclusive, nowait)).await?;
            Ok(!nowait || rows.first().and_then(|r| r.first()).and_then(|v| v.as_deref()) == Some("true"))
        })
    }
    /// A transaction, or a savepoint when called on a transaction.
    fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>>;
}

pub trait Transaction: Executor {
    fn commit(&self) -> BoxFuture<'_, DbResult<()>>;
    fn rollback(&self) -> BoxFuture<'_, DbResult<()>>;
}

pub trait Driver: Executor {
    fn close(&self) -> BoxFuture<'_, ()>;
    /// A session advisory lock on a pinned pool connection, outside any transaction.
    /// `None` when it is not taken: held with `nowait`, or still held after `timeout_ms`.
    fn session_lock(
        &self, _key: i64, _exclusive: bool, _nowait: bool, _timeout_ms: Option<u64>,
    ) -> BoxFuture<'_, DbResult<Option<Arc<dyn SessionLock>>>> {
        Box::pin(async { Err(DbError::other(format!("{} does not support advisory locks", self.dialect().name()))) })
    }
}

/// A held session advisory lock. Dropped without `release`, it closes its connection,
/// so the server releases the lock.
pub trait SessionLock: Send + Sync {
    /// Unlocks, and gives the connection back to the pool.
    fn release(&self) -> BoxFuture<'_, DbResult<()>>;
}

/// Opens the selected driver for `postgres://`, `postgresql://` or `sqlite://`.
pub async fn connect(url: &str, max_connections: usize) -> DbResult<Arc<dyn Driver>> {
    let _ = max_connections;
    let scheme = url.split_once("://").map(|(s, _)| s).unwrap_or("");
    match scheme {
        #[cfg(feature = "sqlite")]
        "sqlite" => Ok(Arc::new(sqlite::SqliteDriver::connect(url).await?)),
        #[cfg(feature = "postgres")]
        "postgres" | "postgresql" => Ok(Arc::new(postgres::PgDriver::connect(url, max_connections).await?)),
        #[allow(unreachable_patterns)]
        "postgres" | "postgresql" | "sqlite" => Err(DbError::other(format!("database backend for {scheme:?} is not compiled into this native profile"))),
        other => Err(DbError::other(format!("unsupported database URL scheme {other:?}"))),
    }
}

/// SQL text and parameters of a statement in `dialect`.
pub fn build<S: QueryStatementWriter>(dialect: Dialect, stmt: &S) -> (String, Vec<Value>) {
    let (sql, values): (String, sea_query::Values) = match dialect {
        #[cfg(feature = "postgres")]
        Dialect::Postgres => stmt.build(PostgresQueryBuilder),
        #[cfg(feature = "sqlite")]
        Dialect::Sqlite => stmt.build(SqliteQueryBuilder),
        #[allow(unreachable_patterns)]
        _ => panic!("database dialect is not compiled into this native profile"),
    };
    (sql, values.0)
}

/// `stmt` with parameters inlined, for debugging.
pub fn to_string<S: QueryStatementWriter>(dialect: Dialect, stmt: &S) -> String {
    match dialect {
        #[cfg(feature = "postgres")]
        Dialect::Postgres => stmt.to_string(PostgresQueryBuilder),
        #[cfg(feature = "sqlite")]
        Dialect::Sqlite => stmt.to_string(SqliteQueryBuilder),
        #[allow(unreachable_patterns)]
        _ => panic!("database dialect is not compiled into this native profile"),
    }
}

/// Reject unsupported schema dialects before planning or opening connections.
pub fn require_dialect(dialect: Dialect) -> DbResult<()> {
    let enabled = match dialect {
        Dialect::Postgres => cfg!(feature = "postgres"),
        Dialect::Sqlite => cfg!(feature = "sqlite"),
    };
    if enabled { Ok(()) } else { Err(DbError::other(format!("{} is not compiled into this native profile", dialect.name()))) }
}
