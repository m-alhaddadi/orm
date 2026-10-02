//! Postgres driver: tokio-postgres connections in a deadpool pool.
//!
//! * Parameters are sent in Postgres's binary format with their types stated in the
//!   `Parse` message (from the sea-query [`Value`] variant, which the planner picked from
//!   the column type), so the server never has to guess them.
//! * Statements are prepared once per connection and cached; the cache is cleared when
//!   it grows past [`STATEMENT_CACHE_MAX`] (e.g. many different `IN (...)` lengths).
//! * Transactions pin one pooled connection and run `BEGIN` / `SAVEPOINT` on it. A
//!   transaction dropped without commit or rollback closes its connection instead of
//!   returning it to the pool mid-transaction.
//! * TLS follows libpq's `sslmode`: `disable`; `prefer` (the default) and `require`
//!   encrypt without verifying the certificate; `verify-ca` / `verify-full` verify it
//!   against the webpki roots, hostname included.

use std::error::Error as StdError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bytes::BytesMut;
use chrono::{DateTime, NaiveDate, Utc};
use deadpool_postgres::{ClientWrapper, Manager, ManagerConfig, Object, Pool, RecyclingMethod};
use pyo3::prelude::*;
use pyo3::types::{PyList, PyTuple};
use pyo3::IntoPyObjectExt;
use sea_query::Value;
use tokio::sync::Mutex;
use tokio_postgres::types::{to_sql_checked, FromSql, IsNull, ToSql, Type};
use tokio_postgres::{NoTls, Row, SimpleQueryMessage, Statement};

use super::{BoxFuture, DbError, DbResult, Driver, ErrorKind, Executor, RowSet, Transaction};
use crate::convert::json_to_py;
use orm_core::dialect::Dialect;
use orm_core::ir::ColType;

const STATEMENT_CACHE_MAX: usize = 512;

fn pg_err(e: tokio_postgres::Error) -> DbError {
    match e.as_db_error() {
        Some(db) => {
            let code = db.code().code();
            let kind = if code.starts_with("23") {
                ErrorKind::Integrity
            } else if code == "55P03" {
                ErrorKind::LockNotAvailable
            } else {
                ErrorKind::Other
            };
            let mut message = format!("{} ({code}): {}", db.severity(), db.message());
            if let Some(detail) = db.detail() {
                message.push_str("\nDETAIL: ");
                message.push_str(detail);
            }
            DbError { kind, message }
        }
        None => DbError::other(e.to_string()),
    }
}

fn closed() -> DbError {
    DbError::other("transaction is already committed or rolled back")
}

// -- parameters ---------------------------------------------------------------------------

fn pg_type(v: &Value) -> DbResult<Type> {
    Ok(match v {
        Value::Bool(_) => Type::BOOL,
        Value::SmallInt(_) | Value::TinyInt(_) | Value::TinyUnsigned(_) => Type::INT2,
        Value::Int(_) | Value::SmallUnsigned(_) => Type::INT4,
        Value::BigInt(_) | Value::Unsigned(_) | Value::BigUnsigned(_) => Type::INT8,
        Value::Float(_) => Type::FLOAT4,
        Value::Double(_) => Type::FLOAT8,
        Value::String(_) => Type::TEXT,
        Value::Bytes(_) => Type::BYTEA,
        Value::Json(_) => Type::JSONB,
        Value::ChronoDate(_) => Type::DATE,
        Value::ChronoDateTimeWithTimeZone(_) | Value::ChronoDateTimeUtc(_) => Type::TIMESTAMPTZ,
        Value::ChronoDateTime(_) => Type::TIMESTAMP,
        Value::Uuid(_) => Type::UUID,
        other => return Err(DbError::other(format!("unsupported parameter value {other:?}"))),
    })
}

/// A sea-query value as a binary Postgres parameter of the type `pg_type` gives it.
#[derive(Debug)]
struct Param<'a>(&'a Value);

