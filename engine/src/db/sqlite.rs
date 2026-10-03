//! One serialized SQLite connection on a dedicated thread. No SQLite calls on Tokio.
use std::sync::{atomic::{AtomicBool, AtomicU64, Ordering}, mpsc, Arc, Mutex};

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use rusqlite::{Connection, types::Value as SqlValue};
use sea_query::Value;
use tokio::sync::{oneshot, Mutex as AsyncMutex, OwnedMutexGuard};

use super::{BoxFuture, Cell, DbError, DbResult, Driver, ErrorKind, Executor, RowSet, Transaction};
use orm_core::{dialect::Dialect, ir::{ColType, ValueType}};

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

fn sqlite_err(e: rusqlite::Error) -> DbError {
    let kind = match &e {
        rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::ConstraintViolation => ErrorKind::Integrity,
        rusqlite::Error::SqliteFailure(e, _) if matches!(e.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => ErrorKind::LockNotAvailable,
        _ => ErrorKind::Other,
    };
    DbError { kind, message: e.to_string() }
}

fn closed() -> DbError { DbError::other("SQLite connection or transaction is closed") }

struct Worker {
    sender: Mutex<Option<mpsc::Sender<Job>>>,
    next_savepoint: AtomicU64,
}

impl Worker {
    async fn call<T: Send + 'static>(&self, f: impl FnOnce(&mut Connection) -> DbResult<T> + Send + 'static) -> DbResult<T> {
        let (send, receive) = oneshot::channel();
        self.enqueue(Box::new(move |conn| { let _ = send.send(f(conn)); }))?;
        receive.await.map_err(|_| closed())?
    }

    fn enqueue(&self, job: Job) -> DbResult<()> {
        self.sender.lock().unwrap().as_ref().ok_or_else(closed)?.send(job).map_err(|_| closed())
    }
}

pub struct SqliteDriver {
    worker: Arc<Worker>,
    gate: Arc<AsyncMutex<()>>,
}

impl SqliteDriver {
    pub async fn connect(url: &str) -> DbResult<Self> {
        let path = url.strip_prefix("sqlite://").ok_or_else(|| DbError::other("use sqlite://:memory: or sqlite://path/to/database.db"))?.to_owned();
        if path.is_empty() || path.contains('?') || path.contains('#') { return Err(DbError::other("SQLite URL requires a file path or :memory:; URL options are unsupported")); }
        let (sender, receive) = mpsc::channel::<Job>();
        let (ready, opened) = oneshot::channel();
        std::thread::Builder::new().name("orm-sqlite".into()).spawn(move || {
            let result = Connection::open(&path).and_then(|conn| {
                conn.busy_timeout(std::time::Duration::from_secs(5))?;
                conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA case_sensitive_like = ON;")?;
                Ok(conn)
            });
            match result {
                Ok(mut conn) => {
                    if ready.send(Ok(())).is_err() { return; }
                    for job in receive { job(&mut conn); }
                }
                Err(e) => { let _ = ready.send(Err(sqlite_err(e))); }
            }
        }).map_err(|e| DbError::other(e.to_string()))?;
        opened.await.map_err(|_| closed())??;
        Ok(Self { worker: Arc::new(Worker { sender: Mutex::new(Some(sender)), next_savepoint: AtomicU64::new(0) }), gate: Arc::new(AsyncMutex::new(())) })
    }

    async fn with<T: Send + 'static>(&self, f: impl FnOnce(&mut Connection) -> DbResult<T> + Send + 'static) -> DbResult<T> {
        let _guard = self.gate.lock().await;
        self.worker.call(f).await
    }

    async fn transaction(&self, migration: bool) -> DbResult<Arc<dyn Transaction>> {
        let guard = self.gate.clone().lock_owned().await;
        let state = Arc::new(TxState {
            worker: self.worker.clone(), gate: Arc::new(AsyncMutex::new(())), guard: Mutex::new(Some(guard)),
            done: AtomicBool::new(false), savepoint: None, migration, parent: Mutex::new(None),
        });
        // State already owns the guard: cancellation queues rollback before releasing it.
        self.worker.call(move |conn| {
            if migration { conn.execute_batch("PRAGMA foreign_keys = OFF").map_err(sqlite_err)?; }
            conn.execute_batch("BEGIN IMMEDIATE").map_err(sqlite_err)
        }).await?;
        Ok(Arc::new(SqliteTx { state }))
    }
}

