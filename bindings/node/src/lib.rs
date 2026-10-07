//! Native engine behind the `orm` TypeScript package (Node, Bun).
//!
//! The same calls as the Python binding: the schema is sent once (`new Schema(json)`),
//! then one IR document per operation, its literals in a separate array of JS values.
//! Planning (which reads those values) happens in the call; the query runs on Tokio and
//! the promise resolves with the rows decoded in one pass: a flat array of cell values
//! per row set, plus how the rows become objects, which the TypeScript runtime builds.
//!
//! Errors are thrown as JS errors whose message starts with `[orm:<Kind>] `; the
//! runtime turns them into its error classes.

mod convert;
mod js;

#[cfg(any(
    all(feature = "profile-postgres", feature = "profile-sqlite"),
    all(feature = "profile-postgres", feature = "profile-combined"),
    all(feature = "profile-postgres", feature = "profile-tooling"),
    all(feature = "profile-sqlite", feature = "profile-combined"),
    all(feature = "profile-sqlite", feature = "profile-tooling"),
    all(feature = "profile-combined", feature = "profile-tooling")
))]
compile_error!("select exactly one named native profile; use a custom feature build for exact combinations");

use std::path::Path;
use std::sync::Arc;

use napi::bindgen_prelude::{PromiseRaw, ToNapiValue};
use napi::{sys, Env, JsValue, Unknown};
use napi_derive::napi;

use crate::convert::{Conv, JsParams};
use crate::js::{Js, V};
use orm_core::dialect::Target;
use orm_core::ir::{self, ValueType};
use orm_core::{migrate, schema};
use orm_engine::db::{self, DbError, Driver, ErrorKind, Executor, RowSet};
use orm_engine::exec::{self, Conflict, Fetched, Outcome};
use orm_engine::plan::{Output, Planner};
use orm_engine::migrate as engine_migrate;
use orm_engine::{parse_op, Error};

// -- errors ---------------------------------------------------------------------------------

fn tagged(kind: &str, msg: impl std::fmt::Display) -> napi::Error {
    napi::Error::from_reason(format!("[orm:{kind}] {msg}"))
}

fn db_kind(e: &DbError) -> &'static str {
    match e.kind {
        ErrorKind::Integrity => "IntegrityError",
        ErrorKind::LockNotAvailable => "LockNotAvailable",
        ErrorKind::Other => "DatabaseError",
    }
}

fn engine_err(e: Error) -> napi::Error {
    match e {
        Error::Query(m) => tagged("QueryError", m),
        Error::Schema(m) => tagged("SchemaError", m),
        Error::Migration(m) => tagged("MigrationError", m),
        Error::Db(e) => tagged(db_kind(&e), e),
        Error::Value(m) => tagged("TypeError", m),
        Error::Binding(e) => tagged("TypeError", e),
    }
}

fn query_err(msg: impl std::fmt::Display) -> napi::Error {
    tagged("QueryError", msg)
}

fn schema_err(msg: impl std::fmt::Display) -> napi::Error {
    tagged("SchemaError", msg)
}

// -- shared state -----------------------------------------------------------------------------

/// Per-environment data: the `Decimal` class decimal values are built with.
struct Shared {
    decimal: Option<sys::napi_ref>,
}

/// Registers the class decimal values are read as (and recognized by, as parameters):
/// `decimal.js`'s `Decimal`. The runtime calls this once when it loads.
#[napi]
pub fn set_decimal_class(env: &Env, ctor: Unknown<'_>) -> napi::Result<()> {
    let r = Js(env.raw()).reference(ctor.raw())?;
    match env.get_instance_data::<Shared>()? {
        Some(s) => s.decimal = Some(r),
        None => env.set_instance_data(Shared { decimal: Some(r) }, (), |_| {})?,
    }
    Ok(())
}

fn conv(env: &Env) -> napi::Result<Conv> {
    let js = Js(env.raw());
    let decimal = match env.get_instance_data::<Shared>()? {
        Some(Shared { decimal: Some(r) }) => Some(js.deref(*r)?),
        _ => None,
    };
    Ok(Conv { js, decimal })
}

/// A JS value already created, handed back as is.
pub struct Raw(V);

impl ToNapiValue for Raw {
    unsafe fn to_napi_value(_: sys::napi_env, val: Self) -> napi::Result<sys::napi_value> {
        Ok(val.0)
    }
}