impl ToSql for Param<'_> {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn StdError + Sync + Send>> {
        match self.0 {
            Value::Bool(v) => v.to_sql(ty, out),
            Value::TinyInt(v) => v.map(i16::from).to_sql(ty, out),
            Value::TinyUnsigned(v) => v.map(i16::from).to_sql(ty, out),
            Value::SmallInt(v) => v.to_sql(ty, out),
            Value::SmallUnsigned(v) => v.map(i32::from).to_sql(ty, out),
            Value::Int(v) => v.to_sql(ty, out),
            Value::Unsigned(v) => v.map(i64::from).to_sql(ty, out),
            Value::BigInt(v) => v.to_sql(ty, out),
            Value::BigUnsigned(v) => v.map(i64::try_from).transpose()?.to_sql(ty, out),
            Value::Float(v) => v.to_sql(ty, out),
            Value::Double(v) => v.to_sql(ty, out),
            Value::String(v) => v.as_deref().to_sql(ty, out),
            Value::Bytes(v) => v.as_deref().to_sql(ty, out),
            Value::Json(v) => v.as_deref().to_sql(ty, out),
            Value::ChronoDate(v) => v.to_sql(ty, out),
            Value::ChronoDateTimeWithTimeZone(v) => v.to_sql(ty, out),
            Value::ChronoDateTimeUtc(v) => v.to_sql(ty, out),
            Value::ChronoDateTime(v) => v.to_sql(ty, out),
            Value::Uuid(v) => v.to_sql(ty, out),
            other => Err(format!("unsupported parameter value {other:?}").into()),
        }
    }

    fn accepts(_: &Type) -> bool {
        true // the statement was prepared with the types `pg_type` gives
    }

    to_sql_checked!();
}

async fn prepare(c: &ClientWrapper, sql: &str, args: &[Value]) -> DbResult<Statement> {
    let types = args.iter().map(pg_type).collect::<DbResult<Vec<_>>>()?;
    let stmt = c.prepare_typed_cached(sql, &types).await.map_err(pg_err)?;
    if c.statement_cache.size() > STATEMENT_CACHE_MAX {
        c.statement_cache.clear();
    }
    Ok(stmt)
}

async fn query(c: &ClientWrapper, sql: &str, args: &[Value]) -> DbResult<Box<dyn RowSet>> {
    let stmt = prepare(c, sql, args).await?;
    let params: Vec<Param<'_>> = args.iter().map(Param).collect();
    let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
    let rows = c.query(&stmt, &refs).await.map_err(pg_err)?;
    Ok(Box::new(PgRows(rows)))
}

async fn execute(c: &ClientWrapper, sql: &str, args: &[Value]) -> DbResult<u64> {
    let stmt = prepare(c, sql, args).await?;
    let params: Vec<Param<'_>> = args.iter().map(Param).collect();
    let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
    c.execute(&stmt, &refs).await.map_err(pg_err)
}

/// Simple-query protocol: several statements, no parameters, text results.
async fn simple(c: &ClientWrapper, sql: &str) -> DbResult<Vec<SimpleQueryMessage>> {
    c.simple_query(sql).await.map_err(pg_err)
}

async fn batch(c: &ClientWrapper, sql: &str) -> DbResult<u64> {
    Ok(simple(c, sql)
        .await?
        .iter()
        .map(|m| match m {
            SimpleQueryMessage::CommandComplete(n) => *n,
            _ => 0,
        })
        .sum())
}

async fn query_text(c: &ClientWrapper, sql: &str) -> DbResult<Vec<Vec<Option<String>>>> {
    Ok(simple(c, sql)
        .await?
        .iter()
        .filter_map(|m| match m {
            SimpleQueryMessage::Row(r) => Some((0..r.len()).map(|i| r.get(i).map(str::to_owned)).collect()),
            _ => None,
        })
        .collect())
}

// -- rows --------------------------------------------------------------------------------

struct PgRows(Vec<Row>);

