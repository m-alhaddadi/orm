//! Per-statement events for query hooks and the N+1 finder.
//!
//! A binding passes a [`Trace`] only when something listens; [`wrap`] then puts a
//! [`Traced`] executor around the pool or transaction, which records each statement it
//! sends (prefetch queries and `update_many` batches included). The binding drains the
//! trace after the call, in the caller's task, also when the call failed.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use sea_query::Value;

use crate::db::{BoxFuture, DbResult, Executor, RawRows, RowSet, Transaction};
use orm_core::dialect::Dialect;

/// One statement the database ran.
#[derive(Debug, Clone)]
pub struct QueryEvent {
    /// The SQL with its parameter placeholders: the statement shape.
    pub sql: String,
    pub start: SystemTime,
    pub duration: Duration,
    /// Rows returned, or rows affected by a statement without rows.
    pub rows: u64,
    /// The error message when the statement failed.
    pub error: Option<String>,
}

/// The events of the calls that share it.
#[derive(Debug, Default, Clone)]
pub struct Trace(Arc<Mutex<Vec<QueryEvent>>>);

impl Trace {
    pub fn new() -> Self {
        Self::default()
    }

    /// The events so far, oldest first; the trace is empty afterwards.
    pub fn take(&self) -> Vec<QueryEvent> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn push(&self, event: QueryEvent) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event);
    }
}

/// `conn`, recording into `trace` when there is one.
pub fn wrap(conn: Arc<dyn Executor>, trace: Option<&Trace>) -> Arc<dyn Executor> {
    match trace {
        Some(t) => Arc::new(Traced { conn, tx: None, trace: t.clone() }),
        None => conn,
    }
}

/// An executor that records each statement; transactions it begins record too.
pub struct Traced {
    conn: Arc<dyn Executor>,
    tx: Option<Arc<dyn Transaction>>,
    trace: Trace,
}

impl Traced {
    async fn timed<T>(&self, sql: String, run: impl Future<Output = DbResult<T>>, rows: impl Fn(&T) -> u64) -> DbResult<T> {
        let start = SystemTime::now();
        let clock = Instant::now();
        let out = run.await;
        let duration = clock.elapsed();
        let (rows, error) = match &out {
            Ok(v) => (rows(v), None),
            Err(e) => (0, Some(e.message.clone())),
        };
        self.trace.push(QueryEvent { sql, start, duration, rows, error });
        out
    }

    fn traced_tx(&self, tx: Arc<dyn Transaction>) -> Arc<dyn Transaction> {
        Arc::new(Traced { conn: tx.clone(), tx: Some(tx), trace: self.trace.clone() })
    }
}

impl Executor for Traced {
    fn dialect(&self) -> Dialect {
        self.conn.dialect()
    }
    fn begin_migration(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
        Box::pin(async move { Ok(self.traced_tx(self.conn.begin_migration().await?)) })
    }
    fn query(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>> {
        Box::pin(self.timed(sql.clone(), self.conn.query(sql, args), |r| r.len() as u64))
    }
    fn fetch(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RawRows>>> {
        Box::pin(self.timed(sql.clone(), self.conn.fetch(sql, args), |r| r.len() as u64))
    }
    fn execute(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(self.timed(sql.clone(), self.conn.execute(sql, args), |n| *n))
    }
    fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(self.timed(sql.clone(), self.conn.batch(sql), |n| *n))
    }
    fn query_text(&self, sql: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>> {
        Box::pin(self.timed(sql.clone(), self.conn.query_text(sql), |r| r.len() as u64))
    }
    fn advisory_lock(&self, key: i64, exclusive: bool, nowait: bool) -> BoxFuture<'_, DbResult<bool>> {
        let sql = crate::advisory::sql(key, exclusive, nowait);
        Box::pin(self.timed(sql, self.conn.advisory_lock(key, exclusive, nowait), |_| 1))
    }
    fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
        Box::pin(async move { Ok(self.traced_tx(self.conn.begin().await?)) })
    }
}

impl Transaction for Traced {
    fn commit(&self) -> BoxFuture<'_, DbResult<()>> {
        match &self.tx {
            Some(tx) => tx.commit(),
            None => Box::pin(async { Err(crate::db::DbError::other("not a transaction")) }),
        }
    }
    fn rollback(&self) -> BoxFuture<'_, DbResult<()>> {
        match &self.tx {
            Some(tx) => tx.rollback(),
            None => Box::pin(async { Err(crate::db::DbError::other("not a transaction")) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_drains() {
        let t = Trace::new();
        t.push(QueryEvent { sql: "SELECT 1".into(), start: SystemTime::now(), duration: Duration::ZERO, rows: 1, error: None });
        assert_eq!(t.take().len(), 1);
        assert!(t.take().is_empty());
    }
}
