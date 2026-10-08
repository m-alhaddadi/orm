//! Native engine behind the `orm` Python package.
//!
//! Frontends send the schema once (`Schema(json)`) and then one IR document per
//! operation. Every operation is one FFI crossing returning an awaitable; results come
//! back as a list of tuples built in one pass, in schema field order.

mod build;
mod convert;
mod errors;
#[cfg(feature = "composition")]
mod methods { include!(env!("ORM_PYTHON_METHODS")); }

#[cfg(any(
    all(feature = "profile-postgres", feature = "profile-sqlite"),
    all(feature = "profile-postgres", feature = "profile-combined"),
    all(feature = "profile-postgres", feature = "profile-tooling"),
    all(feature = "profile-sqlite", feature = "profile-combined"),
    all(feature = "profile-sqlite", feature = "profile-tooling"),
    all(feature = "profile-combined", feature = "profile-tooling")
))]
compile_error!("select exactly one named native profile; use a custom feature build for exact combinations");

use orm_core::{ir, migrate, schema};

use std::path::PathBuf;
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyTuple};
use pyo3::IntoPyObjectExt;

use crate::build::{Builder, Classes};
use crate::convert::{cell_to_py, py_to_value, PyParams};
use crate::errors::{db_err, engine_err, query_err, schema_err};
use orm_core::dialect::Target;
use orm_engine::db::{self, Driver, Executor};
use orm_engine::exec::{self, Conflict, Outcome};
use orm_engine::plan::{self, Planner};
use orm_engine::migrate as engine_migrate;
use orm_engine::{parse_op, protect};
use orm_core::ir::Operation;

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
    without_defaults: bool,
) -> PyResult<(Vec<sea_query::UpdateStatement>, exec::UpdateMany)> {
    let values = convert_rows(schema, model, fields, rows, false)?
        .into_iter()
        .map(|r| r.into_iter().map(|v| v.expect("no DEFAULT in update_many")).collect())
        .collect();
    let (filters, scope) = orm_engine::params::scoped_filters(filters_json).map_err(engine_err)?;
    let params = orm_engine::params::Scoped { params: &PyParams(params), scope: &scope };
    exec::plan_update_many(schema, target, model, fields, values, &filters, &params, returning, batch_size, without_defaults)
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
        let mut ir: ir::SchemaIr =
            ir::SchemaIr::from_json(schema_json).map_err(schema_err)?;
        orm_core::behavior::prepare(&mut ir, Some("python")).map_err(schema_err)?;
        let inner = schema::Schema::from_ir(ir).map_err(schema_err)?;
        db::require_dialect(inner.dialect).map_err(db_err)?;
        let classes = match classes {
            Some(c) => Classes::new(py, &inner, c)?,
            None => Classes::empty(),
        };
        Ok(PySchema { inner: Arc::new(inner), classes: Arc::new(classes) })
    }

    /// Converts and plans a single-row insert without SQL or I/O (`orm.hooks.prepare_insert`).
    /// `allowed`: the write protection check runs here too, before a caller's own I/O.
    fn validate_insert(&self, model: &str, fields: Vec<String>, rows: &Bound<'_, PyList>, allowed: Vec<String>) -> PyResult<()> {
        protect::ensure_writable(&self.inner, model, &allowed).map_err(engine_err)?;
        let values = convert_rows(&self.inner, model, &fields, rows, true)?;
        exec::plan_insert(&self.inner, Target::new(self.inner.dialect), model, &fields, values, None, &orm_engine::NoParams).map_err(engine_err)?;
        Ok(())
    }

    /// Plans an update without SQL or I/O; true when its filters pin one row by a
    /// non-null primary key or unique field (`orm.hooks.prepare_update`).
    fn unique_row_update(&self, op_json: &str, params: Vec<Bound<'_, PyAny>>, allowed: Vec<String>) -> PyResult<bool> {
        let op = parse_op(op_json).map_err(engine_err)?;
        if let Operation::Update(ir::Update { model, .. }) = &op {
            protect::ensure_writable(&self.inner, model, &allowed).map_err(engine_err)?;
        }
        plan::unique_row_update(&self.inner, Target::new(self.inner.dialect), &op, &PyParams(&params)).map_err(engine_err)
    }

    /// SQL for an operation with parameters inlined. For debugging and tests only.
    fn sql(&self, op_json: &str, params: Vec<Bound<'_, PyAny>>) -> PyResult<String> {
        let op = parse_op(op_json).map_err(engine_err)?;
        let target = Target::new(self.inner.dialect);
        let plan = Planner::plan(&self.inner, target, &op, &PyParams(&params)).map_err(engine_err)?;
        Ok(exec::sql(target, &plan))
    }

    /// SQL for an operation with its parameter placeholders: the statement shape.
    fn statement(&self, op_json: &str, params: Vec<Bound<'_, PyAny>>) -> PyResult<String> {
        let op = parse_op(op_json).map_err(engine_err)?;
        let target = Target::new(self.inner.dialect);
        let plan = Planner::plan(&self.inner, target, &op, &PyParams(&params)).map_err(engine_err)?;
        Ok(exec::statement(target, &plan))
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
        let target = Target::new(self.inner.dialect).without(&disable).map_err(query_err)?;
        let (stmts, _) =
            update_many_plan(&self.inner, target, model, &fields, rows, filters_json, &params, false, batch_size, false)?;
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

/// The statements of the engine calls it is passed to, drained by `take()`.
#[pyclass(frozen, module = "orm._native")]
struct Trace(orm_engine::trace::Trace);

#[pymethods]
impl Trace {
    #[new]
    fn new() -> Self {
        Trace(orm_engine::trace::Trace::new())
    }

    /// `(sql, start, duration, rows, error)` per statement, oldest first: `start` in Unix
    /// seconds, `duration` in seconds. The trace is empty afterwards.
    #[allow(clippy::type_complexity)]
    fn take(&self) -> Vec<(String, f64, f64, u64, Option<String>)> {
        self.0
            .take()
            .into_iter()
            .map(|e| {
                let start = e.start.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64();
                (e.sql, start, e.duration.as_secs_f64(), e.rows, e.error)
            })
            .collect()
    }
}

/// A held session advisory lock (`db.lock(..., session=True)`).
#[pyclass(frozen, module = "orm._native")]
struct SessionLock {
    inner: Arc<dyn db::SessionLock>,
    unlock: String,
}

#[pymethods]
impl SessionLock {
    #[pyo3(signature = (trace = None))]
    fn release<'py>(&self, py: Python<'py>, trace: Option<&Bound<'py, Trace>>) -> PyResult<Bound<'py, PyAny>> {
        let lock = self.inner.clone();
        let (sql, trace) = (self.unlock.clone(), trace.map(|t| t.get().0.clone()));
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            orm_engine::trace::timed(trace.as_ref(), sql, lock.release(), |_| 1).await.map_err(db_err)
        })
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

    /// `conn(tx)`, recording each statement into `trace` when given.
    fn traced(&self, tx: Option<&Bound<'_, Transaction>>, trace: Option<&Bound<'_, Trace>>) -> Arc<dyn Executor> {
        orm_engine::trace::wrap(self.conn(tx), trace.map(|t| &t.get().0))
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
    /// `allowed` names the models the current `allow_writes` scope allows to write.
    #[pyo3(signature = (op_json, params, tx = None, row_cls = None, db = None, allowed = vec![], trace = None))]
    #[allow(clippy::too_many_arguments)]
    fn run<'py>(
        &self,
        py: Python<'py>,
        op_json: &str,
        params: Vec<Bound<'py, PyAny>>,
        tx: Option<&Bound<'py, Transaction>>,
        row_cls: Option<Bound<'py, PyAny>>,
        db: Option<Bound<'py, PyAny>>,
        allowed: Vec<String>,
        trace: Option<&Bound<'py, Trace>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let op = parse_op(op_json).map_err(engine_err)?;
        if let Operation::Update(ir::Update { model, .. }) | Operation::Delete(ir::Delete { model, .. }) = &op {
            protect::ensure_writable(&self.schema, model, &allowed).map_err(engine_err)?;
        }
        let target = self.target;
        let plan = Planner::plan(&self.schema, target, &op, &PyParams(&params)).map_err(engine_err)?;
        let conn = self.traced(tx, trace);
        let classes = self.classes.clone();
        let row_cls = row_cls.map(Bound::unbind);
        let db = db.map(Bound::unbind);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let out = exec::run(conn.as_ref(), target, plan).await.map_err(engine_err)?;
            Python::attach(|py| outcome_to_py(py, out, &classes, db, row_cls))
        })
    }

    /// Loads the prefetches of a select IR onto `parents`, instances the caller has, with
    /// only the prefetch queries: `keys` names the root fields they read, `rows` holds
    /// those values per parent. Related instances get `db` as `_db`.
    #[pyo3(signature = (op_json, params, keys, rows, parents, tx = None, db = None))]
    #[allow(clippy::too_many_arguments)]
    fn prefetch<'py>(
        &self,
        py: Python<'py>,
        op_json: &str,
        params: Vec<Bound<'py, PyAny>>,
        keys: Vec<String>,
        rows: Vec<Vec<Bound<'py, PyAny>>>,
        parents: Vec<Bound<'py, PyAny>>,
        tx: Option<&Bound<'py, Transaction>>,
        db: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let Operation::Select(q) = parse_op(op_json).map_err(engine_err)? else {
            return Err(query_err("prefetch() takes a select".into()));
        };
        let target = self.target;
        let plans = plan::plan_prefetch_only(&self.schema, target, &q, &PyParams(&params)).map_err(engine_err)?;
        let values = key_rows(&self.schema, &q.model, &keys, rows.len(), |r, i, ty| py_to_value(&rows[r][i], Some(ty)))?;
        let parents: Vec<Py<PyAny>> = parents.into_iter().map(Bound::unbind).collect();
        let conn = self.conn(tx);
        let classes = self.classes.clone();
        let db = db.map(Bound::unbind);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let out = exec::prefetch(conn.as_ref(), target, Box::new(exec::ValueRows(values)), plans).await.map_err(engine_err)?;
            Python::attach(|py| prefetched_to_py(py, out, &classes, db, parents))
        })
    }

    /// `INSERT ... RETURNING` every column; returns the inserted rows as instances, or
    /// without `returning` the affected-row count.
    ///
    /// With `conflict` (unique field names) rows hitting that constraint update the
    /// `update` fields from the new row and apply the `set` assignments (JSON list of
    /// `{"field", "value"}` IR, parameters in `params`), or are skipped if `update` is
    /// None. `conflict_where` (condition IR, parameters in `params`) is the predicate of a
    /// partial unique index.
    /// Rows beyond the parameter limit, or beyond `batch_size`, go to further statements
    /// in one transaction (inside `tx` when given).
    #[pyo3(signature = (model, fields, rows, conflict = None, update = None, set = None, params = vec![], tx = None, db = None, allowed = vec![], batch_size = None, conflict_where = None, trace = None, returning = true))]
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
        allowed: Vec<String>,
        batch_size: Option<usize>,
        conflict_where: Option<&str>,
        trace: Option<&Bound<'py, Trace>>,
        returning: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        protect::ensure_writable(&self.schema, model, &allowed).map_err(engine_err)?;
        let set: Vec<ir::Assignment> = match set {
            Some(json) => serde_json::from_str(json).map_err(|e| query_err(format!("invalid assignment IR: {e}")))?,
            None => vec![],
        };
        let filter: Option<ir::Expr> = match conflict_where {
            Some(json) => Some(serde_json::from_str(json).map_err(|e| query_err(format!("invalid condition IR: {e}")))?),
            None => None,
        };
        let conflict = conflict.map(|target| match update {
            Some(update) => Conflict::Update { target, filter, update, set },
            None => Conflict::Nothing { target, filter },
        });
        let values = convert_rows(&self.schema, model, &fields, rows, true)?;
        let plans =
            exec::plan_inserts(&self.schema, self.target, model, &fields, values, conflict, &PyParams(&params), batch_size, returning)
                .map_err(engine_err)?;
        let target = self.target;
        let conn = self.traced(tx, trace);
        let own_tx = tx.is_none();
        let classes = self.classes.clone();
        let db = db.map(Bound::unbind);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let out = exec::run_inserts(conn.as_ref(), target, plans, own_tx, returning).await.map_err(engine_err)?;
            Python::attach(|py| outcome_to_py(py, out, &classes, db, None))
        })
    }

    /// Bulk load with Postgres `COPY` (`insert_many(rows, copy=True)`); returns the number
    /// of rows written. `rows` are sequences aligned with `fields`.
    #[pyo3(signature = (model, fields, rows, tx = None, allowed = vec![]))]
    fn copy_insert<'py>(
        &self,
        py: Python<'py>,
        model: &str,
        fields: Vec<String>,
        rows: &Bound<'py, PyList>,
        tx: Option<&Bound<'py, Transaction>>,
        allowed: Vec<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        protect::ensure_writable(&self.schema, model, &allowed).map_err(engine_err)?;
        let values = convert_rows(&self.schema, model, &fields, rows, true)?;
        let copy = exec::plan_copy(&self.schema, self.target, model, &fields, values).map_err(engine_err)?;
        let conn = self.conn(tx);
        pyo3_async_runtimes::tokio::future_into_py(py, async move { exec::run_copy(conn.as_ref(), copy).await.map_err(engine_err) })
    }

    /// Attach local values to an existing shared-key parent.
    #[cfg(feature = "model-composition")]
    #[pyo3(signature = (model, parent_id, fields, rows, tx = None, db = None, allowed = vec![], trace = None))]
    #[allow(clippy::too_many_arguments)]
    fn attach<'py>(&self, py: Python<'py>, model: &str, parent_id: &Bound<'py, PyAny>, fields: Vec<String>, rows: &Bound<'py, PyList>, tx: Option<&Bound<'py, Transaction>>, db: Option<Bound<'py, PyAny>>, allowed: Vec<String>, trace: Option<&Bound<'py, Trace>>) -> PyResult<Bound<'py, PyAny>> {
        protect::ensure_writable(&self.schema, model, &allowed).map_err(engine_err)?;
        let model_idx = self.schema.model_idx(model).map_err(schema_err)?;
        let identity = py_to_value(parent_id, Some(self.schema.model(model_idx).pk_field().value_type()))?;
        let values = convert_rows(&self.schema, model, &fields, rows, true)?;
        let plan = orm_engine::composed::prepare_attach(&self.schema, self.target, model, identity, &fields, values).map_err(engine_err)?;
        let target = self.target;
        let conn = self.traced(tx, trace);
        let classes = self.classes.clone();
        let db = db.map(Bound::unbind);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let out = orm_engine::composed::run_insert(conn.as_ref(), target, plan).await.map_err(engine_err)?;
            Python::attach(|py| outcome_to_py(py, out, &classes, db, None))
        })
    }

    /// Updates each row (a sequence aligned with `fields`, the primary key first) to its
    /// own values, among the rows matching `filters_json` (JSON list of filter IR, values
    /// in `params`). Big inputs run as several statements in one transaction (inside `tx`
    /// when given). Returns the number of rows updated, or the rows with `returning`.
    #[pyo3(signature = (model, fields, rows, filters_json, params, returning = false, batch_size = None, tx = None, db = None, without_defaults = false, allowed = vec![], trace = None))]
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
        without_defaults: bool,
        allowed: Vec<String>,
        trace: Option<&Bound<'py, Trace>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        protect::ensure_writable(&self.schema, model, &allowed).map_err(engine_err)?;
        let classes = self.classes.clone();
        let db = db.map(Bound::unbind);
        let (_, um) = update_many_plan(
            &self.schema, self.target, model, &fields, rows, filters_json, &params, returning, batch_size, without_defaults,
        )?;
        let conn = self.traced(tx, trace);
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

    /// Advisory lock on a validated integer key or UTF-8 name, returning a boolean.
    #[pyo3(signature = (key, name, exclusive, nowait, tx, trace = None))]
    #[allow(clippy::too_many_arguments)]
    fn advisory_lock<'py>(
        &self, py: Python<'py>, key: i64, name: Option<&[u8]>,
        exclusive: bool, nowait: bool, tx: &Bound<'py, Transaction>, trace: Option<&Bound<'py, Trace>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let key = name.map(orm_engine::advisory::key).unwrap_or(key);
        let conn = self.traced(Some(tx), trace);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            conn.advisory_lock(key, exclusive, nowait).await.map_err(db_err)
        })
    }

    /// This engine on the same pool, with `set_config(name, value, true)` for each
    /// setting at the start of every transaction (statements outside one get their own).
    fn with_settings(&self, names: Vec<String>, values: Vec<String>) -> Engine {
        let driver: Arc<dyn Driver> = Arc::new(db::WithSettings::new(self.driver.clone(), names.into_iter().zip(values).collect()));
        Engine { driver, target: self.target, schema: self.schema.clone(), classes: self.classes.clone() }
    }

    /// Session advisory lock on a pinned connection; `None` when it is not taken.
    #[pyo3(signature = (key, name, exclusive, nowait, timeout_ms = None, trace = None))]
    #[allow(clippy::too_many_arguments)]
    fn session_lock<'py>(
        &self, py: Python<'py>, key: i64, name: Option<&[u8]>,
        exclusive: bool, nowait: bool, timeout_ms: Option<u64>, trace: Option<&Bound<'py, Trace>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let key = name.map(orm_engine::advisory::key).unwrap_or(key);
        let driver = self.driver.clone();
        let trace = trace.map(|t| t.get().0.clone());
        let sql = orm_engine::advisory::session_sql(key, exclusive, nowait);
        let unlock = orm_engine::advisory::unlock_sql(key, exclusive);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let taken = driver.session_lock(key, exclusive, nowait, timeout_ms);
            let lock = orm_engine::trace::timed(trace.as_ref(), sql, taken, |l| l.is_some() as u64).await.map_err(db_err)?;
            Ok(lock.map(|inner| SessionLock { inner, unlock }))
        })
    }

    /// Raw SQL escape hatch (one or more statements); returns rows affected.
    #[pyo3(signature = (sql, tx = None, trace = None))]
    fn execute<'py>(
        &self,
        py: Python<'py>,
        sql: String,
        tx: Option<&Bound<'py, Transaction>>,
        trace: Option<&Bound<'py, Trace>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let conn = self.traced(tx, trace);
        pyo3_async_runtimes::tokio::future_into_py(py, async move { conn.batch(sql).await.map_err(db_err) })
    }

    /// The query plan of a read IR document (see `exec::explain`), as text.
    #[pyo3(signature = (op_json, params, analyze = false, tx = None, trace = None))]
    #[allow(clippy::too_many_arguments)]
    fn explain<'py>(
        &self,
        py: Python<'py>,
        op_json: &str,
        params: Vec<Bound<'py, PyAny>>,
        analyze: bool,
        tx: Option<&Bound<'py, Transaction>>,
        trace: Option<&Bound<'py, Trace>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let op = parse_op(op_json).map_err(engine_err)?;
        let target = self.target;
        let plan = Planner::plan(&self.schema, target, &op, &PyParams(&params)).map_err(engine_err)?;
        let conn = self.traced(tx, trace);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            exec::explain(conn.as_ref(), target, &plan, analyze).await.map_err(engine_err)
        })
    }

    /// Raw SQL with parameters (types taken from the values); a list of dicts by
    /// column name, cells typed by the database.
    #[pyo3(signature = (sql, params, tx = None, trace = None))]
    fn fetch<'py>(
        &self,
        py: Python<'py>,
        sql: String,
        params: Vec<Bound<'py, PyAny>>,
        tx: Option<&Bound<'py, Transaction>>,
        trace: Option<&Bound<'py, Trace>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let args = params.iter().map(|p| py_to_value(p, None)).collect::<PyResult<Vec<_>>>()?;
        let conn = self.traced(tx, trace);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let rows = conn.fetch(sql, args).await.map_err(db_err)?;
            Python::attach(|py| {
                let names: Vec<Bound<'_, PyString>> = rows.columns().iter().map(|c| PyString::new(py, c)).collect();
                let out = PyList::empty(py);
                for r in 0..rows.len() {
                    let row = PyDict::new(py);
                    for (c, name) in names.iter().enumerate() {
                        row.set_item(name, cell_to_py(py, rows.cell(r, c).map_err(db_err)?)?)?;
                    }
                    out.append(row)?;
                }
                out.into_py_any(py)
            })
        })
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
        let conn = self.conn(None);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            exec::run_schema_script(conn, stmts).await.map_err(engine_err)
        })
    }

    fn drop_tables<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let stmts = migrate::drop_all(&self.schema).map_err(schema_err)?;
        let conn = self.conn(None);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            exec::run_schema_script(conn, stmts).await.map_err(engine_err)
        })
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

    /// `[(name, applied, applied_at)]` for the migrations of `dir` (see `orm_engine::migrate`).
    fn migration_status<'py>(&self, py: Python<'py>, dir: PathBuf) -> PyResult<Bound<'py, PyAny>> {
        let driver = self.driver.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let out = engine_migrate::status(&*driver, &dir).await.map_err(engine_err)?;
            Ok(out.into_iter().map(|s| (s.migration.name, s.applied, s.applied_at)).collect::<Vec<_>>())
        })
    }

    /// Applies pending migrations (up to `target`); the names applied.
    #[pyo3(signature = (dir, target = None))]
    fn migrate_up<'py>(&self, py: Python<'py>, dir: PathBuf, target: Option<String>) -> PyResult<Bound<'py, PyAny>> {
        let driver = self.driver.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let done = engine_migrate::upgrade(&*driver, &dir, target.as_deref()).await.map_err(engine_err)?;
            Ok(done.into_iter().map(|m| m.name).collect::<Vec<_>>())
        })
    }

    /// The live database as a schema file: `(schema, gaps, [(summary, sql, warning)])`, the steps
    /// being what a migration from the schema would still change (see `orm_engine::introspect`).
    fn pull_schema<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let driver = self.driver.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let p = orm_engine::introspect::pull(&*driver).await.map_err(engine_err)?;
            Ok((p.schema, p.gaps, p.steps.into_iter().map(|s| (s.summary, s.sql, s.warning)).collect::<Vec<_>>()))
        })
    }

    /// The live database against the newest snapshot of `dir`:
    /// `(migration, [(summary, sql, warning)], gaps)`.
    fn migration_drift<'py>(&self, py: Python<'py>, dir: PathBuf) -> PyResult<Bound<'py, PyAny>> {
        let driver = self.driver.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let d = orm_engine::introspect::drift(&*driver, &dir).await.map_err(engine_err)?;
            Ok((d.migration, d.steps.into_iter().map(|s| (s.summary, s.sql, s.warning)).collect::<Vec<_>>(), d.gaps))
        })
    }

    /// Records the first migration of `dir` as applied without running it; its name.
    fn migrate_baseline<'py>(&self, py: Python<'py>, dir: PathBuf) -> PyResult<Bound<'py, PyAny>> {
        let driver = self.driver.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            Ok(engine_migrate::baseline(&*driver, &dir).await.map_err(engine_err)?.name)
        })
    }

    /// The migrations `migrate_up` would apply: `[(name, path)]`.
    #[pyo3(signature = (dir, target = None))]
    fn migration_pending<'py>(&self, py: Python<'py>, dir: PathBuf, target: Option<String>) -> PyResult<Bound<'py, PyAny>> {
        let driver = self.driver.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let todo = engine_migrate::pending(&*driver, &dir, target.as_deref()).await.map_err(engine_err)?;
            Ok(todo.into_iter().map(|m| (m.name, m.path)).collect::<Vec<_>>())
        })
    }

    /// Starts applying one migration: its transaction, with the lock taken and `up.sql`
    /// run; None when another migrator applied it meanwhile.
    fn migration_begin<'py>(&self, py: Python<'py>, name: String, path: PathBuf) -> PyResult<Bound<'py, PyAny>> {
        let driver = self.driver.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let m = engine_migrate::Migration { name, path };
            let tx = engine_migrate::begin_apply(&*driver, &m).await.map_err(engine_err)?;
            Ok(tx.map(|inner| Transaction { inner }))
        })
    }

    /// Records the migration in `tx` (from `migration_begin`) and commits.
    fn migration_finish<'py>(&self, py: Python<'py>, tx: &Bound<'py, Transaction>, name: String, path: PathBuf) -> PyResult<Bound<'py, PyAny>> {
        let inner = tx.get().inner.clone();
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            engine_migrate::finish_apply(&*inner, &engine_migrate::Migration { name, path }).await.map_err(engine_err)
        })
    }

    /// Reverts the last `steps` migrations, or every one after `target`; the names reverted.
    #[pyo3(signature = (dir, steps = 1, target = None))]
    fn migrate_down<'py>(
        &self,
        py: Python<'py>,
        dir: PathBuf,
        steps: usize,
        target: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let driver = self.driver.clone();
        let down = match target {
            Some(t) => engine_migrate::Down::To(t),
            None => engine_migrate::Down::Steps(steps),
        };
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let done = engine_migrate::downgrade(&*driver, &dir, down).await.map_err(engine_err)?;
            Ok(done.into_iter().map(|m| m.name).collect::<Vec<_>>())
        })
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
    #[cfg(feature = "composition")]
    let out = orm_engine::behavior::results(&classes.native, out).map_err(db_err)?;
    #[cfg(feature = "proxy-models")]
    orm_engine::proxy::emit(&orm_engine::proxy::diagnostics(&classes.proxies, &out).map_err(db_err)?);
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
        Outcome::Rows { model, rows, types, shape } => b.model_rows(model, shape.as_ref(), rows.as_ref(), &types)?.into_py_any(py),
        Outcome::Prefetched { .. } => Err(query_err("prefetched rows need their parents".into())),
    }
}