fn get<'a, T: FromSql<'a>>(row: &'a Row, idx: usize) -> DbResult<Option<T>> {
    row.try_get::<_, Option<T>>(idx).map_err(|e| DbError::other(format!("column {idx}: {e}")))
}

fn to_py_err(e: DbError) -> PyErr {
    crate::errors::db_err(e)
}

fn cell_to_py(py: Python<'_>, row: &Row, idx: usize, ty: ColType) -> PyResult<Py<PyAny>> {
    let g = |e| to_py_err(e);
    Ok(match ty {
        ColType::BigInt => get::<i64>(row, idx).map_err(g)?.into_py_any(py)?,
        ColType::Int => get::<i32>(row, idx).map_err(g)?.into_py_any(py)?,
        ColType::Float => get::<f64>(row, idx).map_err(g)?.into_py_any(py)?,
        ColType::Bool => get::<bool>(row, idx).map_err(g)?.into_py_any(py)?,
        ColType::String | ColType::Text => get::<&str>(row, idx).map_err(g)?.into_py_any(py)?,
        // timestamptz is UTC on the wire; `DateTime<Utc>` reuses the
        // `datetime.timezone.utc` singleton instead of building a tzinfo per row.
        ColType::DateTime => get::<DateTime<Utc>>(row, idx).map_err(g)?.into_py_any(py)?,
        ColType::Date => get::<NaiveDate>(row, idx).map_err(g)?.into_py_any(py)?,
        ColType::Uuid => get::<uuid::Uuid>(row, idx).map_err(g)?.into_py_any(py)?,
        ColType::Json => match get::<serde_json::Value>(row, idx).map_err(g)? {
            Some(v) => json_to_py(py, &v)?,
            None => py.None(),
        },
    })
}

impl RowSet for PgRows {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn to_py<'py>(&self, py: Python<'py>, types: &[ColType]) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        let mut cells = Vec::with_capacity(types.len());
        for row in &self.0 {
            for (i, ty) in types.iter().enumerate() {
                cells.push(cell_to_py(py, row, i, *ty)?);
            }
            list.append(PyTuple::new(py, cells.drain(..))?)?;
        }
        Ok(list)
    }

    fn value(&self, row: usize, col: usize, ty: ColType) -> DbResult<Value> {
        let r = &self.0[row];
        Ok(match ty {
            ColType::BigInt => Value::BigInt(get(r, col)?),
            ColType::Int => Value::Int(get(r, col)?),
            ColType::Float => Value::Double(get(r, col)?),
            ColType::Bool => Value::Bool(get(r, col)?),
            ColType::String | ColType::Text => Value::String(get::<String>(r, col)?),
            ColType::DateTime => {
                Value::ChronoDateTimeWithTimeZone(get::<DateTime<Utc>>(r, col)?.map(|d| d.fixed_offset()))
            }
            ColType::Date => Value::ChronoDate(get(r, col)?),
            ColType::Uuid => Value::Uuid(get(r, col)?),
            ColType::Json => Value::Json(get::<serde_json::Value>(r, col)?.map(Box::new)),
        })
    }

    fn get_i64(&self, row: usize, col: usize) -> DbResult<i64> {
        get::<i64>(&self.0[row], col)?.ok_or_else(|| DbError::other("unexpected NULL"))
    }

    fn get_bool(&self, row: usize, col: usize) -> DbResult<bool> {
        get::<bool>(&self.0[row], col)?.ok_or_else(|| DbError::other("unexpected NULL"))
    }
}

// -- pool --------------------------------------------------------------------------------

pub struct PgDriver {
    pool: Pool,
}

