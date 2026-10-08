//! Running planned statements: the main query and its prefetch queries, writes with
//! `RETURNING`, `update_many` batches and DDL scripts. Bindings plan synchronously
//! (their parameter values are only readable there), then run the plan here and turn the
//! [`Outcome`] into their own objects.

use std::sync::Arc;

use sea_query::{InsertStatement, UpdateStatement, Value};

use crate::db::{self, BoxFuture, Cell, DbError, DbResult, Executor, RowSet};
use crate::error::{query_err, Error, Result};
use crate::params::{null_of, Params};
use crate::plan::{self, Plan, PrefetchPlan, SelectPlan};
use orm_core::dialect::Target;
use orm_core::ir::{self, ValueType};
use orm_core::schema::Schema;

/// A related set fetched for parent rows, and the sets fetched for its own rows.
pub struct Fetched {
    pub plan: PrefetchPlan,
    pub rows: Box<dyn RowSet>,
    pub children: Vec<Fetched>,
}

/// The rows of a SELECT and of its prefetch queries.
pub struct Selected {
    pub rows: Box<dyn RowSet>,
    pub plan: SelectPlan,
    pub prefetched: Vec<Fetched>,
}

/// What running a plan gives.
pub enum Outcome {
    Select(Box<Selected>),
    Count(i64),
    Exists(bool),
    /// Rows affected by an UPDATE / DELETE without `RETURNING`.
    Affected(u64),
    /// Rows of `model` returned by a write (`RETURNING` every column).
    Rows { model: usize, rows: Box<dyn RowSet>, types: Vec<ValueType>, shape: Option<orm_core::behavior::ResultShape> },
}

/// Runs `plan` on `conn` (the pool or a transaction).
pub async fn run(conn: &dyn Executor, target: Target, plan: Plan) -> Result<Outcome> {
    let d = target.dialect;
    Ok(match plan {
        Plan::Select(p) => Outcome::Select(Box::new(run_select(conn, target, p).await?)),
        Plan::Count(s) => {
            let (sql, args) = db::build(d, &s);
            let rows = conn.query(sql, args).await?;
            Outcome::Count(if rows.is_empty() { 0 } else { rows.get_i64(0, 0)? })
        }
        Plan::Exists(s) => {
            let (sql, args) = db::build(d, &s);
            let rows = conn.query(sql, args).await?;
            Outcome::Exists(!rows.is_empty() && rows.get_bool(0, 0)?)
        }
        Plan::Update(s, returning) => {
            let (sql, args) = db::build(d, &s);
            count_or_rows(conn, sql, args, returning).await?
        }
        Plan::Delete(s, returning) => {
            let (sql, args) = db::build(d, &s);
            count_or_rows(conn, sql, args, returning).await?
        }
        Plan::Insert(s, returned) => {
            let (sql, args) = db::build(d, &s);
            match returned {
                Some((model, types)) => Outcome::Rows { model, rows: conn.query(sql, args).await?, types, shape: None },
                None => Outcome::Affected(conn.execute(sql, args).await?),
            }
        }
        #[cfg(feature = "model-composition")]
        Plan::ComposedInsert(insert) => return crate::composed::run_insert(conn, target, *insert).await,
        #[cfg(feature = "model-composition")]
        Plan::ComposedMutation(plan) => return crate::composed::run_mutation(conn, target, *plan).await,
    })
}

/// SQL of a plan with its parameters inlined, for debugging.
pub fn sql(target: Target, plan: &Plan) -> String {
    let d = target.dialect;
    match plan {
        Plan::Select(p) => db::to_string(d, &p.stmt),
        Plan::Count(s) | Plan::Exists(s) => db::to_string(d, s),
        Plan::Update(s, _) => db::to_string(d, s),
        Plan::Delete(s, _) => db::to_string(d, s),
        Plan::Insert(s, _) => db::to_string(d, s),
        #[cfg(feature = "model-composition")]
        Plan::ComposedInsert(_) => "-- Composed insert executes ancestor inserts and a final read in one transaction".into(),
        #[cfg(feature = "model-composition")]
        Plan::ComposedMutation(_) => "-- Composed mutation captures identities and writes owners in one transaction".into(),
    }
}