/// Parent rows of `model` for [`exec::prefetch`]: `keys` from `value(row, key, type)`,
/// every other field `NULL`.
fn key_rows(
    schema: &schema::Schema,
    model: &str,
    keys: &[String],
    n: usize,
    value: impl Fn(usize, usize, orm_core::ir::ValueType) -> PyResult<sea_query::Value>,
) -> PyResult<Vec<Vec<Option<sea_query::Value>>>> {
    let m = schema.model(schema.model_idx(model).map_err(query_err)?);
    let positions = keys.iter().map(|k| m.field_pos(k).map_err(query_err)).collect::<PyResult<Vec<_>>>()?;
    (0..n).map(|r| {
        let mut row = vec![None; m.fields().len()];
        for (i, &pos) in positions.iter().enumerate() {
            row[pos] = Some(value(r, i, m.fields()[pos].value_type())?);
        }
        Ok(row)
    }).collect()
}

/// Attaches the rows of a [`Outcome::Prefetched`] to `parents`.
fn prefetched_to_py(py: Python<'_>, out: Outcome, classes: &Classes, db: Option<Py<PyAny>>, parents: Vec<Py<PyAny>>) -> PyResult<Py<PyAny>> {
    #[cfg(feature = "composition")]
    let out = orm_engine::behavior::results(&classes.native, out).map_err(db_err)?;
    #[cfg(feature = "proxy-models")]
    orm_engine::proxy::emit(&orm_engine::proxy::diagnostics(&classes.proxies, &out).map_err(db_err)?);
    let Outcome::Prefetched { parents: rows, prefetched } = out else {
        return Err(query_err("prefetch gave no prefetched rows".into()));
    };
    let db = db.map(|d| d.into_bound(py));
    let parents: Vec<_> = parents.into_iter().map(|p| p.into_bound(py)).collect();
    Builder::new(py, classes, db.as_ref()).prefetched(&parents, rows.as_ref(), &prefetched)?;
    Ok(py.None())
}