fn bind(v: Value) -> DbResult<SqlValue> {
    if v == v.as_null() { return Ok(SqlValue::Null); }
    Ok(match v {
        Value::Bool(Some(v)) => SqlValue::Integer(i64::from(v)),
        Value::TinyInt(Some(v)) => SqlValue::Integer(v.into()),
        Value::SmallInt(Some(v)) => SqlValue::Integer(v.into()),
        Value::Int(Some(v)) => SqlValue::Integer(v.into()),
        Value::BigInt(Some(v)) => SqlValue::Integer(v),
        Value::TinyUnsigned(Some(v)) => SqlValue::Integer(v.into()),
        Value::SmallUnsigned(Some(v)) => SqlValue::Integer(v.into()),
        Value::Unsigned(Some(v)) => SqlValue::Integer(v.into()),
        Value::BigUnsigned(Some(v)) => SqlValue::Integer(i64::try_from(v).map_err(|_| DbError::other("SQLite integer out of range"))?),
        Value::Float(Some(v)) => SqlValue::Real(v.into()),
        Value::Double(Some(v)) => SqlValue::Real(v),
        Value::String(Some(v)) => SqlValue::Text(v.to_string()),
        Value::Char(Some(v)) => SqlValue::Text(v.to_string()),
        Value::Bytes(Some(v)) => SqlValue::Blob(v.to_vec()),
        Value::Json(Some(v)) => SqlValue::Text(v.to_string()),
        Value::Uuid(Some(v)) => SqlValue::Text(v.to_string()),
        Value::ChronoDate(Some(v)) => SqlValue::Text(v.to_string()),
        Value::ChronoDateTimeUtc(Some(v)) => SqlValue::Text(v.format("%Y-%m-%d %H:%M:%S%.f").to_string()),
        Value::ChronoDateTimeWithTimeZone(Some(v)) => SqlValue::Text(v.with_timezone(&Utc).format("%Y-%m-%d %H:%M:%S%.f").to_string()),
        Value::ChronoDateTime(Some(v)) => SqlValue::Text(v.and_utc().format("%Y-%m-%d %H:%M:%S%.f").to_string()),
        _ => return Err(DbError::other("unsupported SQLite parameter type")),
    })
}

fn query(conn: &mut Connection, sql: String, args: Vec<Value>) -> DbResult<Box<dyn RowSet>> {
    let args = args.into_iter().map(bind).collect::<DbResult<Vec<_>>>()?;
    let mut stmt = conn.prepare_cached(&sql).map_err(sqlite_err)?;
    let width = stmt.column_count();
    let mut rows = stmt.query(rusqlite::params_from_iter(args)).map_err(sqlite_err)?;
    let mut out = vec![];
    while let Some(row) = rows.next().map_err(sqlite_err)? {
        out.push((0..width).map(|i| row.get::<_, SqlValue>(i).map_err(sqlite_err)).collect::<DbResult<Vec<_>>>()?);
    }
    Ok(Box::new(Rows(out)))
}

fn execute(conn: &mut Connection, sql: String, args: Vec<Value>) -> DbResult<u64> {
    let args = args.into_iter().map(bind).collect::<DbResult<Vec<_>>>()?;
    conn.execute(&sql, rusqlite::params_from_iter(args)).map(|n| n as u64).map_err(sqlite_err)
}

fn batch(conn: &mut Connection, sql: String) -> DbResult<u64> {
    let before = conn.total_changes();
    conn.execute_batch(&sql).map_err(sqlite_err)?;
    Ok(conn.total_changes() - before)
}

fn query_text(conn: &mut Connection, sql: String) -> DbResult<Vec<Vec<Option<String>>>> {
    let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
    let width = stmt.column_count();
    let mut rows = stmt.query([]).map_err(sqlite_err)?;
    let mut out = vec![];
    while let Some(row) = rows.next().map_err(sqlite_err)? {
        out.push((0..width).map(|i| Ok(match row.get::<_, SqlValue>(i).map_err(sqlite_err)? {
            SqlValue::Null => None, SqlValue::Text(v) => Some(v),
            SqlValue::Integer(v) => Some(v.to_string()), SqlValue::Real(v) => Some(v.to_string()),
            SqlValue::Blob(_) => return Err(DbError::other("cannot read a blob as text")),
        })).collect::<DbResult<Vec<_>>>()?);
    }
    Ok(out)
}

