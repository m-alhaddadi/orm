//! Native engine behind the `orm` Python package.
//!
//! Frontends send the schema once (`Schema(json)`) and then one IR document per
//! operation. Every operation is one FFI crossing returning an awaitable; results come
//! back as a list of tuples built in one pass, in schema field order.

mod convert;
mod errors;
mod plan;

use orm_core::{ir, migrate, schema};

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
use crate::errors::{db_err, query_err, schema_err};
use orm_core::ir::{ColType, Operation};
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
            serde_json::from_str(schema_json).map_err(|e| schema_err(format!("invalid schema IR: {e}")))?;
        let inner = schema::Schema::from_ir(ir).map_err(schema_err)?;
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

    /// Idempotent DDL for the whole schema, in dependency order.
    fn ddl(&self) -> PyResult<Vec<String>> {
        migrate::create_all(&self.inner).map_err(schema_err)
    }

    /// The database schema as a snapshot (JSON), the format migrations store.
    fn snapshot(&self) -> PyResult<String> {
        let s = migrate::snapshot(&self.inner).map_err(schema_err)?;
        serde_json::to_string_pretty(&s).map_err(|e| schema_err(e.to_string()))
    }

    /// The next migration for the migrations directory `dir`, as JSON (see
    /// `migration`), without writing anything.
    fn plan_migration(&self, dir: &str) -> PyResult<String> {
        let plan = migrate::files::next(std::path::Path::new(dir), &self.inner).map_err(schema_err)?;
        serde_json::to_string(&plan).map_err(|e| schema_err(e.to_string()))
    }

    /// Writes the next migration into `dir`; returns its folder name, or None when
    /// nothing changed (and not `empty`).
    #[pyo3(signature = (dir, name = None, empty = false))]
    fn make_migration(&self, dir: &str, name: Option<&str>, empty: bool) -> PyResult<Option<String>> {
        let made = migrate::files::make(std::path::Path::new(dir), &self.inner, name, empty).map_err(schema_err)?;
        Ok(made.map(|(folder, _)| folder.name))
    }

    /// The migration from `previous` (a snapshot; `None` for an empty database) to this
    /// schema, as JSON: `{"up": [step], "down": [step], "snapshot": {...}}` where a
    /// step is `{"summary", "sql", "warning"?}`.
    #[pyo3(signature = (previous = None))]
    fn migration(&self, previous: Option<&str>) -> PyResult<String> {
        let previous = match previous {
            Some(json) => migrate::parse_snapshot(json).map_err(schema_err)?,
            None => migrate::DbSchema::default(),
        };
        let plan = migrate::plan(&self.inner, &previous).map_err(schema_err)?;
        serde_json::to_string(&plan).map_err(|e| schema_err(e.to_string()))
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
    fn run_script<'py>(&self, py: Python<'py>, statements: Vec<String>) -> PyResult<Bound<'py, PyAny>> {
        let db = self.db.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let tx = db.begin().await.map_err(db_err)?;
            for s in &statements {
                tx.execute_unprepared(s).await.map_err(db_err)?;
            }
            tx.commit().await.map_err(db_err)
        })
    }

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

    /// Raw query returning rows of text: every selected column must be text (cast it).
    /// For tooling such as the migration runner, not for application queries.
    #[pyo3(signature = (sql, tx = None))]
    fn fetch_text<'py>(
        &self,
        py: Python<'py>,
        sql: String,
        tx: Option<&Bound<'py, Transaction>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let conn = self.conn(tx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let rows = conn.query_all(Statement::from_string(DbBackend::Postgres, sql)).await.map_err(db_err)?;
            let mut out: Vec<Vec<Option<String>>> = Vec::with_capacity(rows.len());
            for r in &rows {
                let n = r.column_names().len();
                out.push((0..n).map(|i| r.try_get_by_index::<Option<String>>(i)).collect::<Result<_, _>>().map_err(db_err)?);
            }
            Python::attach(|py| {
                let list = PyList::empty(py);
                for row in out {
                    list.append(PyTuple::new(py, row)?)?;
                }
                list.into_py_any(py)
            })
        })
    }

    fn create_tables<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stmts = migrate::create_all(&self.schema).map_err(schema_err)?;
        self.run_script(py, stmts)
    }

    fn drop_tables<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stmts = migrate::drop_all(&self.schema).map_err(schema_err)?;
        self.run_script(py, stmts)
    }

    /// Runs SQL statements in order, in one transaction (Postgres DDL is transactional).
    #[pyo3(signature = (statements, tx = None))]
    fn execute_script<'py>(
        &self,
        py: Python<'py>,
        statements: Vec<String>,
        tx: Option<&Bound<'py, Transaction>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        match tx {
            Some(tx) => {
                let conn = Conn::Tx(tx.get().slot.clone());
                pyo3_async_runtimes::tokio::future_into_py(py, async move {
                    for s in &statements {
                        conn.execute_unprepared(s).await.map_err(db_err)?;
                    }
                    Ok(())
                })
            }
            None => self.run_script(py, statements),
        }
    }

    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let db = self.db.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { db.close().await.map_err(db_err) })
    }
}

/// Compiles schema-language source to the schema IR (JSON). `path` is where it came
/// from, for error messages and `import` resolution.
#[pyfunction]
#[pyo3(signature = (source, path = None))]
fn compile_schema(source: &str, path: Option<&str>) -> PyResult<String> {
    let ir = orm_core::dsl::compile(source, path.map(std::path::Path::new)).map_err(schema_err)?;
    let (ir, _) = orm_core::dsl::check(ir).map_err(schema_err)?;
    serde_json::to_string(&ir).map_err(|e| schema_err(e.to_string()))
}

#[pyfunction]
fn compile_schema_file(path: &str) -> PyResult<String> {
    let ir = orm_core::dsl::compile_file(std::path::Path::new(path)).map_err(schema_err)?;
    let (ir, _) = orm_core::dsl::check(ir).map_err(schema_err)?;
    serde_json::to_string(&ir).map_err(|e| schema_err(e.to_string()))
}

/// `(models.py, models.pyi)` source for a schema file.
#[pyfunction]
fn generate_python(path: &str) -> PyResult<(String, String)> {
    let p = std::path::Path::new(path);
    let (ir, schema) =
        orm_core::dsl::check(orm_core::dsl::compile_file(p).map_err(schema_err)?).map_err(schema_err)?;
    let source = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let g = orm_core::codegen::python::generate(&ir, &schema, &source).map_err(schema_err)?;
    Ok((g.module, g.stub))
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
    m.add_function(wrap_pyfunction!(compile_schema, m)?)?;
    m.add_function(wrap_pyfunction!(compile_schema_file, m)?)?;
    m.add_function(wrap_pyfunction!(generate_python, m)?)?;
    m.add_class::<PySchema>()?;
    m.add_class::<Engine>()?;
    m.add_class::<Transaction>()?;
    m.add("DEFAULT", Py::new(py, DefaultMarker)?)?;
    m.add("DatabaseError", py.get_type::<errors::DatabaseError>())?;
    m.add("IntegrityError", py.get_type::<errors::IntegrityError>())?;
    m.add("QueryError", py.get_type::<errors::QueryError>())?;
    m.add("SchemaError", py.get_type::<errors::SchemaError>())?;
    Ok(())
}