impl PgDriver {
    pub async fn connect(url: &str, max_connections: usize) -> DbResult<Self> {
        let (config, tls) = tls::parse(url)?;
        let mgr_config = ManagerConfig { recycling_method: RecyclingMethod::Fast };
        let manager = match tls {
            Some(tls) => Manager::from_config(config, tls, mgr_config),
            None => Manager::from_config(config, NoTls, mgr_config),
        };
        let pool = Pool::builder(manager)
            .max_size(max_connections.max(1))
            .build()
            .map_err(|e| DbError::other(e.to_string()))?;
        drop(get_client(&pool).await?); // fail at connect() on a bad URL or password
        Ok(PgDriver { pool })
    }
}

async fn get_client(pool: &Pool) -> DbResult<Object> {
    pool.get().await.map_err(|e| match e {
        deadpool_postgres::PoolError::Backend(e) => pg_err(e),
        other => DbError::other(other.to_string()),
    })
}

impl Executor for PgDriver {
    fn query(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>> {
        Box::pin(async move { let c = get_client(&self.pool).await?; query(&c, &sql, &args).await })
    }

    fn execute(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(async move { let c = get_client(&self.pool).await?; execute(&c, &sql, &args).await })
    }

    fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(async move { let c = get_client(&self.pool).await?; batch(&c, &sql).await })
    }

    fn query_text(&self, sql: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>> {
        Box::pin(async move { let c = get_client(&self.pool).await?; query_text(&c, &sql).await })
    }

    fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
        Box::pin(async move {
            let client = get_client(&self.pool).await?;
            client.batch_execute("BEGIN").await.map_err(pg_err)?;
            let session = Session { client: Some(client), finished: false };
            let tx: Arc<dyn Transaction> = Arc::new(PgTx {
                session: Arc::new(Mutex::new(session)),
                savepoint: None,
                depth: 0,
                done: AtomicBool::new(false),
            });
            Ok(tx)
        })
    }
}

impl Driver for PgDriver {
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.pool.close() })
    }
}

// -- transactions ------------------------------------------------------------------------

/// The connection a transaction (and its savepoints) runs on.
struct Session {
    client: Option<Object>,
    finished: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Never hand a connection that is still inside a transaction back to the pool:
        // detach it, so it closes and the server rolls back.
        if !self.finished {
            if let Some(obj) = self.client.take() {
                drop(Object::take(obj));
            }
        }
    }
}

struct PgTx {
    session: Arc<Mutex<Session>>,
    /// `None` for the outermost transaction.
    savepoint: Option<String>,
    depth: usize,
    done: AtomicBool,
}

impl PgTx {
    /// Runs `f` on the transaction's connection, unless it already ended.
    async fn with<T>(
        &self,
        f: impl for<'c> FnOnce(&'c ClientWrapper) -> BoxFuture<'c, DbResult<T>>,
    ) -> DbResult<T> {
        if self.done.load(Ordering::Acquire) {
            return Err(closed());
        }
        let session = self.session.lock().await;
        let client = session.client.as_ref().ok_or_else(closed)?;
        f(client).await
    }

    async fn finish(&self, sql: String) -> DbResult<()> {
        if self.done.swap(true, Ordering::AcqRel) {
            return Err(closed());
        }
        let mut session = self.session.lock().await;
        let client = session.client.as_ref().ok_or_else(closed)?;
        let result = client.batch_execute(&sql).await.map_err(pg_err);
        if self.savepoint.is_none() {
            // COMMIT / ROLLBACK end the transaction even when they fail (a failed
            // COMMIT rolls back), so the connection can go back to the pool.
            session.finished = true;
            session.client = None;
        }
        result
    }
}

impl Executor for PgTx {
    fn query(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<Box<dyn RowSet>>> {
        Box::pin(async move { self.with(|c| Box::pin(async move { query(c, &sql, &args).await })).await })
    }

    fn execute(&self, sql: String, args: Vec<Value>) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(async move { self.with(|c| Box::pin(async move { execute(c, &sql, &args).await })).await })
    }