/// SQL of a plan with its parameter placeholders: the statement shape, for debugging.
pub fn statement(target: Target, plan: &Plan) -> String {
    let d = target.dialect;
    match plan {
        Plan::Select(p) => db::build(d, &p.stmt).0,
        Plan::Count(s) | Plan::Exists(s) => db::build(d, s).0,
        Plan::Update(s, _) => db::build(d, s).0,
        Plan::Delete(s, _) => db::build(d, s).0,
        Plan::Insert(s, _) => db::build(d, s).0,
        #[cfg(feature = "model-composition")]
        Plan::ComposedInsert(_) | Plan::ComposedMutation(_) => sql(target, plan),
    }
}

/// An UPDATE / DELETE: the row count, or the rows when it has `RETURNING`.
async fn count_or_rows(
    conn: &dyn Executor,
    sql: String,
    args: Vec<Value>,
    returning: Option<plan::Returned>,
) -> Result<Outcome> {
    Ok(match returning {
        None => Outcome::Affected(conn.execute(sql, args).await?),
        Some((model, types, shape)) => Outcome::Rows { model, rows: conn.query(sql, args).await?, types, shape },
    })
}

async fn run_select(conn: &dyn Executor, target: Target, mut plan: SelectPlan) -> DbResult<Selected> {
    let (sql, args) = db::build(target.dialect, &plan.stmt);
    let rows = conn.query(sql, args).await?;
    let prefetched = run_prefetch(conn, target, rows.as_ref(), std::mem::take(&mut plan.prefetch)).await?;
    Ok(Selected { rows, plan, prefetched })
}

/// Runs each prefetch query for the keys in `parent`, then the prefetches nested in it.
///
/// Each key is one bound parameter, so keys beyond the dialect's parameter limit (less
/// what the query binds itself) go to further queries. The chunks split the parents,
/// never one parent's rows, so ordering and slices per parent hold.
fn run_prefetch<'a>(
    conn: &'a dyn Executor,
    target: Target,
    parent: &'a dyn RowSet,
    plans: Vec<PrefetchPlan>,
) -> BoxFuture<'a, DbResult<Vec<Fetched>>> {
    Box::pin(async move {
        let mut out = Vec::with_capacity(plans.len());
        for mut p in plans {
            let null = null_of(Some(p.key_type));
            let mut seen = std::collections::HashSet::new();
            let mut keys = vec![];
            for i in 0..parent.len() {
                let k = parent.value(i, p.key_pos, p.key_type)?;
                if k != null && seen.insert(k.clone()) {
                    keys.push(k);
                }
            }
            let rows = match keys.first() {
                None => Box::new(EmptyRows) as Box<dyn RowSet>,
                Some(k) => {
                    let own = db::build(target.dialect, &p.statement(vec![k.clone()])).1.len() - 1;
                    let chunk = target.caps.max_params.saturating_sub(own).max(1);
                    let mut parts = Vec::with_capacity(keys.len().div_ceil(chunk));
                    for keys in keys.chunks(chunk) {
                        let (sql, args) = db::build(target.dialect, &p.statement(keys.to_vec()));
                        parts.push(conn.query(sql, args).await?);
                    }
                    if parts.len() == 1 {
                        parts.pop().expect("one part")
                    } else {
                        Box::new(ChainedRows::new(parts))
                    }
                }
            };
            let nested = std::mem::take(&mut p.children);
            let children = run_prefetch(conn, target, rows.as_ref(), nested).await?;
            out.push(Fetched { plan: p, rows, children });
        }
        Ok(out)
    })
}

/// The rows of several statements as one set (a prefetch split into several queries,
/// `update_many` batches).
pub struct ChainedRows {
    parts: Vec<Box<dyn RowSet>>,
    /// Index of each part's first row.
    starts: Vec<usize>,
    len: usize,
}