impl Executor for SqliteDriver {
    fn dialect(&self) -> Dialect { Dialect::Sqlite }
    fn query(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>> { Box::pin(self.with(move |c| query(c, sql, args))) }
    fn execute(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<u64>> { Box::pin(self.with(move |c| execute(c, sql, args))) }
    fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>> { Box::pin(self.with(move |c| batch(c, sql))) }
    fn query_text(&self, sql: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>> { Box::pin(self.with(move |c| query_text(c, sql))) }
    fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> { Box::pin(self.transaction(false)) }
    fn begin_migration(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> { Box::pin(self.transaction(true)) }
}

impl Driver for SqliteDriver {
    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _guard = self.gate.lock().await;
            self.worker.sender.lock().unwrap().take();
        })
    }
}

struct TxState {
    worker: Arc<Worker>, gate: Arc<AsyncMutex<()>>, guard: Mutex<Option<OwnedMutexGuard<()>>>,
    done: AtomicBool, savepoint: Option<String>, migration: bool, parent: Mutex<Option<Arc<TxState>>>,
}

impl Drop for TxState {
    fn drop(&mut self) {
        if self.done.swap(true, Ordering::AcqRel) { return; }
        let guard = self.guard.lock().unwrap().take();
        let sql = self.savepoint.as_ref().map(|n| format!("ROLLBACK TO {n}; RELEASE {n}")).unwrap_or_else(|| "ROLLBACK".into());
        let migration = self.migration;
        let _ = self.worker.enqueue(Box::new(move |c| {
            let _guard = guard;
            let _ = c.execute_batch(&sql);
            if migration { let _ = c.execute_batch("PRAGMA foreign_keys = ON"); }
        }));
    }
}

struct SqliteTx { state: Arc<TxState> }

impl SqliteTx {
    async fn with<T: Send + 'static>(&self, f: impl FnOnce(&mut Connection) -> DbResult<T> + Send + 'static) -> DbResult<T> {
        let _guard = self.state.gate.lock().await;
        if self.state.done.load(Ordering::Acquire) { return Err(closed()); }
        self.state.worker.call(f).await
    }

    async fn finish(&self, commit: bool) -> DbResult<()> {
        let _gate = self.state.gate.lock().await;
        if self.state.done.swap(true, Ordering::AcqRel) { return Err(closed()); }
        let guard = self.state.guard.lock().unwrap().take();
        let parent = self.state.parent.lock().unwrap().take();
        let name = self.state.savepoint.clone();
        let migration = self.state.migration;
        self.state.worker.call(move |c| {
            let _guard = guard;
            let _parent = parent;
            let result = (|| {
                if migration && commit {
                    let mut stmt = c.prepare("PRAGMA foreign_key_check").map_err(sqlite_err)?;
                    if stmt.query([]).map_err(sqlite_err)?.next().map_err(sqlite_err)?.is_some() {
                        return Err(DbError { kind: ErrorKind::Integrity, message: "SQLite migration violates foreign key constraints".into() });
                    }
                }
                let sql = match (&name, commit) {
                    (Some(n), true) => format!("RELEASE {n}"),
                    (Some(n), false) => format!("ROLLBACK TO {n}; RELEASE {n}"),
                    (None, true) => "COMMIT".into(), (None, false) => "ROLLBACK".into(),
                };
                c.execute_batch(&sql).map_err(sqlite_err)
            })();
            if result.is_err() && name.is_none() { let _ = c.execute_batch("ROLLBACK"); }
            if migration { c.execute_batch("PRAGMA foreign_keys = ON").map_err(sqlite_err)?; }
            result
        }).await
    }
}

