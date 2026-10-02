//! Native engine behind the `orm` Python package.
//!
//! Frontends send the schema once (`Schema(json)`) and then one IR document per
//! operation. Every operation is one FFI crossing returning an awaitable; results come
//! back as a list of tuples built in one pass, in schema field order.

mod convert;
mod ddl;
mod errors;
mod ir;
mod plan;
mod schema;

use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use pyo3::IntoPyObjectExt;
use sea_orm::sea_query::{Alias, Expr as SExpr, ExprTrait, PostgresQueryBuilder};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr,
    QueryResult, Statement, StatementBuilder, TransactionTrait,
};
use tokio::sync::Mutex;

use crate::convert::{cell_to_py, cell_to_value};
use crate::errors::{db_err, query_err};
use crate::ir::{ColType, Operation};
use crate::plan::{Plan, Planner, SelectPlan};

/// Marker for "use the column's server default" in insert rows.
#[pyclass(frozen, module = "orm._native", name = "_Default")]
struct DefaultMarker;

#[pymethods]
impl DefaultMarker {
    fn __repr__(&self) -> &'static str {
        "DEFAULT"
    }
}

fn parse_op(op_json: &str) -> PyResult<Operation> {
    serde_json::from_str(op_json).map_err(|e| query_err(format!("invalid query IR: {e}")))
}

/// The compiled schema. Usable without a connection, e.g. to render SQL.
#[pyclass(frozen, module = "orm._native", name = "Schema")]
struct PySchema {
    inner: Arc<schema::Schema>,
}

#[pymethods]
impl PySchema {
    #[new]
    fn new(schema_json: &str) -> PyResult<Self> {
        let ir: ir::SchemaIr =
            serde_json::from_str(schema_json).map_err(|e| query_err(format!("invalid schema IR: {e}")))?;
        let inner = schema::Schema::from_ir(ir).map_err(query_err)?;
        Ok(PySchema { inner: Arc::new(inner) })
    }

    /// SQL for an operation with parameters inlined. For debugging and tests only.
    fn sql(&self, op_json: &str, params: Vec<Bound<'_, PyAny>>) -> PyResult<String> {
        let op = parse_op(op_json)?;
        Ok(match Planner::plan(&self.inner, &op, &params)? {
            Plan::Select(p) => p.stmt.to_string(PostgresQueryBuilder),
            Plan::Count(s) | Plan::Exists(s) => s.to_string(PostgresQueryBuilder),
            Plan::Update(s, _) => s.to_string(PostgresQueryBuilder),
            Plan::Delete(s) => s.to_string(PostgresQueryBuilder),
        })
    }

    /// DDL for every model, in dependency order.
    fn ddl(&self) -> PyResult<Vec<String>> {
        let (tables, indexes) = ddl::create_statements(&self.inner).map_err(query_err)?;
        Ok(tables
            .iter()
            .map(|t| t.to_string(PostgresQueryBuilder))
            .chain(indexes.iter().map(|i| i.to_string(PostgresQueryBuilder)))
            .collect())
    }
}

type TxSlot = Arc<Mutex<Option<DatabaseTransaction>>>;

#[derive(Clone)]
enum Conn {
    Pool(DatabaseConnection),
    Tx(TxSlot),
}

fn closed_tx() -> DbErr {
    DbErr::Custom("transaction is already committed or rolled back".into())
}

impl Conn {
    async fn query_all(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
        match self {
            Conn::Pool(db) => db.query_all_raw(stmt).await,
            Conn::Tx(slot) => slot.lock().await.as_ref().ok_or_else(closed_tx)?.query_all_raw(stmt).await,
        }
    }

    async fn query_one(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
        match self {
            Conn::Pool(db) => db.query_one_raw(stmt).await,
            Conn::Tx(slot) => slot.lock().await.as_ref().ok_or_else(closed_tx)?.query_one_raw(stmt).await,
        }
    }

    async fn execute(&self, stmt: Statement) -> Result<u64, DbErr> {
        let r = match self {
            Conn::Pool(db) => db.execute_raw(stmt).await,
            Conn::Tx(slot) => slot.lock().await.as_ref().ok_or_else(closed_tx)?.execute_raw(stmt).await,
        };
        r.map(|r| r.rows_affected())
    }

    async fn execute_unprepared(&self, sql: &str) -> Result<u64, DbErr> {
        let r = match self {
            Conn::Pool(db) => db.execute_unprepared(sql).await,
            Conn::Tx(slot) => slot.lock().await.as_ref().ok_or_else(closed_tx)?.execute_unprepared(sql).await,
        };
        r.map(|r| r.rows_affected())
    }
}