fn params(env: &Env, values: Unknown<'_>) -> napi::Result<JsParams> {
    let conv = conv(env)?;
    let values = conv.js.elements(values.raw())?;
    Ok(JsParams { conv, values })
}

// -- results ----------------------------------------------------------------------------------

fn model_name(schema: &schema::Schema, idx: usize) -> Option<&str> {
    (idx < schema.models.len()).then(|| schema.model(idx).ir.name.as_str())
}

/// `{n, width, values}`: the cells of `rows`, row after row.
fn rows_js(c: Conv, rows: &dyn RowSet, types: &[ValueType]) -> napi::Result<V> {
    let js = c.js;
    let width = types.len();
    let values = js.array(rows.len() * width)?;
    let mut k = 0u32;
    for r in 0..rows.len() {
        for (col, ty) in types.iter().enumerate() {
            let cell = rows.cell(r, col, *ty).map_err(|e| tagged(db_kind(&e), e))?;
            js.set_element(values, k, c.cell(cell)?)?;
            k += 1;
        }
    }
    let obj = js.object()?;
    js.set(obj, "n", js.number(rows.len() as f64)?)?;
    js.set(obj, "width", js.number(width as f64)?)?;
    js.set(obj, "values", values)?;
    Ok(obj)
}

/// How rows become objects: `{model, joins: [{parent, attr, model, start, pk}]}` for
/// instances (`parent` -1 for the root object), `{model, items: [width | -1]}` for
/// `select()` rows (-1: one value).
fn output_js(js: Js, schema: &schema::Schema, output: &Output) -> napi::Result<V> {
    let obj = js.object()?;
    let name = |idx: usize| -> napi::Result<V> {
        match model_name(schema, idx) {
            Some(n) => js.str(n),
            None => js.null(),
        }
    };
    match output {
        Output::Instances { model, shape, joins } => {
            js.set(obj, "model", name(*model)?)?;
            js.set(obj, "shape", shape_js(js, shape.as_ref())?)?;
            let arr = js.array_of(joins.iter().map(|j| {
                let o = js.object()?;
                js.set(o, "parent", js.number(j.parent.map(|p| p as f64).unwrap_or(-1.0))?)?;
                js.set(o, "attr", js.str(&j.attr)?)?;
                js.set(o, "model", name(j.model)?)?;
                js.set(o, "start", js.number(j.start as f64)?)?;
                js.set(o, "pk", js.number(j.pk_pos as f64)?)?;
                js.set(o, "shape", shape_js(js, j.shape.as_ref())?)?;
                Ok(o)
            }))?;
            js.set(obj, "joins", arr)?;
        }
        Output::Rows { model, items } => {
            js.set(obj, "model", name(*model)?)?;
            let arr = js.array_of(items.iter().map(|w| js.number(w.map(|w| w as f64).unwrap_or(-1.0))))?;
            js.set(obj, "items", arr)?;
        }
    }
    Ok(obj)
}

fn prefetched_js(c: Conv, schema: &schema::Schema, fetched: &[Fetched]) -> napi::Result<V> {
    let js = c.js;
    js.array_of(fetched.iter().map(|f| {
        let p = &f.plan;
        let o = js.object()?;
        js.set(o, "attr", js.str(&p.attr)?)?;
        js.set(o, "many", js.boolean(p.many)?)?;
        js.set(o, "keyPos", js.number(p.key_pos as f64)?)?;
        js.set(o, "childKeyPos", js.number(p.child_key_pos as f64)?)?;
        js.set(o, "back", match &p.back {
            Some(b) => js.str(b)?,
            None => js.null()?,
        })?;
        js.set(o, "rows", rows_js(c, f.rows.as_ref(), &p.types)?)?;
        js.set(o, "output", output_js(js, schema, &p.output)?)?;
        js.set(o, "children", prefetched_js(c, schema, &f.children)?)?;
        Ok(o)
    }))
}

