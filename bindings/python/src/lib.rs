//! Native engine behind the `orm` Python package.
//!
//! Frontends send the schema once (`Schema(json)`) and then one IR document per
//! operation. Every operation is one FFI crossing returning an awaitable; results come
//! back as a list of tuples built in one pass, in schema field order.

mod build;
mod convert;
mod errors;

use orm_core::{ir, migrate, schema};

use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use pyo3::IntoPyObjectExt;

use crate::build::{Builder, Classes};
use crate::convert::{py_to_value, PyParams};
use crate::errors::{db_err, engine_err, query_err, schema_err};
use orm_core::dialect::{Dialect, Target};
use orm_engine::db::{self, Driver, Executor};
use orm_engine::exec::{self, Conflict, Outcome};
use orm_engine::plan::Planner;
use orm_engine::parse_op;

/// Marker for "use the column's server default" in insert rows.
#[pyclass(frozen, module = "orm._native", name = "_Default")]
struct DefaultMarker;

#[pymethods]
impl DefaultMarker {
    fn __repr__(&self) -> &'static str {
        "DEFAULT"
    }
}

/// `rows` (sequences aligned with `fields`) as bind values of the fields' types; the
/// `DEFAULT` marker (when `defaults`) becomes `None`.
fn convert_rows(
    schema: &schema::Schema,
    model: &str,
    fields: &[String],
    rows: &Bound<'_, PyList>,
    defaults: bool,
) -> PyResult<Vec<Vec<Option<sea_query::Value>>>> {
    let types = exec::field_types(schema, model, fields).map_err(engine_err)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let mut values = Vec::with_capacity(types.len());
        for item in row.try_iter()? {
            let item = item?;
            let ty = types.get(values.len()).copied();
            values.push(if defaults && item.is_instance_of::<DefaultMarker>() {
                None
            } else {
                Some(py_to_value(&item, ty)?)
            });
        }
        if values.len() != types.len() {
            return Err(query_err("row length does not match fields".into()));
        }
        out.push(values);
    }
    Ok(out)
}

/// `update_many`'s statements (see `orm_engine::exec::plan_update_many`).
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
) -> PyResult<(Vec<sea_query::UpdateStatement>, exec::UpdateMany)> {
    let values = convert_rows(schema, model, fields, rows, false)?
        .into_iter()
        .map(|r| r.into_iter().map(|v| v.expect("no DEFAULT in update_many")).collect())
        .collect();
    let filters: Vec<ir::Expr> =
        serde_json::from_str(filters_json).map_err(|e| query_err(format!("invalid filter IR: {e}")))?;
    exec::plan_update_many(schema, target, model, fields, values, &filters, &PyParams(params), returning, batch_size)
        .map_err(engine_err)
}

/// The compiled schema. Usable without a connection, e.g. to render SQL.
#[pyclass(frozen, module = "orm._native", name = "Schema")]
struct PySchema {
    inner: Arc<schema::Schema>,
    classes: Arc<Classes>,
}

#[pymethods]
impl PySchema {
    /// `classes` maps model names to the classes queries build instances of.
    #[new]
    #[pyo3(signature = (schema_json, classes = None))]
    fn new(py: Python<'_>, schema_json: &str, classes: Option<&Bound<'_, PyDict>>) -> PyResult<Self> {
        let ir: ir::SchemaIr =
            serde_json::from_str(schema_json).map_err(|e| schema_err(format!("invalid schema IR: {e}")))?;
        let inner = schema::Schema::from_ir(ir).map_err(schema_err)?;
        let classes = match classes {
            Some(c) => Classes::new(py, &inner, c)?,
            None => Classes::empty(),
        };
        Ok(PySchema { inner: Arc::new(inner), classes: Arc::new(classes) })
    }

    /// SQL for an operation with parameters inlined. For debugging and tests only.
    fn sql(&self, op_json: &str, params: Vec<Bound<'_, PyAny>>) -> PyResult<String> {
        let op = parse_op(op_json).map_err(engine_err)?;
        let target = Target::new(Dialect::Postgres);
        let plan = Planner::plan(&self.inner, target, &op, &PyParams(&params)).map_err(engine_err)?;
        Ok(exec::sql(target, &plan))
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
    classes: Arc<Classes>,
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
        let in_tx = tx.is_some();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            exec::run_script(conn, statements, in_tx).await.map_err(engine_err)
        })
    }
}