fn build<S: StatementBuilder>(stmt: &S) -> Statement {
    DbBackend::Postgres.build(stmt)
}

fn rows_to_py<'py>(py: Python<'py>, rows: &[QueryResult], types: &[ColType]) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    let mut cells = Vec::with_capacity(types.len());
    for row in rows {
        cells.clear();
        for (i, ty) in types.iter().enumerate() {
            cells.push(cell_to_py(py, row, i, *ty)?);
        }
        list.append(PyTuple::new(py, cells.drain(..))?)?;
    }
    Ok(list)
}

struct Fetched {
    rows: Vec<QueryResult>,
    types: Vec<ColType>,
    prefetched: Vec<(String, Vec<ColType>, Vec<QueryResult>)>,
}

async fn run_select(conn: &Conn, plan: SelectPlan) -> Result<Fetched, DbErr> {
    let rows = conn.query_all(build(&plan.stmt)).await?;
    let mut prefetched = Vec::with_capacity(plan.prefetch.len());
    for p in plan.prefetch {
        let keys = rows
            .iter()
            .map(|r| cell_to_value(r, p.key_pos, p.key_type))
            .collect::<Result<Vec<_>, _>>()?;
        let related = if keys.is_empty() {
            vec![]
        } else {
            let mut stmt = p.stmt;
            stmt.and_where(
                SExpr::col((Alias::new(&p.to_table), Alias::new(&p.to_column))).is_in(keys.into_iter().map(SExpr::val)),
            );
            conn.query_all(build(&stmt)).await?
        };
        prefetched.push((p.name, p.types, related));
    }
    Ok(Fetched { rows, types: plan.types, prefetched })
}

/// A database transaction (or savepoint, when nested).
#[pyclass(frozen, module = "orm._native")]
struct Transaction {
    slot: TxSlot,
}

#[pymethods]
impl Transaction {
    fn commit<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let slot = self.slot.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let tx = slot.lock().await.take().ok_or_else(closed_tx).map_err(db_err)?;
            tx.commit().await.map_err(db_err)
        })
    }

    fn rollback<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let slot = self.slot.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let tx = slot.lock().await.take().ok_or_else(closed_tx).map_err(db_err)?;
            tx.rollback().await.map_err(db_err)
        })
    }
}

#[pyclass(frozen, module = "orm._native")]
struct Engine {
    db: DatabaseConnection,
    schema: Arc<schema::Schema>,
}

impl Engine {
    fn conn(&self, tx: Option<&Bound<'_, Transaction>>) -> Conn {
        match tx {
            Some(tx) => Conn::Tx(tx.get().slot.clone()),
            None => Conn::Pool(self.db.clone()),
        }
    }
}