/// The JS result of an operation: `{rows, output, prefetched}` for a select, a number
/// for a count or a write without `RETURNING`, a boolean for exists, `{model, rows}` for
/// rows a write returned.
fn outcome_js(env: &Env, schema: &schema::Schema, out: Outcome) -> napi::Result<Raw> {
    #[cfg(feature = "composition")]
    let out = orm_engine::behavior::results(&schema.native_models, out).map_err(|e| tagged(db_kind(&e), e))?;
    #[cfg(feature = "proxy-models")]
    orm_engine::proxy::emit(&orm_engine::proxy::diagnostics(&schema.proxy_models, &out).map_err(|e| tagged(db_kind(&e), e))?);
    let c = conv(env)?;
    let js = c.js;
    Ok(Raw(match out {
        Outcome::Select(s) => {
            let o = js.object()?;
            js.set(o, "rows", rows_js(c, s.rows.as_ref(), &s.plan.types)?)?;
            js.set(o, "output", output_js(js, schema, &s.plan.output)?)?;
            js.set(o, "prefetched", prefetched_js(c, schema, &s.prefetched)?)?;
            o
        }
        Outcome::Count(n) => js.number(n as f64)?,
        Outcome::Exists(b) => js.boolean(b)?,
        Outcome::Affected(n) => js.number(n as f64)?,
        Outcome::Rows { model, rows, types, shape } => {
            let o = js.object()?;
            js.set(o, "model", match model_name(schema, model) {
                Some(n) => js.str(n)?,
                None => js.null()?,
            })?;
            js.set(o, "rows", rows_js(c, rows.as_ref(), &types)?)?;
            js.set(o, "shape", shape_js(js, shape.as_ref())?)?;
            o
        }
    }))
}