/// Normalize behavioral declarations before building language classes.
#[pyfunction]
#[pyo3(signature = (schema_json, context_json = None))]
fn prepare_schema(schema_json: &str, context_json: Option<&str>) -> PyResult<String> {
    let mut ir = ir::SchemaIr::from_json(schema_json).map_err(schema_err)?;
    if let Some(context) = context_json {
        let context = serde_json::from_str(context).map_err(|e| schema_err(format!("invalid definition context: {e}")))?;
        ir = orm_core::behavior::merge_definition(context, ir).map_err(schema_err)?;
    }
    orm_core::behavior::prepare(&mut ir, Some("python")).map_err(schema_err)?;
    serde_json::to_string(&ir).map_err(|e| schema_err(e.to_string()))
}

#[pyfunction]
fn native_artifact() -> PyResult<String> {
    serde_json::to_string(&orm_core::behavior::artifact()).map_err(|e| schema_err(e.to_string()))
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
#[cfg(feature = "generate-python")]
#[pyfunction]
fn generate_python(path: &str) -> PyResult<(String, String)> {
    let p = std::path::Path::new(path);
    let (ir, schema) =
        orm_core::dsl::check(orm_core::dsl::compile_file(p).map_err(schema_err)?).map_err(schema_err)?;
    let source = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let g = orm_core::codegen::python::generate(&ir, &schema, &source).map_err(schema_err)?;
    Ok((g.module, g.stub))
}

/// `python -m orm`: the `orm` command line (`orm_cli`), on its own runtime with the GIL
/// released. Gives the exit code; output goes to the process's stdout / stderr.
#[cfg(feature = "cli")]
#[pyfunction]
fn cli(py: Python<'_>, argv: Vec<String>) -> i32 {
    py.detach(|| orm_cli::run_blocking(&argv, orm_cli::Host::Python))
}

/// For `python -m orm migrate`: `(schema, dir, url, target)` as the CLI resolves them,
/// or None for any other command line.
#[cfg(feature = "cli")]
#[pyfunction]
fn cli_migrate_args(argv: Vec<String>) -> Option<(PathBuf, PathBuf, Option<String>, Option<String>)> {
    orm_cli::migrate_args(&argv, orm_cli::Host::Python)
}

/// Migration folders of `dir` in order: `[(name, path)]`.
#[pyfunction]
fn list_migrations(dir: PathBuf) -> PyResult<Vec<(String, PathBuf)>> {
    let all = engine_migrate::list(&dir).map_err(engine_err)?;
    Ok(all.into_iter().map(|m| (m.name, m.path)).collect())
}

/// A migration folder by name or number: `(name, path)`.
#[pyfunction]
fn find_migration(dir: PathBuf, name: &str) -> PyResult<(String, PathBuf)> {
    let m = engine_migrate::find(&dir, name).map_err(engine_err)?;
    Ok((m.name, m.path))
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
        if driver.dialect() != schema.dialect {
            driver.close().await;
            return Err(schema_err(format!("schema targets {}, connection uses {}", schema.dialect.name(), driver.dialect().name())));
        }
        let target = Target::new(driver.dialect()).without(&disable).map_err(query_err)?;
        Ok(Engine { driver, target, schema, classes })
    })
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    #[cfg(feature = "composition")]
    methods::register(m)?;
    m.add_function(wrap_pyfunction!(connect, m)?)?;
    m.add_function(wrap_pyfunction!(prepare_schema, m)?)?;
    m.add_function(wrap_pyfunction!(native_artifact, m)?)?;
    m.add_function(wrap_pyfunction!(profile_metadata, m)?)?;
    m.add_function(wrap_pyfunction!(compile_schema, m)?)?;
    m.add_function(wrap_pyfunction!(compile_schema_file, m)?)?;
    #[cfg(feature = "generate-python")]
    m.add_function(wrap_pyfunction!(generate_python, m)?)?;
    #[cfg(feature = "cli")]
    m.add_function(wrap_pyfunction!(cli, m)?)?;
    #[cfg(feature = "cli")]
    m.add_function(wrap_pyfunction!(cli_migrate_args, m)?)?;
    m.add_function(wrap_pyfunction!(list_migrations, m)?)?;
    m.add_function(wrap_pyfunction!(find_migration, m)?)?;
    m.add_class::<PySchema>()?;
    m.add_class::<Engine>()?;
    m.add_class::<Transaction>()?;
    m.add_class::<Trace>()?;
    m.add_class::<SessionLock>()?;
    m.add("DEFAULT", Py::new(py, DefaultMarker)?)?;
    errors::add_defaults(py)?;
    m.add("DatabaseError", py.get_type::<errors::DatabaseError>())?;
    m.add("IntegrityError", py.get_type::<errors::IntegrityError>())?;
    m.add("LockNotAvailable", py.get_type::<errors::LockNotAvailable>())?;
    m.add("QueryError", py.get_type::<errors::QueryError>())?;
    m.add("WriteProtected", py.get_type::<errors::WriteProtected>())?;
    m.add("SchemaError", py.get_type::<errors::SchemaError>())?;
    m.add("MigrationError", py.get_type::<errors::MigrationError>())?;
    Ok(())
}

/// Static packaging compatibility metadata (separate from extension manifests).
#[pyfunction]
fn profile_metadata() -> PyResult<String> {
    let profile = if cfg!(feature = "profile-tooling") { "tooling" }
        else if cfg!(feature = "profile-combined") { "combined" }
        else if cfg!(feature = "profile-postgres") { "postgres" }
        else if cfg!(feature = "profile-sqlite") { "sqlite" }
        else { "custom" };
    serde_json::to_string(&serde_json::json!({
        "abi": 1, "version": env!("CARGO_PKG_VERSION"), "language": "python", "profile": profile,
        "backends": orm_engine::compiled_backends(),
        "adapters": [],
        "build": serde_json::from_str::<serde_json::Value>(env!("ORM_BUILD_RECORD")).map_err(|e| schema_err(e.to_string()))?,
        "capabilities": { "cli": cfg!(feature = "cli"),
            "generate-python": cfg!(feature = "generate-python"),
            "generate-typescript": cfg!(feature = "generate-typescript"),
            "composition": cfg!(feature = "composition") }
    })).map_err(|e| schema_err(e.to_string()))
}