#[pymethods]
impl Engine {
    /// Runs one query IR document. Returns, by operation: select -> a list of model
    /// instances (prefetched relations attached), or of `row_cls(values)` rows for
    /// `select(...)` columns; count -> int; exists -> bool; update / delete -> rows
    /// affected (with `returning` -> instances). Instances get `db` as `_db`.
    #[pyo3(signature = (op_json, params, tx = None, row_cls = None, db = None))]
    #[allow(clippy::too_many_arguments)]
    fn run<'py>(
        &self,
        py: Python<'py>,
        op_json: &str,
        params: Vec<Bound<'py, PyAny>>,
        tx: Option<&Bound<'py, Transaction>>,
        row_cls: Option<Bound<'py, PyAny>>,
        db: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let op = parse_op(op_json).map_err(engine_err)?;
        let target = self.target;
        let plan = Planner::plan(&self.schema, target, &op, &PyParams(&params)).map_err(engine_err)?;
        let conn = self.conn(tx);
        let classes = self.classes.clone();
        let row_cls = row_cls.map(Bound::unbind);
        let db = db.map(Bound::unbind);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let out = exec::run(conn.as_ref(), target, plan).await.map_err(engine_err)?;
            Python::attach(|py| outcome_to_py(py, out, &classes, db, row_cls))
        })
    }

    /// `INSERT ... RETURNING` every column; returns the inserted rows as instances.
    ///
    /// With `conflict` (unique field names) rows hitting that constraint update the
    /// `update` fields from the new row and apply the `set` assignments (JSON list of
    /// `{"field", "value"}` IR, parameters in `params`), or are skipped if `update` is
    /// None.
    #[pyo3(signature = (model, fields, rows, conflict = None, update = None, set = None, params = vec![], tx = None, db = None))]
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
        db: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let set: Vec<ir::Assignment> = match set {
            Some(json) => serde_json::from_str(json).map_err(|e| query_err(format!("invalid assignment IR: {e}")))?,
            None => vec![],
        };
        let conflict = conflict.map(|target| match update {
            Some(update) => Conflict::Update { target, update, set },
            None => Conflict::Nothing { target },
        });
        let values = convert_rows(&self.schema, model, &fields, rows, true)?;
        let plan = exec::plan_insert(&self.schema, self.target, model, &fields, values, conflict, &PyParams(&params))
            .map_err(engine_err)?;
        let target = self.target;
        let conn = self.conn(tx);
        let classes = self.classes.clone();
        let db = db.map(Bound::unbind);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let out = exec::run(conn.as_ref(), target, plan).await.map_err(engine_err)?;
            Python::attach(|py| outcome_to_py(py, out, &classes, db, None))
        })
    }

    /// Updates each row (a sequence aligned with `fields`, the primary key first) to its
    /// own values, among the rows matching `filters_json` (JSON list of filter IR, values
    /// in `params`). Big inputs run as several statements in one transaction (inside `tx`
    /// when given). Returns the number of rows updated, or the rows with `returning`.
    #[pyo3(signature = (model, fields, rows, filters_json, params, returning = false, batch_size = None, tx = None, db = None))]
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
        db: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let classes = self.classes.clone();
        let db = db.map(Bound::unbind);
        let (_, um) = update_many_plan(
            &self.schema, self.target, model, &fields, rows, filters_json, &params, returning, batch_size,
        )?;
        let conn = self.conn(tx);
        let own_tx = tx.is_none();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let out = exec::run_update_many(conn.as_ref(), um, own_tx).await.map_err(engine_err)?;
            Python::attach(|py| outcome_to_py(py, out, &classes, db, None))
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

/// The Python result of an operation: instances (prefetched relations attached) or
/// `row_cls` rows for a select, an int for a count or a write without `RETURNING`, a bool
/// for exists, instances for rows a write returned. Instances get `db` as `_db`.
fn outcome_to_py(
    py: Python<'_>,
    out: Outcome,
    classes: &Classes,
    db: Option<Py<PyAny>>,
    row_cls: Option<Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    let db = db.map(|d| d.into_bound(py));
    let b = Builder::new(py, classes, db.as_ref());
    match out {
        Outcome::Select(s) => {
            let row_cls = row_cls.map(|c| c.into_bound(py));
            let out = b.select(&s.plan.output, s.rows.as_ref(), &s.plan.types, &s.prefetched, row_cls.as_ref())?;
            Ok(out.unbind().into_any())
        }
        Outcome::Count(n) => n.into_py_any(py),
        Outcome::Exists(v) => v.into_py_any(py),
        Outcome::Affected(n) => n.into_py_any(py),
        Outcome::Rows { model, rows, types } => b.model_rows(model, rows.as_ref(), &types)?.into_py_any(py),
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
    let classes = schema.get().classes.clone();
    let schema = schema.get().inner.clone();
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let driver = db::connect(&url, max_connections as usize).await.map_err(db_err)?;
        let target = Target::new(driver.dialect()).without(&disable).map_err(query_err)?;
        Ok(Engine { driver, target, schema, classes })
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