/// Insert / `update_many` rows (arrays aligned with `fields`) as bind values of the
/// fields' types; `undefined` (when `defaults`) is the column's default.
fn convert_rows(
    env: &Env,
    schema: &schema::Schema,
    model: &str,
    fields: &[String],
    rows: Unknown<'_>,
    defaults: bool,
) -> napi::Result<Vec<Vec<Option<sea_query::Value>>>> {
    let c = conv(env)?;
    let types = exec::field_types(schema, model, fields).map_err(engine_err)?;
    let mut out = vec![];
    for row in c.js.elements(rows.raw())? {
        let items = c.js.elements(row)?;
        if items.len() != types.len() {
            return Err(query_err("row length does not match fields"));
        }
        let mut values = Vec::with_capacity(items.len());
        for (item, ty) in items.into_iter().zip(&types) {
            let undefined = c.js.type_of(item)? == sys::ValueType::napi_undefined;
            values.push(if defaults && undefined { None } else { Some(c.value(item, Some(*ty)).map_err(engine_err)?) });
        }
        out.push(values);
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn update_many_plan(
    env: &Env,
    schema: &schema::Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: Unknown<'_>,
    filters_json: &str,
    params_: Unknown<'_>,
    returning: bool,
    batch_size: Option<u32>,
    without_defaults: bool,
) -> napi::Result<(Vec<sea_query::UpdateStatement>, exec::UpdateMany)> {
    let values = convert_rows(env, schema, model, fields, rows, false)?
        .into_iter()
        .map(|r| r.into_iter().map(|v| v.expect("no defaults in update_many")).collect())
        .collect();
    let filters: Vec<ir::Expr> =
        serde_json::from_str(filters_json).map_err(|e| query_err(format!("invalid filter IR: {e}")))?;
    let p = params(env, params_)?;
    exec::plan_update_many(schema, target, model, fields, values, &filters, &p, returning, batch_size.map(|n| n as usize), without_defaults)
        .map_err(engine_err)
}

// -- schema -----------------------------------------------------------------------------------

/// The compiled schema. Usable without a connection, e.g. to render SQL.
#[napi(js_name = "Schema")]
pub struct JsSchema {
    inner: Arc<schema::Schema>,
}

// A separate impl block: `#[napi]` registers every method of a block, ignoring `cfg` on items.
#[cfg(feature = "file-storage")]
#[napi]
impl JsSchema {
    /// Selected upload preflight: ordinary conversion/planning without SQL or I/O.
    #[napi]
    pub fn validate_file_insert(&self, env: &Env, model: String, fields: Vec<String>, rows: Unknown<'_>) -> napi::Result<()> {
        let values = convert_rows(env, &self.inner, &model, &fields, rows, true)?;
        exec::plan_insert(&self.inner, Target::new(self.inner.dialect), &model, &fields, values, None, &orm_engine::NoParams).map_err(engine_err)?;
        Ok(())
    }
}

#[napi]
impl JsSchema {
    #[napi(constructor)]
    pub fn new(schema_json: String) -> napi::Result<Self> {
        let mut ir: ir::SchemaIr =
            serde_json::from_str(&schema_json).map_err(|e| schema_err(format!("invalid schema IR: {e}")))?;
        orm_core::behavior::prepare(&mut ir, Some("typescript")).map_err(schema_err)?;
        let inner = schema::Schema::from_ir(ir).map_err(schema_err)?;
        db::require_dialect(inner.dialect).map_err(|e| schema_err(e.to_string()))?;
        Ok(JsSchema { inner: Arc::new(inner) })
    }

    /// SQL for an operation with parameters inlined. For debugging and tests only.
    #[napi]
    pub fn sql(&self, env: &Env, op_json: String, params_: Unknown<'_>) -> napi::Result<String> {
        let op = parse_op(&op_json).map_err(engine_err)?;
        let target = Target::new(self.inner.dialect);
        let p = params(env, params_)?;
        let plan = Planner::plan(&self.inner, target, &op, &p).map_err(engine_err)?;
        Ok(exec::sql(target, &plan))
    }

    /// The SQL of `update_many` (one statement per batch), parameters inlined.
    #[napi]
    #[allow(clippy::too_many_arguments)]
    pub fn update_many_sql(
        &self,
        env: &Env,
        model: String,
        fields: Vec<String>,
        rows: Unknown<'_>,
        filters_json: String,
        params_: Unknown<'_>,
        batch_size: Option<u32>,
        disable: Vec<String>,
    ) -> napi::Result<Vec<String>> {
        let target = Target::new(self.inner.dialect).without(&disable).map_err(query_err)?;
        let (stmts, _) =
            update_many_plan(env, &self.inner, target, &model, &fields, rows, &filters_json, params_, false, batch_size, false)?;
        Ok(stmts.iter().map(|s| db::to_string(target.dialect, s)).collect())
    }

    /// Idempotent DDL for the whole schema, in dependency order.
    #[napi]
    pub fn ddl(&self) -> napi::Result<Vec<String>> {
        migrate::create_all(&self.inner).map_err(schema_err)
    }

    /// The database schema as a snapshot (JSON), the format migrations store.
    #[napi]
    pub fn snapshot(&self) -> napi::Result<String> {
        let s = migrate::snapshot(&self.inner).map_err(schema_err)?;
        serde_json::to_string_pretty(&s).map_err(schema_err)
    }

    /// The next migration for the migrations directory `dir`, as JSON, without writing.
    #[napi]
    pub fn plan_migration(&self, dir: String) -> napi::Result<String> {
        let plan = migrate::files::next(std::path::Path::new(&dir), &self.inner).map_err(schema_err)?;
        serde_json::to_string(&plan).map_err(schema_err)
    }

    /// Writes the next migration into `dir`; returns its folder name, or null when
    /// nothing changed (and not `empty`).
    #[napi]
    pub fn make_migration(&self, dir: String, name: Option<String>, empty: Option<bool>) -> napi::Result<Option<String>> {
        let made = migrate::files::make(std::path::Path::new(&dir), &self.inner, name.as_deref(), empty.unwrap_or(false))
            .map_err(schema_err)?;
        Ok(made.map(|(folder, _)| folder.name))
    }

    /// The migration from `previous` (a snapshot; null for an empty database) to this
    /// schema, as JSON: `{"up": [step], "down": [step], "snapshot": {...}}`.
    #[napi]
    pub fn migration(&self, previous: Option<String>) -> napi::Result<String> {
        let previous = match previous {
            Some(json) => migrate::parse_snapshot(&json).map_err(schema_err)?,
            None => migrate::DbSchema::default(),
        };
        let plan = migrate::plan(&self.inner, &previous).map_err(schema_err)?;
        serde_json::to_string(&plan).map_err(schema_err)
    }
}

// -- transactions -------------------------------------------------------------------------------

/// A database transaction (or savepoint, when nested).
#[napi]
pub struct Transaction {
    inner: Arc<dyn db::Transaction>,
}

#[napi]
impl Transaction {
    #[napi]
    pub fn commit<'env>(&self, env: &'env Env) -> napi::Result<PromiseRaw<'env, ()>> {
        let tx = self.inner.clone();
        env.spawn_future(async move { tx.commit().await.map_err(|e| tagged(db_kind(&e), e)) })
    }

    #[napi]
    pub fn rollback<'env>(&self, env: &'env Env) -> napi::Result<PromiseRaw<'env, ()>> {
        let tx = self.inner.clone();
        env.spawn_future(async move { tx.rollback().await.map_err(|e| tagged(db_kind(&e), e)) })
    }
}

// -- engine ---------------------------------------------------------------------------------------

#[napi]
pub struct Engine {
    driver: Arc<dyn Driver>,
    target: Target,
    schema: Arc<schema::Schema>,
}