impl ChainedRows {
    pub fn new(parts: Vec<Box<dyn RowSet>>) -> Self {
        let mut starts = Vec::with_capacity(parts.len());
        let mut len = 0;
        for p in &parts {
            starts.push(len);
            len += p.len();
        }
        ChainedRows { parts, starts, len }
    }

    /// The part holding `row`, and the row's index in it.
    fn locate(&self, row: usize) -> (&dyn RowSet, usize) {
        let i = self.starts.partition_point(|&s| s <= row) - 1;
        (self.parts[i].as_ref(), row - self.starts[i])
    }
}

impl RowSet for ChainedRows {
    fn len(&self) -> usize {
        self.len
    }
    fn cell(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Cell<'_>> {
        let (p, r) = self.locate(row);
        p.cell(r, col, ty)
    }
    fn value(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Value> {
        let (p, r) = self.locate(row);
        p.value(r, col, ty)
    }
    fn get_i64(&self, row: usize, col: usize) -> DbResult<i64> {
        let (p, r) = self.locate(row);
        p.get_i64(r, col)
    }
    fn get_bool(&self, row: usize, col: usize) -> DbResult<bool> {
        let (p, r) = self.locate(row);
        p.get_bool(r, col)
    }
}

/// The result of a prefetch with no keys: no query runs.
pub struct EmptyRows;

impl RowSet for EmptyRows {
    fn len(&self) -> usize {
        0
    }
    fn cell(&self, _: usize, _: usize, _: ValueType) -> DbResult<Cell<'_>> {
        Err(DbError::other("no rows"))
    }
    fn value(&self, _: usize, _: usize, _: ValueType) -> DbResult<Value> {
        Err(DbError::other("no rows"))
    }
    fn get_i64(&self, _: usize, _: usize) -> DbResult<i64> {
        Err(DbError::other("no rows"))
    }
    fn get_bool(&self, _: usize, _: usize) -> DbResult<bool> {
        Err(DbError::other("no rows"))
    }
}

// -- writes --------------------------------------------------------------------------------

/// The value types of `fields` of `model`: what a binding converts insert and
/// `update_many` rows by.
pub fn field_types(schema: &Schema, model: &str, fields: &[String]) -> Result<Vec<ValueType>> {
    let m = schema.model(schema.model_idx(model).map_err(query_err)?);
    fields.iter().map(|f| m.field(f).map(|f| f.value_type())).collect::<std::result::Result<_, _>>().map_err(query_err)
}

/// What an insert does with rows hitting a unique constraint (field names; `filter`, the
/// partial unique index's predicate, and `set` are IR whose parameters are the insert's
/// `params`).
#[derive(Clone)]
pub enum Conflict {
    Nothing { target: Vec<String>, filter: Option<ir::Expr> },
    Update { target: Vec<String>, filter: Option<ir::Expr>, update: Vec<String>, set: Vec<ir::Assignment> },
}

/// `INSERT ... RETURNING` every column. `rows` hold a value per field (`None`: the
/// column's default), converted by [`field_types`].
pub fn plan_insert(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: Vec<Vec<Option<Value>>>,
    conflict: Option<Conflict>,
    params: &dyn Params,
) -> Result<Plan> {
    plan_insert_with(schema, target, model, fields, rows, conflict, params, true)
}

/// [`plan_insert`]; without `returning` the insert gives the affected-row count. A
/// composed model's insert reads its rows back in any case.
#[allow(clippy::too_many_arguments)]
fn plan_insert_with(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: Vec<Vec<Option<Value>>>,
    conflict: Option<Conflict>,
    params: &dyn Params,
    returning: bool,
) -> Result<Plan> {
    #[cfg(feature = "model-composition")]
    if crate::composed::is_composed(schema, model)? {
        if conflict.is_some() { return Err(Error::query("composed inserts do not support on_conflict")); }
        return Ok(Plan::ComposedInsert(Box::new(crate::composed::prepare_insert(schema, target, model, fields, rows)?)));
    }
    let model_idx = schema.model_idx(model).map_err(query_err)?;
    let on_conflict = conflict.map(|c| match c {
        Conflict::Nothing { target, filter } => plan::OnConflict::Nothing(target, filter),
        Conflict::Update { target, filter, update, set } => plan::OnConflict::Update(target, filter, update, set),
    });
    let (stmt, types): (InsertStatement, _) =
        plan::plan_insert(schema, target, model, fields, rows, on_conflict, params, returning)?;
    Ok(Plan::Insert(stmt, returning.then_some((model_idx, types))))
}

