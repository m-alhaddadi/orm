//! Native engine behind the `orm` Python package.
//!
//! Frontends send the schema once (`Schema(json)`) and then one IR document per
//! operation. Every operation is one FFI crossing returning an awaitable; results come
//! back as a list of tuples built in one pass, in schema field order.

mod convert;
mod db;
mod errors;
mod plan;

use orm_core::{ir, migrate, schema};

use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use pyo3::IntoPyObjectExt;
use sea_query::{Alias, Expr as SExpr, ExprTrait};

use crate::db::{DbResult, Driver, Executor, RowSet};
use crate::errors::{db_err, query_err, schema_err};
use crate::plan::{Plan, Planner, SelectPlan};
use orm_core::dialect::Dialect;
use orm_core::ir::{ColType, Operation};

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
        let d = Dialect::Postgres;
        Ok(match Planner::plan(&self.inner, d, &op, &params)? {
            Plan::Select(p) => db::to_string(d, &p.stmt),
            Plan::Count(s) | Plan::Exists(s) => db::to_string(d, &s),
            Plan::Update(s, _) => db::to_string(d, &s),
            Plan::Delete(s, _) => db::to_string(d, &s),
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

struct Fetched {
    rows: Box<dyn RowSet>,
    types: Vec<ColType>,
    prefetched: Vec<(String, Vec<ColType>, Box<dyn RowSet>)>,
}

async fn run_select(conn: &dyn Executor, dialect: Dialect, plan: SelectPlan) -> DbResult<Fetched> {
    let (sql, args) = db::build(dialect, &plan.stmt);
    let rows = conn.query(sql, args).await?;
    let mut prefetched = Vec::with_capacity(plan.prefetch.len());
    for p in plan.prefetch {
        let keys = (0..rows.len()).map(|i| rows.value(i, p.key_pos, p.key_type)).collect::<DbResult<Vec<_>>>()?;
        let related = if keys.is_empty() {
            Box::new(EmptyRows) as Box<dyn RowSet>
        } else {
            let mut stmt = p.stmt;
            stmt.and_where(
                SExpr::col((Alias::new(&p.to_table), Alias::new(&p.to_column))).is_in(keys.into_iter().map(SExpr::val)),
            );
            let (sql, args) = db::build(dialect, &stmt);
            conn.query(sql, args).await?
        };
        prefetched.push((p.name, p.types, related));
    }
    Ok(Fetched { rows, types: plan.types, prefetched })
}

/// The result of a prefetch with no keys: no query runs.
struct EmptyRows;

impl RowSet for EmptyRows {
    fn len(&self) -> usize {
        0
    }
    fn to_py<'py>(&self, py: Python<'py>, _: &[ColType]) -> PyResult<Bound<'py, PyList>> {
        Ok(PyList::empty(py))
    }
    fn value(&self, _: usize, _: usize, _: ColType) -> DbResult<sea_query::Value> {
        Err(db::DbError::other("no rows"))
    }
    fn get_i64(&self, _: usize, _: usize) -> DbResult<i64> {
        Err(db::DbError::other("no rows"))
    }
    fn get_bool(&self, _: usize, _: usize) -> DbResult<bool> {
        Err(db::DbError::other("no rows"))
    }
}

/// A database transaction (or savepoint, when nested).
#[pyclass(frozen, module = "orm._native")]
struct Transaction {
    inner: Arc<dyn db::Transaction>,
}

#[pymethods]
impl Transaction {
    fn commit<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { tx.commit().await.map_err(db_err) })
    }

    fn rollback<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let tx = self.inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { tx.rollback().await.map_err(db_err) })
    }
}

#[pyclass(frozen, module = "orm._native")]
struct Engine {
    driver: Arc<dyn Driver>,
    dialect: Dialect,
    schema: Arc<schema::Schema>,
}

impl Engine {
    fn conn(&self, tx: Option<&Bound<'_, Transaction>>) -> Arc<dyn Executor> {
        match tx {
            Some(tx) => tx.get().inner.clone(),
            None => self.driver.clone(),
        }
    }

    /// Runs `statements` in order, in one transaction (Postgres DDL is transactional),
    /// or inside `tx`.
    fn run_script<'py>(
        &self,
        py: Python<'py>,
        statements: Vec<String>,
        tx: Option<&Bound<'py, Transaction>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let conn = self.conn(tx);
        let own_tx = tx.is_none();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            if !own_tx {
                for s in statements {
                    conn.batch(s).await.map_err(db_err)?;
                }
                return Ok(());
            }
            let tx = conn.begin().await.map_err(db_err)?;
            for s in statements {
                if let Err(e) = tx.batch(s).await {
                    let _ = tx.rollback().await;
                    return Err(db_err(e));
                }
            }
            tx.commit().await.map_err(db_err)
        })
    }
}