impl Engine {
    fn conn(&self, tx: Option<&Transaction>) -> Arc<dyn Executor> {
        match tx {
            Some(tx) => tx.inner.clone(),
            None => self.driver.clone(),
        }
    }

    fn script<'env>(
        &self,
        env: &'env Env,
        statements: Vec<String>,
        tx: Option<&Transaction>,
    ) -> napi::Result<PromiseRaw<'env, ()>> {
        let conn = self.conn(tx);
        let in_tx = tx.is_some();
        env.spawn_future(async move { exec::run_script(conn, statements, in_tx).await.map_err(engine_err) })
    }
}

#[napi]
impl Engine {
    /// Runs one query IR document (see `outcome_js` for what the promise gives).
    #[napi(ts_return_type = "Promise<unknown>")]
    pub fn run<'env>(
        &self,
        env: &'env Env,
        op_json: String,
        params_: Unknown<'_>,
        tx: Option<&Transaction>,
    ) -> napi::Result<PromiseRaw<'env, Raw>> {
        let op = parse_op(&op_json).map_err(engine_err)?;
        let target = self.target;
        let plan = Planner::plan(&self.schema, target, &op, &params(env, params_)?).map_err(engine_err)?;
        let conn = self.conn(tx);
        let schema = self.schema.clone();
        env.spawn_future_with_callback(
            async move { exec::run(conn.as_ref(), target, plan).await.map_err(engine_err) },
            move |env, out| outcome_js(env, &schema, out),
        )
    }

    /// `INSERT ... RETURNING` every column; the promise gives `{model, rows}`.
    ///
    /// `rows` are arrays aligned with `fields` (`undefined`: the column's default). With
    /// `conflict` (unique field names) rows hitting that constraint update the `update`
    /// fields from the new row and apply the `set` assignments (JSON list of
    /// `{"field", "value"}` IR, parameters in `params`), or are skipped if `update` is
    /// null.
    #[napi(ts_return_type = "Promise<unknown>")]
    #[allow(clippy::too_many_arguments)]
    pub fn insert<'env>(
        &self,
        env: &'env Env,
        model: String,
        fields: Vec<String>,
        rows: Unknown<'_>,
        conflict: Option<Vec<String>>,
        update: Option<Vec<String>>,
        set: Option<String>,
        params_: Unknown<'_>,
        tx: Option<&Transaction>,
    ) -> napi::Result<PromiseRaw<'env, Raw>> {
        let set: Vec<ir::Assignment> = match set {
            Some(json) => serde_json::from_str(&json).map_err(|e| query_err(format!("invalid assignment IR: {e}")))?,
            None => vec![],
        };
        let conflict = conflict.map(|target| match update {
            Some(update) => Conflict::Update { target, update, set },
            None => Conflict::Nothing { target },
        });
        let values = convert_rows(env, &self.schema, &model, &fields, rows, true)?;
        let p = params(env, params_)?;
        let plan =
            exec::plan_insert(&self.schema, self.target, &model, &fields, values, conflict, &p).map_err(engine_err)?;
        let target = self.target;
        let conn = self.conn(tx);
        let schema = self.schema.clone();
        env.spawn_future_with_callback(
            async move { exec::run(conn.as_ref(), target, plan).await.map_err(engine_err) },
            move |env, out| outcome_js(env, &schema, out),
        )
    }

    /// Updates each row (an array aligned with `fields`, the primary key first) to its
    /// own values, among the rows matching `filtersJson`. Several statements run in one
    /// transaction (inside `tx` when given). Gives the number of rows updated, or
    /// `{model, rows}` with `returning`.
    #[napi(ts_return_type = "Promise<unknown>")]
    #[allow(clippy::too_many_arguments)]
    pub fn update_many<'env>(
        &self,
        env: &'env Env,
        model: String,
        fields: Vec<String>,
        rows: Unknown<'_>,
        filters_json: String,
        params_: Unknown<'_>,
        returning: bool,
        batch_size: Option<u32>,
        tx: Option<&Transaction>,
        without_defaults: Option<bool>,
    ) -> napi::Result<PromiseRaw<'env, Raw>> {
        let (_, um) = update_many_plan(
            env, &self.schema, self.target, &model, &fields, rows, &filters_json, params_, returning, batch_size,
            without_defaults.unwrap_or(false),
        )?;
        let conn = self.conn(tx);
        let own_tx = tx.is_none();
        let schema = self.schema.clone();
        env.spawn_future_with_callback(
            async move { exec::run_update_many(conn.as_ref(), um, own_tx).await.map_err(engine_err) },
            move |env, out| outcome_js(env, &schema, out),
        )
    }

    /// Starts a transaction, or a savepoint inside `tx`.
    #[napi]
    pub fn begin<'env>(&self, env: &'env Env, tx: Option<&Transaction>) -> napi::Result<PromiseRaw<'env, Transaction>> {
        let conn = self.conn(tx);
        env.spawn_future(async move {
            let inner = conn.begin().await.map_err(|e| tagged(db_kind(&e), e))?;
            Ok(Transaction { inner })
        })
    }

    /// Advisory lock on a validated integer key or UTF-8 name, returning a boolean.
    #[napi]
    pub fn advisory_lock<'env>(
        &self, env: &'env Env, key: String, name: Option<napi::bindgen_prelude::Buffer>,
        exclusive: bool, nowait: bool, tx: &Transaction,
    ) -> napi::Result<PromiseRaw<'env, bool>> {
        let key = match name {
            Some(name) => orm_engine::advisory::key(&name),
            None => key.parse::<i64>().map_err(|_| tagged("TypeError", "lock key must fit in 64 bits"))?,
        };
        let conn = self.conn(Some(tx));
        env.spawn_future(async move {
            conn.advisory_lock(key, exclusive, nowait).await.map_err(|e| tagged(db_kind(&e), e))
        })
    }

    /// Raw SQL escape hatch (one or more statements); gives the rows affected.
    #[napi]
    pub fn execute<'env>(&self, env: &'env Env, sql: String, tx: Option<&Transaction>) -> napi::Result<PromiseRaw<'env, f64>> {
        let conn = self.conn(tx);
        env.spawn_future(async move { conn.batch(sql).await.map(|n| n as f64).map_err(|e| tagged(db_kind(&e), e)) })
    }

    /// Raw query whose columns are all read as text. For tooling such as the migration
    /// runner, not for application queries.
    #[napi]
    pub fn fetch_text<'env>(
        &self,
        env: &'env Env,
        sql: String,
        tx: Option<&Transaction>,
    ) -> napi::Result<PromiseRaw<'env, Vec<Vec<Option<String>>>>> {
        let conn = self.conn(tx);
        env.spawn_future(async move { conn.query_text(sql).await.map_err(|e| tagged(db_kind(&e), e)) })
    }

    #[napi]
    pub fn create_tables<'env>(&self, env: &'env Env) -> napi::Result<PromiseRaw<'env, ()>> {
        let stmts = migrate::create_all(&self.schema).map_err(schema_err)?;
        let conn = self.conn(None);
        env.spawn_future(async move { exec::run_schema_script(conn, stmts).await.map_err(engine_err) })
    }

    #[napi]
    pub fn drop_tables<'env>(&self, env: &'env Env) -> napi::Result<PromiseRaw<'env, ()>> {
        let stmts = migrate::drop_all(&self.schema).map_err(schema_err)?;
        let conn = self.conn(None);
        env.spawn_future(async move { exec::run_schema_script(conn, stmts).await.map_err(engine_err) })
    }

    /// Runs SQL statements in order, in one transaction (Postgres DDL is transactional).
    #[napi]
    pub fn execute_script<'env>(
        &self,
        env: &'env Env,
        statements: Vec<String>,
        tx: Option<&Transaction>,
    ) -> napi::Result<PromiseRaw<'env, ()>> {
        self.script(env, statements, tx)
    }

    /// The migrations of `dir` and whether they are applied: JSON
    /// `[{name, path, applied, appliedAt}]` (see `orm_engine::migrate`).
    #[napi]
    pub fn migration_status<'env>(&self, env: &'env Env, dir: String) -> napi::Result<PromiseRaw<'env, String>> {
        let driver = self.driver.clone();
        env.spawn_future(async move {
            let out = engine_migrate::status(&*driver, Path::new(&dir)).await.map_err(engine_err)?;
            let rows: Vec<_> = out
                .into_iter()
                .map(|s| serde_json::json!({"name": s.migration.name, "path": s.migration.path, "applied": s.applied, "appliedAt": s.applied_at}))
                .collect();
            Ok(serde_json::Value::Array(rows).to_string())
        })
    }

    /// Applies pending migrations (up to `target`); the names applied.
    #[napi]
    pub fn migrate_up<'env>(&self, env: &'env Env, dir: String, target: Option<String>) -> napi::Result<PromiseRaw<'env, Vec<String>>> {
        let driver = self.driver.clone();
        env.spawn_future(async move {
            let done = engine_migrate::upgrade(&*driver, Path::new(&dir), target.as_deref()).await.map_err(engine_err)?;
            Ok(done.into_iter().map(|m| m.name).collect())
        })
    }

    /// Reverts the last `steps` migrations, or every one after `target`; the names reverted.
    #[napi]
    pub fn migrate_down<'env>(
        &self,
        env: &'env Env,
        dir: String,
        steps: u32,
        target: Option<String>,
    ) -> napi::Result<PromiseRaw<'env, Vec<String>>> {
        let driver = self.driver.clone();
        let down = match target {
            Some(t) => engine_migrate::Down::To(t),
            None => engine_migrate::Down::Steps(steps as usize),
        };
        env.spawn_future(async move {
            let done = engine_migrate::downgrade(&*driver, Path::new(&dir), down).await.map_err(engine_err)?;
            Ok(done.into_iter().map(|m| m.name).collect())
        })
    }

    #[napi]
    pub fn close<'env>(&self, env: &'env Env) -> napi::Result<PromiseRaw<'env, ()>> {
        let driver = self.driver.clone();
        env.spawn_future(async move {
            driver.close().await;
            Ok(())
        })
    }
}