/// `insert_many`'s plans: [`plan_insert`] for each batch of rows. A batch holds as many
/// rows as fit in the dialect's parameter limit (less the conflict clause's own
/// parameters), or `batch_size` rows when that is smaller. Without `returning` the plans
/// give the affected-row count. Run them with [`run_inserts`].
#[allow(clippy::too_many_arguments)]
pub fn plan_inserts(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: Vec<Vec<Option<Value>>>,
    conflict: Option<Conflict>,
    params: &dyn Params,
    batch_size: Option<usize>,
    returning: bool,
) -> Result<Vec<Plan>> {
    if batch_size == Some(0) {
        return Err(Error::query("batch_size must be at least 1"));
    }
    #[cfg(feature = "model-composition")]
    if crate::composed::is_composed(schema, model)? {
        return Ok(vec![plan_insert_with(schema, target, model, fields, rows, conflict, params, returning)?]);
    }
    let m = schema.model(schema.model_idx(model).map_err(query_err)?);
    // Client defaults can add columns, so the row width is known only after them.
    let (fields, mut rows) = crate::client_default::fill(m, fields, rows)?;
    let mut chunk = target.caps.max_params.saturating_sub(params.len()) / fields.len().max(1);
    if fields.is_empty() {
        chunk = 1; // `DEFAULT VALUES` inserts one row
    }
    if let Some(n) = batch_size {
        chunk = chunk.min(n);
    }
    let chunk = chunk.max(1);
    if rows.len() <= chunk {
        return Ok(vec![plan_insert_with(schema, target, model, &fields, rows, conflict, params, returning)?]);
    }
    let mut plans = Vec::with_capacity(rows.len().div_ceil(chunk));
    while !rows.is_empty() {
        let rest = rows.split_off(chunk.min(rows.len()));
        let conflict = conflict.clone();
        plans.push(plan_insert_with(schema, target, model, &fields, std::mem::replace(&mut rows, rest), conflict, params, returning)?);
    }
    Ok(plans)
}