    fn batch(&self, sql: String) -> BoxFuture<'_, DbResult<u64>> {
        Box::pin(async move { self.with(|c| Box::pin(async move { batch(c, &sql).await })).await })
    }

    fn query_text(&self, sql: String) -> BoxFuture<'_, DbResult<Vec<Vec<Option<String>>>>> {
        Box::pin(async move { self.with(|c| Box::pin(async move { query_text(c, &sql).await })).await })
    }

    fn begin(&self) -> BoxFuture<'_, DbResult<Arc<dyn Transaction>>> {
        Box::pin(async move {
            let depth = self.depth + 1;
            let name = format!("orm_sp_{depth}");
            let sql = format!("SAVEPOINT {name}");
            self.with(|c| Box::pin(async move { c.batch_execute(&sql).await.map_err(pg_err) })).await?;
            let tx: Arc<dyn Transaction> = Arc::new(PgTx {
                session: self.session.clone(),
                savepoint: Some(name),
                depth,
                done: AtomicBool::new(false),
            });
            Ok(tx)
        })
    }
}

impl Transaction for PgTx {
    fn commit(&self) -> BoxFuture<'_, DbResult<()>> {
        Box::pin(async move {
            let sql = match &self.savepoint {
                Some(sp) => format!("RELEASE SAVEPOINT {sp}"),
                None => "COMMIT".to_owned(),
            };
            self.finish(sql).await
        })
    }

    fn rollback(&self) -> BoxFuture<'_, DbResult<()>> {
        Box::pin(async move {
            let sql = match &self.savepoint {
                Some(sp) => format!("ROLLBACK TO SAVEPOINT {sp}; RELEASE SAVEPOINT {sp}"),
                None => "ROLLBACK".to_owned(),
            };
            self.finish(sql).await
        })
    }
}

// -- TLS ---------------------------------------------------------------------------------

mod tls {
    use std::sync::Arc;

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
    use tokio_postgres_rustls::MakeRustlsConnect;

    use super::{DbError, DbResult};

    /// The tokio-postgres config for `url`, and the TLS connector its `sslmode` asks
    /// for (`None` for `disable`).
    pub fn parse(url: &str) -> DbResult<(tokio_postgres::Config, Option<MakeRustlsConnect>)> {
        let (base, query) = url.split_once('?').unwrap_or((url, ""));
        let mut mode = String::from("prefer");
        let mut pairs = vec![];
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            match pair.strip_prefix("sslmode=") {
                Some(m) => {
                    mode = m.to_owned();
                    // tokio-postgres knows disable / prefer / require; the stricter modes
                    // are enforced by the connector built below.
                    let m = match m {
                        "verify-ca" | "verify-full" => "require",
                        "allow" => "prefer",
                        m => m,
                    };
                    pairs.push(format!("sslmode={m}"));
                }
                None => pairs.push(pair.to_owned()),
            }
        }
        if !matches!(mode.as_str(), "disable" | "allow" | "prefer" | "require" | "verify-ca" | "verify-full") {
            return Err(DbError::other(format!("unknown sslmode {mode:?}")));
        }
        let url = if pairs.is_empty() { base.to_owned() } else { format!("{base}?{}", pairs.join("&")) };
        let config: tokio_postgres::Config =
            url.parse().map_err(|e: tokio_postgres::Error| DbError::other(format!("invalid database URL: {e}")))?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| DbError::other(e.to_string()))?;
        let tls = match mode.as_str() {
            "disable" => return Ok((config, None)),
            "allow" | "prefer" | "require" => {
                builder.dangerous().with_custom_certificate_verifier(Arc::new(NoVerify(provider))).with_no_client_auth()
            }
            "verify-ca" | "verify-full" => {
                let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
                builder.with_root_certificates(roots).with_no_client_auth()
            }
            _ => unreachable!("checked above"),
        };
        Ok((config, Some(MakeRustlsConnect::new(tls))))
    }

    /// libpq's `prefer` / `require`: encrypt, but accept any certificate. Handshake
    /// signatures are still checked.
    #[derive(Debug)]
    struct NoVerify(Arc<CryptoProvider>);

    impl ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }
}