/// `engine = await connect(url, schema, maxConnections, disable)`. `disable` switches
/// capabilities off (`"ilike"`, `"update_from_values"`, ...) so the planner takes its
/// fallback paths.
#[napi]
pub fn connect<'env>(
    env: &'env Env,
    url: String,
    schema: &JsSchema,
    max_connections: u32,
    disable: Vec<String>,
) -> napi::Result<PromiseRaw<'env, Engine>> {
    let schema = schema.inner.clone();
    env.spawn_future(async move {
        let driver = db::connect(&url, max_connections as usize).await.map_err(|e| tagged(db_kind(&e), e))?;
        if driver.dialect() != schema.dialect {
            driver.close().await;
            return Err(schema_err(format!("schema targets {}, connection uses {}", schema.dialect.name(), driver.dialect().name())));
        }
        let target = Target::new(driver.dialect()).without(&disable).map_err(query_err)?;
        Ok(Engine { driver, target, schema })
    })
}

#[cfg(feature = "model-composition")]
#[napi]
impl Engine {
    /// Attach local values to an existing shared-key parent.
    #[napi(ts_return_type = "Promise<unknown>")]
    pub fn attach<'env>(&self, env: &'env Env, model: String, parent_id: Unknown<'_>, fields: Vec<String>, rows: Unknown<'_>, tx: Option<&Transaction>) -> napi::Result<PromiseRaw<'env, Raw>> {
        let model_idx = self.schema.model_idx(&model).map_err(schema_err)?;
        let identity = conv(env)?.value(parent_id.raw(), Some(self.schema.model(model_idx).pk_field().value_type())).map_err(engine_err)?;
        let values = convert_rows(env, &self.schema, &model, &fields, rows, true)?;
        let plan = orm_engine::composed::prepare_attach(&self.schema, self.target, &model, identity, &fields, values).map_err(engine_err)?;
        let target = self.target;
        let conn = self.conn(tx);
        let schema = self.schema.clone();
        env.spawn_future_with_callback(
            async move { orm_engine::composed::run_insert(conn.as_ref(), target, plan).await.map_err(engine_err) },
            move |env, out| outcome_js(env, &schema, out),
        )
    }

}