/// Runs [`plan_inserts`]' plans, in a transaction of their own when there are several
/// and `conn` isn't one already (`own_tx`). Gives the inserted rows in input order, or
/// without `returning` the affected-row count.
pub async fn run_inserts(conn: &dyn Executor, target: Target, mut plans: Vec<Plan>, own_tx: bool, returning: bool) -> Result<Outcome> {
    if plans.len() == 1 {
        return Ok(match run(conn, target, plans.pop().expect("one plan")).await? {
            Outcome::Rows { rows, .. } if !returning => Outcome::Affected(rows.len() as u64),
            out => out,
        });
    }
    let tx = if own_tx { Some(conn.begin().await?) } else { None };
    let exec: &dyn Executor = match &tx {
        Some(t) => t.as_ref(),
        None => conn,
    };
    let mut parts = Vec::with_capacity(plans.len());
    let mut returned = None;
    let mut affected = 0;
    let mut failed = None;
    for plan in plans {
        match run(exec, target, plan).await {
            Ok(Outcome::Rows { model, rows, types, .. }) => {
                affected += rows.len() as u64;
                returned = Some((model, types));
                parts.push(rows);
            }
            Ok(Outcome::Affected(n)) => affected += n,
            Ok(_) => unreachable!("an insert plan returns rows or a count"),
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    if let Some(t) = tx {
        match failed {
            None => t.commit().await?,
            Some(_) => {
                let _ = t.rollback().await;
            }
        }
    }
    if let Some(e) = failed {
        return Err(e);
    }
    Ok(match returned {
        Some((model, types)) if returning => {
            Outcome::Rows { model, rows: Box::new(ChainedRows::new(parts)), types, shape: None }
        }
        _ => Outcome::Affected(affected),
    })
}

/// A bulk load planned by [`plan_copy`]: run it with [`run_copy`].
pub struct Copy {
    table: String,
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
}

/// Plans `insert_many(rows, copy=True)`: Postgres `COPY ... FROM STDIN (FORMAT binary)`.
/// `rows` are converted by [`field_types`] (`None`: the column's default); client
/// defaults fill values first. COPY has no per-row `DEFAULT`, so a field must be set in
/// every row or in none. COPY writes values as they are, so models whose writes run
/// native code or SQL templates are rejected.
pub fn plan_copy(schema: &Schema, target: Target, model: &str, fields: &[String], rows: Vec<Vec<Option<Value>>>) -> Result<Copy> {
    if target.dialect != orm_core::dialect::Dialect::Postgres {
        return Err(Error::query("insert_many(copy=True) needs Postgres"));
    }
    #[cfg(feature = "model-composition")]
    if crate::composed::is_composed(schema, model)? {
        return Err(Error::query("insert_many(copy=True) does not support composed models"));
    }
    let m = schema.model(schema.model_idx(model).map_err(query_err)?);
    #[cfg(feature = "composition")]
    {
        crate::ownership::require_local_write(m)?;
        if !matches!(m.native, orm_core::behavior::NativeModel::None) {
            return Err(Error::query(format!("insert_many(copy=True): {} has native write behavior; use insert_many()", m.ir.name)));
        }
    }
    let (fields, rows) = crate::client_default::fill(m, fields, rows)?;
    #[cfg(feature = "file-storage")]
    crate::file_storage::rows(m, &fields, &rows)?;
    let mut columns = vec![];
    let mut keep = vec![];
    for (i, name) in fields.iter().enumerate() {
        let f = m.field(name).map_err(query_err)?;
        if f.write_sql.is_some() && f.enum_name.is_none() {
            return Err(Error::query(format!("insert_many(copy=True): {}.{name} writes through SQL; use insert_many()", m.ir.name)));
        }
        let set = rows.iter().filter(|r| r.get(i).is_some_and(Option::is_some)).count();
        if set == 0 {
            continue; // the database default for every row
        }
        if set != rows.len() {
            return Err(Error::query(format!(
                "insert_many(copy=True): {}.{name} is set in some rows only; COPY has no per-row DEFAULT",
                m.ir.name
            )));
        }
        columns.push(f.column.clone());
        keep.push(i);
    }
    if columns.is_empty() && !rows.is_empty() {
        return Err(Error::query("insert_many(copy=True) needs at least one field"));
    }
    let rows = rows
        .into_iter()
        .map(|mut r| keep.iter().map(|&i| r[i].take().expect("checked above")).collect())
        .collect();
    Ok(Copy { table: m.table().to_owned(), columns, rows })
}

/// Runs a [`plan_copy`] load on `conn` (the pool or a transaction): the rows written.
pub async fn run_copy(conn: &dyn Executor, copy: Copy) -> Result<u64> {
    if copy.rows.is_empty() {
        return Ok(0);
    }
    Ok(conn.copy_in(copy.table, copy.columns, copy.rows).await?)
}

/// `update_many`'s statements, SQL built: run them with [`run_update_many`].
pub struct UpdateMany {
    pub statements: Vec<(String, Vec<Value>)>,
    /// The model and column types of the returned rows, with `returning`.
    pub returning: Option<(usize, Vec<ValueType>)>,
}

/// Plans `update_many`: `rows` (aligned with `fields`, primary key first, converted by
/// [`field_types`]) are split so no statement exceeds the dialect's parameter limit or
/// `batch_size` rows. `filters` (planned against `params`) restrict the rows updated.
#[allow(clippy::too_many_arguments)]
pub fn plan_update_many(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: Vec<Vec<Value>>,
    filters: &[ir::Expr],
    params: &dyn Params,
    returning: bool,
    batch_size: Option<usize>,
    without_defaults: bool,
) -> Result<(Vec<UpdateStatement>, UpdateMany)> {
    if rows.iter().any(|r| r.len() != fields.len()) {
        return Err(Error::query("update_many row length does not match fields"));
    }
    #[cfg(feature = "composition")]
    let rows = {
        let m = schema.model(schema.model_idx(model).map_err(query_err)?);
        let mut rows = rows;
        let positions: Vec<_> = fields.iter().map(|name| m.field_pos(name).map_err(query_err)).collect::<Result<_>>()?;
        let map = crate::behavior::input_map(m.fields().len(), &positions)?;
        if positions.iter().skip(1).any(|p| m.native.computed().contains(p)) { return Err(Error::query("computed fields are read-only")); }
        crate::behavior::update_values(m.native, &map, &mut rows, fields.len())?;
        rows
    };
    #[cfg(feature = "file-storage")]
    {
        let m = schema.model(schema.model_idx(model).map_err(query_err)?);
        for row in &rows {
            for (name, v) in fields.iter().zip(row) {
                crate::file_storage::value(m, m.field_pos(name).map_err(query_err)?, v)?;
            }
        }
    }
    let per_row = if target.caps.update_from_values { fields.len() } else { 2 * fields.len() - 1 };
    // Leave room for the filters' parameters.
    let mut chunk = target.caps.max_params.saturating_sub(params.len()) / per_row.max(1);
    if let Some(n) = batch_size {
        chunk = chunk.min(n);
    }
    let (stmts, types) = plan::plan_update_many(schema, target, model, fields, &rows, chunk, filters, params, returning, without_defaults)?;
    let model_idx = schema.model_idx(model).map_err(query_err)?;
    let statements = stmts.iter().map(|s| db::build(target.dialect, s)).collect();
    Ok((stmts, UpdateMany { statements, returning: types.map(|t| (model_idx, t)) }))
}

/// Runs `update_many`'s statements, in a transaction of their own when there are several
/// and `conn` isn't one already (`own_tx`). Gives the rows updated, or the rows.
pub async fn run_update_many(conn: &dyn Executor, um: UpdateMany, own_tx: bool) -> Result<Outcome> {
    let tx = if own_tx && um.statements.len() > 1 { Some(conn.begin().await?) } else { None };
    let exec: &dyn Executor = match &tx {
        Some(t) => t.as_ref(),
        None => conn,
    };
    let mut count = 0u64;
    let mut fetched = vec![];
    let mut failed = None;
    for (sql, args) in um.statements {
        let r = match &um.returning {
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
            None => t.commit().await?,
            Some(_) => {
                let _ = t.rollback().await;
            }
        }
    }
    if let Some(e) = failed {
        return Err(e.into());
    }
    Ok(match um.returning {
        None => Outcome::Affected(count),
        Some((model, types)) => Outcome::Rows { model, rows: Box::new(ChainedRows::new(fetched)), types, shape: None },
    })
}

/// Runs `statements` in order, in one transaction (Postgres DDL is transactional), or
/// as part of the transaction `conn` already is (`in_tx`).
pub async fn run_script(conn: Arc<dyn Executor>, statements: Vec<String>, in_tx: bool) -> Result<()> {
    if in_tx {
        for s in statements {
            conn.batch(s).await?;
        }
        return Ok(());
    }
    let tx = conn.begin().await?;
    for s in statements {
        if let Err(e) = tx.batch(s).await {
            let _ = tx.rollback().await;
            return Err(e.into());
        }
    }
    Ok(tx.commit().await?)
}

/// Runs schema DDL with SQLite foreign key validation at commit (also handles cycles).
pub async fn run_schema_script(conn: Arc<dyn Executor>, statements: Vec<String>) -> Result<()> {
    let tx = conn.begin_migration().await?;
    for s in statements {
        if let Err(e) = tx.batch(s).await {
            let _ = tx.rollback().await;
            return Err(e.into());
        }
    }
    Ok(tx.commit().await?)
}
