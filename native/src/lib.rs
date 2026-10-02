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
use orm_core::dialect::{Dialect, Target};
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

/// Postgres accepts at most this many parameters in one statement.
const MAX_PARAMS: usize = 65_535;

/// `update_many`'s statements: `rows` (sequences aligned with `fields`, primary key
/// first) converted by column type and split so no statement exceeds `MAX_PARAMS`, or
/// `batch_size` rows.
#[allow(clippy::too_many_arguments)]
fn update_many_plan<'py>(
    schema: &schema::Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: &Bound<'py, PyList>,
    filters_json: &str,
    params: &[Bound<'py, PyAny>],
    returning: bool,
    batch_size: Option<usize>,
) -> PyResult<(Vec<sea_query::UpdateStatement>, Option<Vec<ColType>>)> {
    let m = schema.model(schema.model_idx(model).map_err(query_err)?);
    let types = fields.iter().map(|f| m.field(f).map(|f| f.ty)).collect::<Result<Vec<_>, _>>().map_err(query_err)?;
    let mut values = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let row = row
            .try_iter()?
            .zip(&types)
            .map(|(item, ty)| convert::py_to_value(&item?, Some(*ty)))
            .collect::<PyResult<Vec<_>>>()?;
        if row.len() != fields.len() {
            return Err(query_err("update_many row length does not match fields".into()));
        }
        values.push(row);
    }
    let filters: Vec<ir::Expr> =
        serde_json::from_str(filters_json).map_err(|e| query_err(format!("invalid filter IR: {e}")))?;
    let per_row = if target.caps.update_from_values { fields.len() } else { 2 * fields.len() - 1 };
    // Leave room for the filters' parameters.
    let mut chunk = MAX_PARAMS.saturating_sub(params.len()) / per_row.max(1);
    if let Some(n) = batch_size {
        chunk = chunk.min(n);
    }
    plan::plan_update_many(schema, target, model, fields, &values, chunk, &filters, params, returning)
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
        Ok(match Planner::plan(&self.inner, Target::new(d), &op, &params)? {
            Plan::Select(p) => db::to_string(d, &p.stmt),
            Plan::Count(s) | Plan::Exists(s) => db::to_string(d, &s),
            Plan::Update(s, _) => db::to_string(d, &s),
            Plan::Delete(s, _) => db::to_string(d, &s),
        })
    }

    /// The SQL of `update_many` (one statement per batch), parameters inlined. For
    /// debugging and tests; `disable` switches capabilities off as in `connect`.
    #[pyo3(signature = (model, fields, rows, filters_json = "[]", params = vec![], batch_size = None, disable = vec![]))]
    #[allow(clippy::too_many_arguments)]
    fn update_many_sql<'py>(
        &self,
        model: &str,
        fields: Vec<String>,
        rows: &Bound<'py, PyList>,
        filters_json: &str,
        params: Vec<Bound<'py, PyAny>>,
        batch_size: Option<usize>,
        disable: Vec<String>,
    ) -> PyResult<Vec<String>> {
        let target = Target::new(Dialect::Postgres).without(&disable).map_err(query_err)?;
        let (stmts, _) =
            update_many_plan(&self.inner, target, model, &fields, rows, filters_json, &params, false, batch_size)?;
        Ok(stmts.iter().map(|s| db::to_string(target.dialect, s)).collect())
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
    target: Target,
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
        let d = self.target.dialect;
        let plan = Planner::plan(&self.schema, self.target, &op, &params)?;
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
        let (stmt, types) = plan::plan_insert(&self.schema, self.target, model, &fields, rows, on_conflict, &params)?;
        let (sql, args) = db::build(self.target.dialect, &stmt);
        let conn = self.conn(tx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let rows = conn.query(sql, args).await.map_err(db_err)?;
            Python::attach(|py| rows.to_py(py, &types).map(|l| l.unbind()))
        })
    }

    /// Updates each row (a sequence aligned with `fields`, the primary key first) to its
    /// own values, among the rows matching `filters_json` (JSON list of filter IR, values
    /// in `params`). Big inputs run as several statements in one transaction (inside `tx`
    /// when given). Returns the number of rows updated, or the rows with `returning`.
    #[pyo3(signature = (model, fields, rows, filters_json, params, returning = false, batch_size = None, tx = None))]
    #[allow(clippy::too_many_arguments)]
    fn update_many<'py>(
        &self,
        py: Python<'py>,
        model: &str,
        fields: Vec<String>,
        rows: &Bound<'py, PyList>,
        filters_json: &str,
        params: Vec<Bound<'py, PyAny>>,
        returning: bool,
        batch_size: Option<usize>,
        tx: Option<&Bound<'py, Transaction>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (stmts, types) = update_many_plan(
            &self.schema, self.target, model, &fields, rows, filters_json, &params, returning, batch_size,
        )?;
        let d = self.target.dialect;
        let built: Vec<_> = stmts.iter().map(|s| db::build(d, s)).collect();
        let conn = self.conn(tx);
        let own_tx = tx.is_none() && built.len() > 1;
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let tx = if own_tx { Some(conn.begin().await.map_err(db_err)?) } else { None };
            let exec: &dyn Executor = match &tx {
                Some(t) => t.as_ref(),
                None => conn.as_ref(),
            };
            let mut count = 0u64;
            let mut fetched = vec![];
            let mut failed = None;
            for (sql, args) in built {
                let r = match &types {
                    None => exec.execute(sql, args).await.map(|n| count += n),
                    Some(_) => exec.query(sql, args).await.map(|rows| fetched.push(rows)),
                };
                if let Err(e) = r {
                    failed = Some(e);
                    break;
                }
            }
            if let Some(t) = tx {
                match failed {
                    None => t.commit().await.map_err(db_err)?,
                    Some(_) => {
                        let _ = t.rollback().await;
                    }
                }
            }
            if let Some(e) = failed {
                return Err(db_err(e));
            }
            Python::attach(|py| match &types {
                None => count.into_py_any(py),
                Some(types) => {
                    let all = PyList::empty(py);
                    for rows in &fetched {
                        for r in rows.to_py(py, types)?.iter() {
                            all.append(r)?;
                        }
                    }
                    all.into_py_any(py)
                }
            })
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
///
/// `disable` switches capabilities off (`"ilike"`, `"update_from_values"`, ...), so the
/// planner takes its fallback paths: for tests of SQL other databases will need.
#[pyfunction]
#[pyo3(signature = (url, schema, max_connections = 10, disable = vec![]))]
fn connect<'py>(
    py: Python<'py>,
    url: String,
    schema: &Bound<'py, PySchema>,
    max_connections: u32,
    disable: Vec<String>,
) -> PyResult<Bound<'py, PyAny>> {
    let schema = schema.get().inner.clone();
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let driver = db::connect(&url, max_connections as usize).await.map_err(db_err)?;
        let target = Target::new(driver.dialect()).without(&disable).map_err(query_err)?;
        Ok(Engine { driver, target, schema })
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