// -- schema compiler ------------------------------------------------------------------------------

/// Compiles schema-language source to the schema IR (JSON). `path` is where it came
/// from, for error messages and `import` resolution.
#[napi]
pub fn prepare_schema(schema_json: String, context_json: Option<String>) -> napi::Result<String> {
    let mut ir: ir::SchemaIr = serde_json::from_str(&schema_json).map_err(|e| schema_err(e.to_string()))?;
    if let Some(context) = context_json {
        let context = serde_json::from_str(&context).map_err(|e| schema_err(format!("invalid definition context: {e}")))?;
        ir = orm_core::behavior::merge_definition(context, ir).map_err(schema_err)?;
    }
    orm_core::behavior::prepare(&mut ir, Some("typescript")).map_err(schema_err)?;
    serde_json::to_string(&ir).map_err(|e| schema_err(e.to_string()))
}

#[napi]
pub fn native_artifact() -> napi::Result<String> {
    serde_json::to_string(&orm_core::behavior::artifact()).map_err(|e| schema_err(e.to_string()))
}

#[napi]
pub fn compile_schema(source: String, path: Option<String>) -> napi::Result<String> {
    let ir = orm_core::dsl::compile(&source, path.as_deref().map(std::path::Path::new)).map_err(schema_err)?;
    let (ir, _) = orm_core::dsl::check(ir).map_err(schema_err)?;
    serde_json::to_string(&ir).map_err(schema_err)
}