#[pymethods]
impl Engine {
    /// Runs one query IR document. Returns, by operation:
    /// select -> `(rows, {relation: rows})`, count -> int, exists -> bool,
    /// update / delete -> rows affected (with `returning` -> rows).
    #[pyo3(signature = (op_json, params, tx = None))]
    fn run<'py>(
        &self,
        py: Python<'py>,
        op_json: &str,
        params: Vec<Bound<'py, PyAny>>,
        tx: Option<&Bound<'py, Transaction>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let op = parse_op(op_json)?;
        let d = self.dialect;
        let plan = Planner::plan(&self.schema, d, &op, &params)?;
        let conn = self.conn(tx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let conn = conn.as_ref();
            match plan {
                Plan::Select(p) => {
                    let f = run_select(conn, d, p).await.map_err(db_err)?;
                    Python::attach(|py| {
                        let rows = f.rows.to_py(py, &f.types)?;
                        let related = PyDict::new(py);
                        for (name, types, rows) in &f.prefetched {
                            related.set_item(name, rows.to_py(py, types)?)?;
                        }
                        (rows, related).into_py_any(py)
                    })
                }
                Plan::Count(s) => {
                    let (sql, args) = db::build(d, &s);
                    let rows = conn.query(sql, args).await.map_err(db_err)?;
                    let n = if rows.len() == 0 { 0 } else { rows.get_i64(0, 0).map_err(db_err)? };
                    Python::attach(|py| n.into_py_any(py))
                }
                Plan::Exists(s) => {
                    let (sql, args) = db::build(d, &s);
                    let rows = conn.query(sql, args).await.map_err(db_err)?;
                    let b = rows.len() > 0 && rows.get_bool(0, 0).map_err(db_err)?;
                    Python::attach(|py| b.into_py_any(py))
                }
                Plan::Update(s, types) => {
                    let (sql, args) = db::build(d, &s);
                    count_or_rows(conn, sql, args, types).await
                }
                Plan::Delete(s, types) => {
                    let (sql, args) = db::build(d, &s);
                    count_or_rows(conn, sql, args, types).await
                }
            }
        })
    }

    /// `INSERT ... RETURNING` every column; returns the inserted rows as tuples.
    ///
    /// With `conflict` (unique field names) rows hitting that constraint update the
    /// `update` fields from the new row and apply the `set` assignments (JSON list of
    /// `{"field", "value"}` IR, parameters in `params`), or are skipped if `update` is
    /// None.
    #[pyo3(signature = (model, fields, rows, conflict = None, update = None, set = None, params = vec![], tx = None))]
    #[allow(clippy::too_many_arguments)]
    fn insert<'py>(
        &self,
        py: Python<'py>,
        model: &str,
        fields: Vec<String>,
        rows: &Bound<'py, PyList>,
        conflict: Option<Vec<String>>,
        update: Option<Vec<String>>,
        set: Option<&str>,
        params: Vec<Bound<'py, PyAny>>,
        tx: Option<&Bound<'py, Transaction>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let set: Vec<ir::Assignment> = match set {
            Some(json) => serde_json::from_str(json).map_err(|e| query_err(format!("invalid assignment IR: {e}")))?,
            None => vec![],
        };
        let on_conflict = conflict.map(|target| match update {
            Some(update) => plan::OnConflict::Update(target, update, set),
            None => plan::OnConflict::Nothing(target),
        });
        let (stmt, types) = plan::plan_insert(&self.schema, self.dialect, model, &fields, rows, on_conflict, &params)?;
        let (sql, args) = db::build(self.dialect, &stmt);
        let conn = self.conn(tx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let rows = conn.query(sql, args).await.map_err(db_err)?;
            Python::attach(|py| rows.to_py(py, &types).map(|l| l.unbind()))
        })
    }

    /// Starts a transaction, or a savepoint inside `tx`.
    #[pyo3(signature = (tx = None))]
    fn begin<'py>(&self, py: Python<'py>, tx: Option<&Bound<'py, Transaction>>) -> PyResult<Bound<'py, PyAny>> {
        let conn = self.conn(tx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let inner = conn.begin().await.map_err(db_err)?;
            Ok(Transaction { inner })
        })
    }

    /// Raw SQL escape hatch (one or more statements); returns rows affected.
    #[pyo3(signature = (sql, tx = None))]
    fn execute<'py>(
        &self,
        py: Python<'py>,
        sql: String,
        tx: Option<&Bound<'py, Transaction>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let conn = self.conn(tx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move { conn.batch(sql).await.map_err(db_err) })
    }

    /// Raw query returning rows of text: every selected column is read as text.
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
            let out = conn.query_text(sql).await.map_err(db_err)?;
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
        self.run_script(py, stmts, None)
    }

    fn drop_tables<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stmts = migrate::drop_all(&self.schema).map_err(schema_err)?;
        self.run_script(py, stmts, None)
    }

    /// Runs SQL statements in order, in one transaction (Postgres DDL is transactional).
    #[pyo3(signature = (statements, tx = None))]
    fn execute_script<'py>(
        &self,
        py: Python<'py>,
        statements: Vec<String>,
        tx: Option<&Bound<'py, Transaction>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.run_script(py, statements, tx)
    }

    fn close<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let driver = self.driver.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            driver.close().await;
            Ok(())
        })
    }
}

/// An UPDATE / DELETE: the row count, or the rows when it has `RETURNING`.
async fn count_or_rows(
    conn: &dyn Executor,
    sql: String,
    args: Vec<sea_query::Value>,
    types: Option<Vec<ColType>>,
) -> PyResult<Py<PyAny>> {
    match types {
        None => {
            let n = conn.execute(sql, args).await.map_err(db_err)?;
            Python::attach(|py| n.into_py_any(py))
        }
        Some(types) => {
            let rows = conn.query(sql, args).await.map_err(db_err)?;
            Python::attach(|py| rows.to_py(py, &types)?.into_py_any(py))
        }
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
        let driver = db::connect(&url, max_connections as usize).await.map_err(db_err)?;
        let dialect = driver.dialect();
        Ok(Engine { driver, dialect, schema })
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
    m.add("LockNotAvailable", py.get_type::<errors::LockNotAvailable>())?;
    m.add("QueryError", py.get_type::<errors::QueryError>())?;
    m.add("SchemaError", py.get_type::<errors::SchemaError>())?;
    Ok(())
}