impl Executor for SqliteTx {
    fn dialect(&self) -> Dialect { Dialect::Sqlite }
    fn query(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>> { Box::pin(self.with(move |c| query(c, sql, args))) }
    fn execute(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<u64>> { Box::pin(self.with(move |c| execute(c, sql, args))) }
    fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>> { Box::pin(self.with(move |c| batch(c, sql))) }
    fn query_text(&self, sql: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>> { Box::pin(self.with(move |c| query_text(c, sql))) }
    fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
        Box::pin(async move {
            let guard = self.state.gate.clone().lock_owned().await;
            if self.state.done.load(Ordering::Acquire) { return Err(closed()); }
            let name = format!("orm_sp_{}", self.state.worker.next_savepoint.fetch_add(1, Ordering::Relaxed));
            let state = Arc::new(TxState { worker: self.state.worker.clone(), gate: Arc::new(AsyncMutex::new(())), guard: Mutex::new(Some(guard)), done: AtomicBool::new(false), savepoint: Some(name.clone()), migration: false, parent: Mutex::new(Some(self.state.clone())) });
            self.state.worker.call(move |c| c.execute_batch(&format!("SAVEPOINT {name}")).map_err(sqlite_err)).await?;
            Ok(Arc::new(SqliteTx { state }) as Arc<dyn Transaction>)
        })
    }
}

impl Transaction for SqliteTx {
    fn commit(&self) -> BoxFuture<'_, DbResult<()>> { Box::pin(self.finish(true)) }
    fn rollback(&self) -> BoxFuture<'_, DbResult<()>> { Box::pin(self.finish(false)) }
}

struct Rows(Vec<Vec<SqlValue>>);

impl Rows {
    fn get(&self, row: usize, col: usize) -> DbResult<&SqlValue> { self.0.get(row).and_then(|r| r.get(col)).ok_or_else(|| DbError::other("SQLite result index out of range")) }
    fn text(&self, row: usize, col: usize) -> DbResult<&str> { match self.get(row, col)? { SqlValue::Text(s) => Ok(s), _ => Err(DbError::other("expected SQLite text value")) } }
}

impl RowSet for Rows {
    fn len(&self) -> usize { self.0.len() }
    fn cell(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Cell<'_>> {
        if matches!(self.get(row, col)?, SqlValue::Null) { return Ok(Cell::Null); }
        if ty.array { return Err(DbError::other("sqlite does not support array columns")); }
        Ok(match ty.ty {
            ColType::Bool => Cell::Bool(self.get_bool(row, col)?),
            ColType::BigInt => Cell::BigInt(self.get_i64(row, col)?),
            ColType::Int => Cell::Int(i32::try_from(self.get_i64(row, col)?).map_err(|_| DbError::other("SQLite Int value out of range"))?),
            ColType::Float => Cell::Float(match self.get(row, col)? { SqlValue::Real(v) => *v, SqlValue::Integer(v) => *v as f64, _ => return Err(DbError::other("expected SQLite number")) }),
            ColType::String | ColType::Text => Cell::Text(self.text(row, col)?),
            ColType::Json => Cell::Json(serde_json::from_str(self.text(row, col)?).map_err(|e| DbError::other(e.to_string()))?),
            ColType::Uuid => Cell::Uuid(uuid::Uuid::parse_str(self.text(row, col)?).map_err(|e| DbError::other(e.to_string()))?),
            ColType::Date => Cell::Date(NaiveDate::parse_from_str(self.text(row, col)?, "%Y-%m-%d").map_err(|e| DbError::other(e.to_string()))?),
            ColType::DateTime => {
                let s = self.text(row, col)?;
                let dt = DateTime::parse_from_rfc3339(s).map(|v| v.with_timezone(&Utc)).or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f").map(|v| v.and_utc())).map_err(|e| DbError::other(e.to_string()))?;
                Cell::DateTime(dt)
            }
            ColType::Decimal => return Err(DbError::other("sqlite does not support exact decimal columns")),
        })
    }
    fn value(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Value> {
        Ok(match self.cell(row, col, ty)? {
            Cell::Null => crate::params::null_of(Some(ty)), Cell::Bool(v) => v.into(),
            Cell::Int(v) => v.into(), Cell::BigInt(v) => v.into(), Cell::Float(v) => v.into(),
            Cell::Text(v) => v.into(), Cell::Json(v) => v.into(), Cell::Uuid(v) => v.into(),
            Cell::Date(v) => v.into(), Cell::DateTime(v) => v.into(),
            _ => return Err(DbError::other("unsupported SQLite value")),
        })
    }
    fn get_i64(&self, row: usize, col: usize) -> DbResult<i64> { match self.get(row, col)? { SqlValue::Integer(v) => Ok(*v), _ => Err(DbError::other("expected SQLite integer")) } }
    fn get_bool(&self, row: usize, col: usize) -> DbResult<bool> { self.get_i64(row, col).map(|v| v != 0) }
}