/// `npx orm`: the `orm` command line (`orm_cli`). Resolves to the exit code; output
/// goes to the process's stdout / stderr.
#[cfg(feature = "cli")]
#[napi]
pub fn cli<'env>(env: &'env Env, argv: Vec<String>) -> napi::Result<PromiseRaw<'env, i32>> {
    env.spawn_future(async move { Ok(orm_cli::run(&argv, orm_cli::Host::Node).await) })
}

/// Migration folders of `dir` in order: `[[name, path]]`.
#[napi]
pub fn list_migrations(dir: String) -> napi::Result<Vec<Vec<String>>> {
    let all = engine_migrate::list(Path::new(&dir)).map_err(engine_err)?;
    Ok(all.into_iter().map(|m| vec![m.name, m.path.to_string_lossy().into_owned()]).collect())
}

/// A migration folder by name or number: `[name, path]`.
#[napi]
pub fn find_migration(dir: String, name: String) -> napi::Result<Vec<String>> {
    let m = engine_migrate::find(Path::new(&dir), &name).map_err(engine_err)?;
    Ok(vec![m.name, m.path.to_string_lossy().into_owned()])
}

#[napi]
pub fn compile_schema_file(path: String) -> napi::Result<String> {
    let ir = orm_core::dsl::compile_file(std::path::Path::new(&path)).map_err(schema_err)?;
    let (ir, _) = orm_core::dsl::check(ir).map_err(schema_err)?;
    serde_json::to_string(&ir).map_err(schema_err)
}

/// `models.ts` source for a schema file; the runtime is imported from `runtime`.
#[cfg(feature = "generate-typescript")]
#[napi]
pub fn generate_typescript(path: String, runtime: Option<String>) -> napi::Result<String> {
    let p = std::path::Path::new(&path);
    let (ir, schema) =
        orm_core::dsl::check(orm_core::dsl::compile_file(p).map_err(schema_err)?).map_err(schema_err)?;
    let source = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    orm_core::codegen::typescript::generate(&ir, &schema, &source, runtime.as_deref().unwrap_or("orm")).map_err(schema_err)
}

#[cfg(feature = "composition")]
mod methods { include!(env!("ORM_NODE_METHODS")); }

/// Static packaging compatibility metadata (separate from extension manifests).
#[napi]
pub fn profile_metadata() -> napi::Result<String> {
    let profile = if cfg!(feature = "profile-tooling") { "tooling" }
        else if cfg!(feature = "profile-combined") { "combined" }
        else if cfg!(feature = "profile-postgres") { "postgres" }
        else if cfg!(feature = "profile-sqlite") { "sqlite" }
        else { "custom" };
    serde_json::to_string(&serde_json::json!({
        "abi": 1, "version": env!("CARGO_PKG_VERSION"), "language": "node", "profile": profile,
        "backends": orm_engine::compiled_backends(),
        "adapters": [],
        "build": serde_json::from_str::<serde_json::Value>(env!("ORM_BUILD_RECORD")).map_err(|e| schema_err(e.to_string()))?,
        "capabilities": { "cli": cfg!(feature = "cli"),
            "generate-python": cfg!(feature = "generate-python"),
            "generate-typescript": cfg!(feature = "generate-typescript"),
            "composition": cfg!(feature = "composition") }
    })).map_err(|e| schema_err(e.to_string()))
}

fn shape_js(js: Js, shape: Option<&orm_core::behavior::ResultShape>) -> napi::Result<V> {
    let Some(shape) = shape else { return js.null() };
    js.array_of(shape.fields.iter().map(|f| {
        let o = js.object()?;
        js.set(o, "field", js.number(f.field.position as f64)?)?;
        js.set(o, "slot", js.number(f.physical.expect("selected slot") as f64)?)?;
        js.set(o, "public", js.boolean(f.public)?)?;
        Ok(o)
    }))
}