#[pymethods]
impl Engine {
    /// Runs one query IR document. Returns, by operation:
    /// select -> `(rows, {relation: rows})`, count -> int, exists -> bool,
    /// update / delete -> rows affected (update with `returning` -> rows).
    #[pyo3(signature = (op_json, params, tx = None))]
    fn run<'py>(
        &self,
        py: Python<'py>,
        op_json: &str,
        params: Vec<Bound<'py, PyAny>>,
        tx: Option<&Bound<'py, Transaction>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let op = parse_op(op_json)?;
        let plan = Planner::plan(&self.schema, &op, &params)?;
        let conn = self.conn(tx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            match plan {
                Plan::Select(p) => {
                    let f = run_select(&conn, p).await.map_err(db_err)?;
                    Python::attach(|py| {
                        let rows = rows_to_py(py, &f.rows, &f.types)?;
                        let related = PyDict::new(py);
                        for (name, types, rows) in &f.prefetched {
                            related.set_item(name, rows_to_py(py, rows, types)?)?;
                        }
                        (rows, related).into_py_any(py)
                    })
                }
                Plan::Count(s) => {
                    let row = conn.query_one(build(&s)).await.map_err(db_err)?;
                    let n: i64 = match row {
                        Some(r) => r.try_get_by_index(0).map_err(db_err)?,
                        None => 0,
                    };
                    Python::attach(|py| n.into_py_any(py))
                }
                Plan::Exists(s) => {
                    let row = conn.query_one(build(&s)).await.map_err(db_err)?;
                    let b: bool = match row {
                        Some(r) => r.try_get_by_index(0).map_err(db_err)?,
                        None => false,
                    };
                    Python::attach(|py| b.into_py_any(py))
                }
                Plan::Update(s, None) => {
                    let n = conn.execute(build(&s)).await.map_err(db_err)?;
                    Python::attach(|py| n.into_py_any(py))
                }
                Plan::Update(s, Some(types)) => {
                    let rows = conn.query_all(build(&s)).await.map_err(db_err)?;
                    Python::attach(|py| rows_to_py(py, &rows, &types)?.into_py_any(py))
                }
                Plan::Delete(s) => {
                    let n = conn.execute(build(&s)).await.map_err(db_err)?;
                    Python::attach(|py| n.into_py_any(py))
                }
            }
        })
    }

    /// `INSERT ... RETURNING` every column; returns the inserted rows as tuples.
    ///
    /// With `conflict` (unique field names) rows hitting that constraint update the
    /// `update` fields from the new row, or are skipped if `update` is None.
    #[pyo3(signature = (model, fields, rows, conflict = None, update = None, tx = None))]
    #[allow(clippy::too_many_arguments)]
    fn insert<'py>(
        &self,
        py: Python<'py>,
        model: &str,
        fields: Vec<String>,
        rows: &Bound<'py, PyList>,
        conflict: Option<Vec<String>>,
        update: Option<Vec<String>>,
        tx: Option<&Bound<'py, Transaction>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let on_conflict = conflict.map(|target| match update {
            Some(update) => plan::OnConflict::Update(target, update),
            None => plan::OnConflict::Nothing(target),
        });
        let (stmt, types) = plan::plan_insert(&self.schema, model, &fields, rows, on_conflict)?;
        let conn = self.conn(tx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let rows = conn.query_all(build(&stmt)).await.map_err(db_err)?;
            Python::attach(|py| rows_to_py(py, &rows, &types).map(|l| l.unbind()))
        })
    }

    /// Starts a transaction, or a savepoint inside `tx`.
    #[pyo3(signature = (tx = None))]
    fn begin<'py>(&self, py: Python<'py>, tx: Option<&Bound<'py, Transaction>>) -> PyResult<Bound<'py, PyAny>> {
        let conn = self.conn(tx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let inner = match conn {
                Conn::Pool(db) => db.begin().await,
                Conn::Tx(slot) => slot.lock().await.as_ref().ok_or_else(closed_tx).map_err(db_err)?.begin().await,
            }
            .map_err(db_err)?;
            Ok(Transaction { slot: Arc::new(Mutex::new(Some(inner))) })
        })
    }

    /// Raw SQL escape hatch; returns rows affected.
    #[pyo3(signature = (sql, tx = None))]
    fn execute<'py>(
        &self,
        py: Python<'py>,
        sql: String,
        tx: Option<&Bound<'py, Transaction>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let conn = self.conn(tx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            conn.execute_unprepared(&sql).await.map_err(db_err)
        })
    }

    fn create_tables<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let (tables, indexes) = ddl::create_statements(&self.schema).map_err(query_err)?;
        let conn = Conn::Pool(self.db.clone());
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            for t in &tables {
                conn.execute(build(t)).await.map_err(db_err)?;
            }
            for i in &indexes {
                conn.execute(build(i)).await.map_err(db_err)?;
            }
            Ok(())
        })
    }

    fn drop_tables<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let drops = ddl::drop_statements(&self.schema).map_err(query_err)?;
        let conn = Conn::Pool(self.db.clone());
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            for d in &drops {
                conn.execute(build(d)).await.map_err(db_err)?;
            }
            Ok(())
        })
    }

    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let db = self.db.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { db.close().await.map_err(db_err) })
    }
}

/// `engine = await connect(url, schema, max_connections=10)`
#[pyfunction]
#[pyo3(signature = (url, schema, max_connections = 10))]
fn connect<'py>(
    py: Python<'py>,
    url: String,
    schema: &Bound<'py, PySchema>,
    max_connections: u32,
) -> PyResult<Bound<'py, PyAny>> {
    let schema = schema.get().inner.clone();
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let mut opts = ConnectOptions::new(url);
        opts.max_connections(max_connections).min_connections(1).sqlx_logging(false);
        let db = Database::connect(opts).await.map_err(db_err)?;
        Ok(Engine { db, schema })
    })
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add_function(wrap_pyfunction!(connect, m)?)?;
    m.add_class::<PySchema>()?;
    m.add_class::<Engine>()?;
    m.add_class::<Transaction>()?;
    m.add("DEFAULT", Py::new(py, DefaultMarker)?)?;
    m.add("DatabaseError", py.get_type::<errors::DatabaseError>())?;
    m.add("IntegrityError", py.get_type::<errors::IntegrityError>())?;
    m.add("QueryError", py.get_type::<errors::QueryError>())?;
    Ok(())
}
