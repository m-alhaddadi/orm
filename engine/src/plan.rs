//! Query IR -> SQL statements.
//!
//! Relation paths are the interesting part:
//!
//! * In filters, every relation hop becomes a correlated `EXISTS` subquery. Conditions
//!   inside one `filter()` entry that go through the same relation are grouped into one
//!   subquery, so they must hold for the *same* related row; separate entries get
//!   separate subqueries. `NOT` is a grouping barrier: `~(User.posts.views > 10)` means
//!   "no post with more than 10 views". This matches Django's multi-valued relationship
//!   semantics without its duplicate rows (no JOIN fan-out, no DISTINCT needed).
//! * `select_related` and `order_by` follow to-one relations only, with `LEFT JOIN`s.
//! * `prefetch` loads relations with one extra `WHERE fk IN (...)` query each.
//! * A many-to-many relation is one hop through its join table: `EXISTS (SELECT 1
//!   FROM tags t2 JOIN post_tags t3 ON t3.tag_id = t2.id WHERE t3.post_id = posts.id
//!   AND ...)`, and its prefetch selects the join row's key next to each tag.

#[cfg(feature = "postgres")]
use sea_query::extension::postgres::PgExpr;

use sea_query::{
    self,
    Alias, IntoIden, DeleteStatement, Expr as SExpr, ExprTrait, InsertStatement,
    JoinType, LikeExpr, LockBehavior, LockType, Order as SOrder, Query, SelectStatement, UpdateStatement,
};

use crate::error::{query_err, Error, Result};
use crate::params::Params;
use orm_core::ir::{
    ArithOp, Assignment, CmpOp, ColType, Count, Cte, Delete, Expr, FieldIr, Frame, FrameKind, Lock, Operation, Order, ParamRef,
    Prefetch, RelKind, RelationIr, Select, SelectItem, Update, ValueType,
};
use orm_core::dialect::{Capabilities, Dialect, Target};
use orm_core::schema::{Model, Schema};

/// A `select_related` object inside each row: its columns start at `start` and it is
/// attached to the root object (`parent` None) or to an earlier join's object.
pub struct JoinShape {
    pub parent: Option<usize>,
    pub attr: String,
    pub model: usize,
    pub start: usize,
    pub pk_pos: usize,
    pub shape: Option<orm_core::behavior::ResultShape>,
}

/// How a statement's rows become objects.
pub enum Output {
    /// Instances of `model` (its fields first), with `select_related` objects attached.
    Instances { model: usize, shape: Option<orm_core::behavior::ResultShape>, joins: Vec<JoinShape> },
    /// `select(...)` rows: per item, `Some(width)` for an instance of `model` (taking
    /// that many columns) or `None` for one value.
    Rows { model: usize, items: Vec<Option<usize>> },
}

/// A relation loaded once the parent rows are known: `stmt` plus `WHERE <key> IN (...)`.
pub struct PrefetchPlan {
    /// Attribute of the parent objects the related objects go to.
    pub attr: String,
    /// A list per parent (to-many) or one object or None (to-one).
    pub many: bool,
    /// Position and type of the relation's `from` field in the parent rows.
    pub key_pos: usize,
    pub key_type: ValueType,
    /// Position of the relation's `to` field in the related rows.
    pub child_key_pos: usize,
    /// The related model's to-one relation back to the parent, filled in too.
    pub back: Option<String>,
    stmt: SelectStatement,
    key: SExpr,
    key_field: FieldIr,
    /// A slice per parent: (offset, limit) over the `_rn` column `stmt` computes.
    slice: Option<(u64, Option<u64>)>,
    pub types: Vec<ValueType>,
    pub output: Output,
    #[cfg(feature = "composition")]
    pub computations: Vec<crate::behavior::Computation>,
    pub children: Vec<PrefetchPlan>,
}

impl PrefetchPlan {
    /// The statement for these parent keys (non-NULL, distinct).
    pub fn statement(&self, keys: Vec<sea_query::Value>) -> SelectStatement {
        let mut stmt = self.stmt.clone();
        stmt.and_where(self.key.clone().is_in(keys.into_iter().map(|k| bind(k, Some(&self.key_field)))));
        let Some((offset, limit)) = self.slice else { return stmt };
        let mut outer = Query::select();
        outer.column(sea_query::Asterisk).from_subquery(stmt, Alias::new("p"));
        if offset > 0 {
            outer.and_where(col("p", "_rn").gt(offset));
        }
        if let Some(n) = limit {
            outer.and_where(col("p", "_rn").lte(offset + n));
        }
        outer.order_by_expr(col("p", "_rn"), SOrder::Asc);
        outer
    }
}

pub struct SelectPlan {
    pub stmt: SelectStatement,
    pub types: Vec<ValueType>,
    pub output: Output,
    #[cfg(feature = "composition")]
    pub computations: Vec<crate::behavior::Computation>,
    pub prefetch: Vec<PrefetchPlan>,
}

pub enum Plan {
    Select(SelectPlan),
    Count(SelectStatement),
    Exists(SelectStatement),
    /// The model and column types of the returned rows when the update has `RETURNING`.
    Update(UpdateStatement, Option<Returned>),
    /// The model and column types of the returned rows when the delete has `RETURNING`.
    Delete(DeleteStatement, Option<Returned>),
    /// `INSERT ... RETURNING`: the model and column types of the returned rows.
    Insert(InsertStatement, (usize, Vec<ValueType>)),
    #[cfg(feature = "model-composition")]
    ComposedInsert(Box<crate::composed::Insert>),
    #[cfg(feature = "model-composition")]
    ComposedMutation(Box<crate::composed::Mutation>),
}

/// What a bound value is compared with or assigned to: its type drives the conversion
/// and its field the `write_sql` template.
#[derive(Clone, Copy, Default)]
struct Hint<'s> {
    ty: Option<ValueType>,
    field: Option<&'s FieldIr>,
}

impl<'s> Hint<'s> {
    fn ty(ty: ColType) -> Self {
        Hint { ty: Some(ValueType::scalar(ty)), field: None }
    }

    fn or(self, other: Hint<'s>) -> Self {
        if self.ty.is_some() {
            self
        } else {
            other
        }
    }
}

struct Scope {
    path: Vec<String>,
    model: usize,
    alias: String,
}

fn col(alias: &str, column: &str) -> SExpr {
    SExpr::col((Alias::new(alias), Alias::new(column)))
}

/// A column as it is read: through the field's `read_sql` template, if any.
fn read_col(alias: &str, f: &FieldIr) -> SExpr {
    match &f.read_sql {
        Some(t) => SExpr::cust_with_expr(t.replace("{}", "$1"), col(alias, &f.column)),
        None => col(alias, &f.column),
    }
}

/// A column in `RETURNING` (unqualified), through `read_sql` if any.
pub(crate) fn returning_col(f: &FieldIr) -> SExpr {
    let c = SExpr::col(Alias::new(&f.column));
    match &f.read_sql {
        Some(t) => SExpr::cust_with_expr(t.replace("{}", "$1"), c),
        None => c,
    }
}

/// A bound value for a field: through the field's `write_sql` template, if any.
pub(crate) fn bind(v: sea_query::Value, f: Option<&FieldIr>) -> SExpr {
    match f.and_then(|f| f.write_sql.as_ref()) {
        Some(t) => SExpr::cust_with_expr(t.replace("{}", "$1"), SExpr::val(v)),
        None => SExpr::val(v),
    }
}

/// Internal expression templates use numbered slots. SQLite's builder consumes
/// unnumbered slots, so expand/reorder expressions while translating the template.
fn template(dialect: Dialect, sql: impl Into<String>, exprs: Vec<SExpr>) -> SExpr {
    let sql = sql.into();
    if dialect == Dialect::Postgres { return SExpr::cust_with_exprs(sql, exprs); }
    let mut rendered = String::new();
    let mut values = vec![];
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek().is_some_and(|c| c.is_ascii_digit()) {
            let mut index = String::new();
            while chars.peek().is_some_and(|c| c.is_ascii_digit()) { index.push(chars.next().unwrap()); }
            let index: usize = index.parse().expect("internal template slot");
            values.push(exprs[index - 1].clone());
            rendered.push('?');
        } else { rendered.push(c); }
    }
    SExpr::cust_with_exprs(rendered, values)
}

fn fold(items: Vec<SExpr>, and: bool) -> SExpr {
    let mut it = items.into_iter();
    match it.next() {
        None => SExpr::cust(if and { "TRUE" } else { "FALSE" }),
        Some(first) => it.fold(first, |acc, e| if and { acc.and(e) } else { acc.or(e) }),
    }
}

const AGGREGATES: [&str; 5] = ["count", "sum", "avg", "min", "max"];
const SCALAR_FUNCS: [&str; 16] = [
    "lower", "upper", "length", "abs", "coalesce", "now", "cardinality", "concat", "trim", "ltrim", "rtrim", "replace",
    "substr", "strpos", "element", "unnest",
];
/// Functions whose arguments are strings: their parameters bind as text.
const TEXT_FUNCS: [&str; 7] = ["concat", "trim", "ltrim", "rtrim", "replace", "substr", "strpos"];
/// Functions that only exist with `OVER (...)`.
const WINDOW_FUNCS: [&str; 11] = [
    "row_number", "rank", "dense_rank", "percent_rank", "cume_dist", "ntile", "lag", "lead", "first_value",
    "last_value", "nth_value",
];

fn is_aggregate(name: &str) -> bool {
    AGGREGATES.contains(&name)
}

/// Whether `e` contains an aggregate computed in this query (not in a subquery or a
/// window).
fn has_local_aggregate(e: &Expr) -> bool {
    match e {
        Expr::Func { name, args, rel, .. } => {
            let mut paths = vec![];
            let mut has_not = false;
            for a in args {
                col_paths_all(a, &mut paths, &mut has_not);
            }
            (is_aggregate(name) && rel.is_none() && paths.iter().all(|p| p.is_empty()))
                || args.iter().any(has_local_aggregate)
        }
        Expr::Cmp { l, r, .. } | Expr::Arith { l, r, .. } => has_local_aggregate(l) || has_local_aggregate(r),
        Expr::And { items } | Expr::Or { items } => items.iter().any(has_local_aggregate),
        Expr::Not { item } | Expr::IsNull { item, .. } => has_local_aggregate(item),
        _ => false,
    }
}

/// Whether `e` contains a window function (outside subqueries).
fn has_window(e: &Expr) -> bool {
    match e {
        Expr::Window { .. } => true,
        Expr::Func { args, .. } => args.iter().any(has_window),
        Expr::Cmp { l, r, .. } | Expr::Arith { l, r, .. } => has_window(l) || has_window(r),
        Expr::And { items } | Expr::Or { items } => items.iter().any(has_window),
        Expr::In { item, values, .. } => has_window(item) || values.iter().any(has_window),
        Expr::Not { item } | Expr::IsNull { item, .. } | Expr::Like { item, .. } | Expr::InSelect { item, .. } => {
            has_window(item)
        }
        _ => false,
    }
}

/// Like `col_paths`, but also inside aggregates.
fn col_paths_all<'e>(e: &'e Expr, out: &mut Vec<&'e [String]>, has_not: &mut bool) {
    match e {
        Expr::Func { args, .. } => {
            for a in args {
                col_paths_all(a, out, has_not);
            }
        }
        other => col_paths(other, out, has_not),
    }
}

/// Collects every column path referenced by `e`; sets `has_not` if `e` contains a `NOT`.
/// Aggregates and subqueries are opaque: an aggregate over a relation is its own
/// correlated subquery, so its paths don't make the surrounding condition an `EXISTS`.
fn col_paths<'e>(e: &'e Expr, out: &mut Vec<&'e [String]>, has_not: &mut bool) {
    match e {
        Expr::Col { path, .. } => out.push(path),
        Expr::Param { .. }
        | Expr::Const { .. }
        | Expr::Excluded { .. }
        | Expr::Text { .. }
        | Expr::Int { .. }
        | Expr::Outer { .. }
        | Expr::CteCol { .. }
        | Expr::Exists { .. }
        | Expr::Subquery { .. } => {}
        Expr::Func { name, args, .. } => {
            if !is_aggregate(name) {
                for a in args {
                    col_paths(a, out, has_not);
                }
            }
        }
        // A window is computed over the query's own rows: its paths are read here.
        Expr::Window { func, partition_by, order_by, .. } => {
            col_paths_all(func, out, has_not);
            for p in partition_by {
                col_paths(p, out, has_not);
            }
            for o in order_by {
                col_paths(&o.expr, out, has_not);
            }
        }
        Expr::InSelect { item, .. } => col_paths(item, out, has_not),
        Expr::Cmp { l, r, .. } | Expr::Arith { l, r, .. } => {
            col_paths(l, out, has_not);
            col_paths(r, out, has_not);
        }
        Expr::And { items } | Expr::Or { items } => {
            for i in items {
                col_paths(i, out, has_not);
            }
        }
        Expr::Not { item } => {
            *has_not = true;
            col_paths(item, out, has_not);
        }
        Expr::In { item, values, .. } => {
            col_paths(item, out, has_not);
            for v in values {
                col_paths(v, out, has_not);
            }
        }
        Expr::IsNull { item, .. } => col_paths(item, out, has_not),
        Expr::Like { item, pattern, .. } => {
            col_paths(item, out, has_not);
            col_paths(pattern, out, has_not);
        }
    }
}

/// `ROWS BETWEEN <start> AND <end>` bounds: `None` unbounded, 0 the current row,
/// negative preceding, positive following.
fn frame_bound(b: Option<i64>, start: bool) -> String {
    match b {
        None if start => "UNBOUNDED PRECEDING".into(),
        None => "UNBOUNDED FOLLOWING".into(),
        Some(0) => "CURRENT ROW".into(),
        Some(n) if n < 0 => format!("{} PRECEDING", n.unsigned_abs()),
        Some(n) => format!("{n} FOLLOWING"),
    }
}

/// The columns of each CTE in `ctes`, as models without tables (indexed after the
/// schema's models), so queries can read them like tables.
pub fn derive_ctes(schema: &Schema, target: Target, ctes: &[Cte], params: &dyn Params) -> Result<Vec<Model>> {
    let mut virt: Vec<Model> = vec![];
    for cte in ctes {
        if schema.model_idx(&cte.name).is_ok() || virt.iter().any(|m| m.ir.name == cte.name) {
            return Err(Error::query(format!("CTE name {:?} is already taken", cte.name)));
        }
        let q = &cte.query;
        let p = Planner::new(schema, &virt, target, &q.model, q.from.as_deref(), params, vec![], 0)?;
        let mut fields = vec![];
        let model_fields = |fields: &mut Vec<FieldIr>| {
            for f in p.model(p.root).fields() {
                let mut f = f.clone();
                f.primary_key = false;
                fields.push(f);
            }
        };
        match &q.columns {
            None => model_fields(&mut fields),
            Some(items) => {
                for item in items {
                    match item {
                        SelectItem::Model => model_fields(&mut fields),
                        SelectItem::Expr { expr, name } => {
                            let name = name.as_deref().ok_or_else(|| Error::query("CTE columns need names"))?;
                            let vt = p.expr_type(expr)?;
                            let mut f = FieldIr::plain(name, vt.ty);
                            (f.array, f.enum_idx) = (vt.array, vt.enum_idx);
                            fields.push(f);
                        }
                    }
                }
            }
        }
        drop(p);
        virt.push(Model::derived(&cte.name, fields).map_err(query_err)?);
    }
    Ok(virt)
}

pub struct Planner<'s> {
    schema: &'s Schema,
    /// The statement's CTEs, as models (see `derive_ctes`).
    virt: &'s [Model],
    params: &'s dyn Params,
    root: usize,
    /// What the root rows are read from: the model's table, or a CTE.
    source: String,
    /// Innermost last. `scopes[0]` is the root, referenced by its own name (unless an
    /// enclosing query uses that name) so the same filters work in SELECT, UPDATE and
    /// DELETE.
    scopes: Vec<Scope>,
    /// Top-level to-one LEFT JOINs (select_related / order_by), keyed by path.
    joins: Vec<(Scope, SExpr)>,
    /// The roots of enclosing queries (alias, model), innermost last: `outer()`.
    outer: Vec<(String, usize)>,
    /// In the recursive part of a CTE: its name, and whether the query read its columns
    /// (which adds it to `FROM`, unless the query joins it).
    recursive: Option<(String, bool)>,
    /// CTEs joined to the root rows (`joins`), readable through `CteCol`.
    joined: Vec<String>,
    /// The query's named windows (`WINDOW`), which window functions can refer to.
    windows: Vec<String>,
    next_alias: usize,
    /// `EXCLUDED.<field>` is only valid in an upsert's `DO UPDATE SET`.
    allow_excluded: bool,
    /// Window functions are only valid in the select list and `ORDER BY`.
    allow_window: bool,
    /// `unnest()` returns a set of rows: only valid in the select list.
    allow_unnest: bool,
    target: Target,
    caps: Capabilities,
    #[cfg(feature = "query-defaults")]
    policy_bypass: bool,
}

impl<'s> Planner<'s> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        schema: &'s Schema,
        virt: &'s [Model],
        target: Target,
        model: &str,
        from: Option<&str>,
        params: &'s dyn Params,
        outer: Vec<(String, usize)>,
        next_alias: usize,
    ) -> Result<Self> {
        crate::db::require_dialect(target.dialect)?;
        let mut p = Planner {
            schema,
            virt,
            params,
            root: 0,
            source: String::new(),
            scopes: vec![],
            joins: vec![],
            outer,
            recursive: None,
            joined: vec![],
            windows: vec![],
            next_alias,
            allow_excluded: false,
            allow_window: false,
            allow_unnest: false,
            target,
            caps: target.caps,
            #[cfg(feature = "query-defaults")] policy_bypass: false,
        };
        p.root = p.model_idx(model)?;
        p.source = match from {
            None => p.model(p.root).table().to_owned(),
            Some(cte) => {
                let cols = p.model(p.model_idx(cte)?);
                if p.root >= schema.models.len() || cols.ir.name != cte {
                    return Err(Error::query(format!("{cte:?} is not a CTE")));
                }
                for f in p.model(p.root).fields() {
                    if cols.field(&f.name).is_err() {
                        return Err(Error::query(format!(
                            "CTE {cte:?} has no column {:?}: build it from a query of {} (or a select() including it)",
                            f.name,
                            p.model(p.root).ir.name
                        )));
                    }
                }
                cte.to_owned()
            }
        };
        let alias =
            if p.outer.iter().any(|(a, _)| *a == p.source) { p.alias("s") } else { p.source.clone() };
        p.scopes.push(Scope { path: vec![], model: p.root, alias });
        Ok(p)
    }

    pub fn plan(schema: &'s Schema, target: Target, op: &Operation, params: &'s dyn Params) -> Result<Plan> {
        Ok(match op {
            Operation::Select(q) => Plan::Select(plan_select(schema, target, q, params)?),
            Operation::Count(q) | Operation::Exists(q) => {
                let virt = derive_ctes(schema, target, &q.with, params)?;
                let mut p = Planner::new(schema, &virt, target, &q.model, q.from.as_deref(), params, vec![], 0)?;
                let is_count = matches!(op, Operation::Count(_));
                let mut stmt = if is_count { p.count(q)? } else { p.exists(q)? };
                if let Some(w) = p.with_clause(&q.with)? {
                    stmt.with_cte(w);
                }
                if is_count {
                    Plan::Count(stmt)
                } else {
                    Plan::Exists(stmt)
                }
            }
            Operation::Update(q) => {
                let virt = derive_ctes(schema, target, &q.with, params)?;
                let mut p = Planner::new(schema, &virt, target, &q.model, None, params, vec![], 0)?;
                #[cfg(feature = "model-composition")]
                if crate::composed::is_composed(schema, &q.model)? {
                    let mut select: Select = serde_json::from_value(serde_json::json!({"model":q.model})).map_err(|e| query_err(e.to_string()))?;
                    select.filters = q.filters.clone(); select.with = q.with.clone();
                    let mut matched = p.build_select(&select)?.stmt;
                    let prior_joins = p.joins.len();
                    let expressions = p.composed_update_values(q)?;
                    for (scope, on) in p.joins.iter().skip(prior_joins) {
                        matched.join_as(JoinType::LeftJoin, Alias::new(p.model(scope.model).table()), Alias::new(&scope.alias), on.clone());
                    }
                    if let Some(with) = p.with_clause(&q.with)? { matched.with_cte(with); }
                    return Ok(Plan::ComposedMutation(Box::new(crate::composed::prepare_update(schema, target, q, params, expressions, matched)?)));
                }
                let (mut stmt, types) = p.update(q)?;
                if let Some(w) = p.with_clause(&q.with)? {
                    stmt.with_cte(w);
                }
                Plan::Update(stmt, types.map(|(types, shape)| (p.root, types, shape)))
            }
            Operation::Delete(q) => {
                #[cfg(feature = "model-composition")]
                if crate::composed::is_composed(schema, &q.model)? {
                    return Ok(Plan::ComposedMutation(Box::new(crate::composed::prepare_delete(schema, target, q, params)?)));
                }
                let virt = derive_ctes(schema, target, &q.with, params)?;
                let mut p = Planner::new(schema, &virt, target, &q.model, None, params, vec![], 0)?;
                let (mut stmt, types) = p.delete(q)?;
                if let Some(w) = p.with_clause(&q.with)? {
                    stmt.with_cte(w);
                }
                Plan::Delete(stmt, types.map(|(types, shape)| (p.root, types, shape)))
            }
        })
    }

    fn model(&self, idx: usize) -> &'s Model {
        let n = self.schema.models.len();
        if idx < n {
            self.schema.model(idx)
        } else {
            &self.virt[idx - n]
        }
    }

    fn model_idx(&self, name: &str) -> Result<usize> {
        match self.schema.model_idx(name) {
            Ok(i) => Ok(i),
            Err(e) => match self.virt.iter().position(|m| m.ir.name == name) {
                Some(i) => Ok(self.schema.models.len() + i),
                None => Err(query_err(e)),
            },
        }
    }

    fn walk(&self, root: usize, path: &[String]) -> Result<usize> {
        let mut cur = root;
        for hop in path {
            cur = self.model(cur).relation(hop).map_err(query_err)?.1;
        }
        Ok(cur)
    }

    /// `QueryError` unless the dialect supports `feature`.
    fn require(&self, supported: bool, feature: &str) -> Result<()> {
        self.target.require(supported, feature).map_err(query_err)
    }

    fn alias(&mut self, prefix: &str) -> String {
        self.next_alias += 1;
        format!("{prefix}{}", self.next_alias)
    }

    fn scope(&self) -> &Scope {
        self.scopes.last().expect("root scope")
    }

    fn root_alias(&self) -> &str {
        &self.scopes[0].alias
    }

    /// Takes note of what `q` declares for its expressions: joined CTEs, named windows.
    fn enter(&mut self, q: &Select) -> Result<()> {
        #[cfg(feature = "query-defaults")] { self.policy_bypass = q.without_defaults; }
        self.joined.clear();
        for j in &q.joins {
            let idx = self.model_idx(&j.cte)?;
            if idx < self.schema.models.len() {
                return Err(Error::query(format!("join() takes a CTE, {:?} is a model", j.cte)));
            }
            if j.cte == self.source || <[String]>::contains(&self.joined, &j.cte) {
                return Err(Error::query(format!("CTE {:?} is read twice by one query", j.cte)));
            }
            self.joined.push(j.cte.clone());
        }
        self.windows = q.windows.iter().map(|w| w.name.clone()).collect();
        for w in &self.windows {
            if w.is_empty() || !w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                return Err(Error::query(format!("invalid window name {w:?}")));
            }
        }
        Ok(())
    }

    /// `WINDOW <name> AS (...)` for the query's named windows. sea-query holds one per
    /// statement and writes it after `ORDER BY` / `LIMIT` / `FOR ...`, where Postgres
    /// rejects it, so those combinations raise until the SQL builder changes.
    fn declare_windows(&mut self, q: &Select, stmt: &mut SelectStatement) -> Result<()> {
        let w = match q.windows.as_slice() {
            [] => return Ok(()),
            [w] => w,
            _ => {
                return Err(query_err(
                    "one query can declare only one named window for now; define the others inline \
                     with .over(partition_by=..., order_by=...)"
                        .into(),
                ))
            }
        };
        if !q.order.is_empty() || q.limit.is_some() || q.offset.is_some() || q.lock.is_some() {
            return Err(query_err(
                "a named window can't be combined with order_by(), slicing or lock() yet (the SQL builder \
                 writes WINDOW after them); define the window inline with .over(partition_by=..., order_by=...)"
                    .into(),
            ));
        }
        let mut spec = sea_query::WindowStatement::new();
        for p in &w.partition_by {
            self.join_paths(p, "a window")?;
            let e = self.value(p, Hint::default())?;
            sea_query::OverStatement::add_partition_by(&mut spec, e);
        }
        for o in &w.order_by {
            self.join_paths(&o.expr, "a window")?;
            let e = self.value(&o.expr, Hint::default())?;
            spec.order_by_expr(e, if o.desc { SOrder::Desc } else { SOrder::Asc });
        }
        if let Some(f) = &w.frame {
            let bound = |b: Option<i64>, start: bool| -> Result<sea_query::Frame> {
                let n = |v: i64| u32::try_from(v.unsigned_abs()).map_err(|_| Error::query("frame bound too large"));
                Ok(match b {
                    None if start => sea_query::Frame::UnboundedPreceding,
                    None => sea_query::Frame::UnboundedFollowing,
                    Some(0) => sea_query::Frame::CurrentRow,
                    Some(v) if v < 0 => sea_query::Frame::Preceding(n(v)?),
                    Some(v) => sea_query::Frame::Following(n(v)?),
                })
            };
            let kind = match f.kind {
                FrameKind::Rows => sea_query::FrameType::Rows,
                FrameKind::Range => sea_query::FrameType::Range,
            };
            spec.frame_between(kind, bound(f.start, true)?, bound(f.end, false)?);
        }
        stmt.window(Alias::new(&w.name), spec);
        Ok(())
    }

    // -- subqueries and CTEs ----------------------------------------------------------------

    /// A planner for a subquery: it sees this query's root (and the ones enclosing it)
    /// through `outer()`.
    fn child(&self, q: &Select) -> Result<Planner<'s>> {
        if !q.with.is_empty() {
            return Err(Error::query("a subquery can't declare CTEs; declare them on the outermost query"));
        }
        let mut outer = self.outer.clone();
        outer.push((self.root_alias().to_owned(), self.root));
        Planner::new(self.schema, self.virt, self.target, &q.model, q.from.as_deref(), self.params, outer, self.next_alias)
    }

    /// The statement of a subquery. For `EXISTS` a query without columns selects `1`.
    fn subselect(&mut self, q: &Select, exists: bool) -> Result<SelectStatement> {
        if q.lock.is_some() {
            return Err(Error::query("a subquery can't lock rows"));
        }
        let mut child = self.child(q)?;
        let stmt = if exists && q.columns.is_none() { child.sliced_inner(q)? } else { child.build_select(q)?.stmt };
        self.next_alias = child.next_alias;
        Ok(stmt)
    }

    /// The single column of a subquery used as a value.
    fn one_column<'q>(q: &'q Select, what: &str) -> Result<&'q Expr> {
        match q.columns.as_deref() {
            Some([SelectItem::Expr { expr, .. }]) => Ok(expr),
            _ => Err(Error::query(format!("{what} takes a query that selects exactly one column"))),
        }
    }

    /// `WITH [RECURSIVE] <name> (<columns>) AS (...), ...` for this statement's CTEs.
    fn with_clause(&mut self, ctes: &[Cte]) -> Result<Option<sea_query::WithClause>> {
        if ctes.is_empty() {
            return Ok(None);
        }
        let mut clause = sea_query::WithClause::new();
        let mut recursive = false;
        for cte in ctes {
            let cols = self.model(self.model_idx(&cte.name)?);
            let mut body = self.cte_body(&cte.query, None)?;
            if let Some(rec) = &cte.recursive {
                recursive = true;
                let step = self.cte_body(rec, Some(&cte.name))?;
                let ty = if cte.distinct { sea_query::UnionType::Distinct } else { sea_query::UnionType::All };
                body.union(ty, step);
            }
            let mut c = sea_query::CommonTableExpression::new();
            c.table_name(Alias::new(&cte.name)).columns(cols.fields().iter().map(|f| Alias::new(&f.column)));
            c.query(body);
            if let Some(m) = cte.materialized {
                c.materialized(m);
            }
            clause.cte(c);
        }
        clause.recursive(recursive);
        Ok(Some(clause))
    }

    /// A CTE's query: its columns as stored (no `read_sql`), named after the CTE's columns.
    fn cte_body(&mut self, q: &Select, recursive: Option<&str>) -> Result<SelectStatement> {
        if !q.with.is_empty() || !q.prefetch.is_empty() || !q.select_related.is_empty() || q.lock.is_some() {
            return Err(Error::query("a CTE's query can't declare CTEs, prefetch, select_related or lock"));
        }
        let mut p =
            Planner::new(self.schema, self.virt, self.target, &q.model, q.from.as_deref(), self.params, vec![], self.next_alias)?;
        p.recursive = recursive.map(|r| (r.to_owned(), false));
        let model_only = [SelectItem::Model];
        let items = q.columns.as_deref().unwrap_or(&model_only);
        let plan = p.select_columns(q, items, true)?;
        if let Some(r) = recursive {
            let base = self.model(self.model_idx(r)?).fields().len();
            if plan.types.len() != base {
                return Err(Error::query(format!(
                    "the recursive part of CTE {r:?} has {} columns, its first part {base}",
                    plan.types.len()
                )));
            }
        }
        self.next_alias = p.next_alias;
        Ok(plan.stmt)
    }

    /// A column of a CTE: the one this query reads, or (in its recursive part) the CTE
    /// being defined.
    fn cte_col(&mut self, cte: &str, name: &str) -> Result<(String, &'s FieldIr)> {
        let f = self.model(self.model_idx(cte)?).field(name).map_err(query_err)?;
        if cte == self.source {
            return Ok((self.root_alias().to_owned(), f));
        }
        if self.joined.iter().any(|j| j == cte) {
            return Ok((cte.to_owned(), f));
        }
        if let Some((r, used)) = &mut self.recursive {
            if r == cte {
                *used = true;
                return Ok((cte.to_owned(), f));
            }
        }
        Err(Error::query(format!(
            "{cte}.c.{name} is only available in queries reading {cte} (from_({cte}), join({cte}, ...) or {cte}.select(...))"
        )))
    }

    // -- filters ------------------------------------------------------------------------

    fn apply_filters(&mut self, filters: &[Expr], _without_defaults: bool) -> Result<Vec<SExpr>> {
        #[cfg(feature = "query-defaults")] { self.policy_bypass = _without_defaults; }
        #[allow(unused_mut)]
        let mut out: Vec<SExpr> = filters.iter().map(|f| self.cond(f)).collect::<Result<_>>()?;
        #[cfg(feature = "query-defaults")]
        if !_without_defaults {
            if let Some(filter) = &self.model(self.root).query_defaults.filter { out.push(self.cond(filter)?); }
        }
        Ok(out)
    }

    /// The relation every column in `e` goes through next, if they all agree and `e`
    /// has no `NOT` (which must stay outside the subquery).
    fn pure_hop<'e>(&self, e: &'e Expr) -> Option<&'e str> {
        let (mut paths, mut has_not) = (vec![], false);
        col_paths(e, &mut paths, &mut has_not);
        if has_not || paths.is_empty() {
            return None;
        }
        let base = &self.scope().path;
        let mut hop: Option<&str> = None;
        for p in paths {
            if p.len() <= base.len() || !p.starts_with(base) {
                return None;
            }
            let h = p[base.len()].as_str();
            match hop {
                Some(prev) if prev != h => return None,
                _ => hop = Some(h),
            }
        }
        hop
    }

    /// First relation below the current scope referenced by a leaf condition.
    fn any_hop<'e>(&self, e: &'e Expr) -> Option<&'e str> {
        let (mut paths, mut has_not) = (vec![], false);
        col_paths(e, &mut paths, &mut has_not);
        let base = &self.scope().path;
        paths
            .into_iter()
            .find(|p| p.len() > base.len() && p.starts_with(base))
            .map(|p| p[base.len()].as_str())
    }

    /// Adds the target rows of `rel`, linked to the source rows `src` (alias, model), to
    /// `stmt`: as its `FROM` (linked in `WHERE`) when `first`, otherwise as inner joins.
    /// A many-to-many relation goes through its join model. Returns the target's alias.
    fn add_hop(
        &mut self,
        stmt: &mut SelectStatement,
        first: bool,
        src: (&str, usize),
        rel: &RelationIr,
        target: usize,
        prefix: &str,
    ) -> Result<String> {
        let from_col = &self.model(src.1).field(&rel.from).map_err(query_err)?.column;
        let tm = self.model(target);
        let to_col = &tm.field(&rel.to).map_err(query_err)?.column;
        let table = Alias::new(tm.table());
        let alias = self.alias(prefix);
        match &rel.through {
            None => {
                let link = col(&alias, to_col).eq(col(src.0, from_col));
                if first {
                    stmt.from_as(table, Alias::new(&alias)).and_where(link);
                } else {
                    stmt.join_as(JoinType::InnerJoin, table, Alias::new(&alias), link);
                }
            }
            Some(th) => {
                let jm = self.model(self.model_idx(&th.model)?);
                let jalias = self.alias(prefix);
                let jtable = Alias::new(jm.table());
                let to_target = col(&jalias, &jm.field(&th.target).map_err(query_err)?.column).eq(col(&alias, to_col));
                let to_source = col(&jalias, &jm.field(&th.source).map_err(query_err)?.column).eq(col(src.0, from_col));
                if first {
                    stmt.from_as(table, Alias::new(&alias))
                        .join_as(JoinType::InnerJoin, jtable, Alias::new(&jalias), to_target)
                        .and_where(to_source);
                } else {
                    stmt.join_as(JoinType::InnerJoin, jtable, Alias::new(&jalias), to_source)
                        .join_as(JoinType::InnerJoin, table, Alias::new(&alias), to_target);
                }
            }
        }
        #[cfg(feature = "query-defaults")]
        if !self.policy_bypass { if let Some(filter) = &tm.query_defaults.filter { stmt.and_where(self.target_default(target, &alias, filter)?); } }
        Ok(alias)
    }

    /// `EXISTS (SELECT 1 FROM <target> AS tN WHERE tN.to = <scope>.from AND <body>)`
    /// (through the join table for a many-to-many relation).
    /// TODO: skip a hop whose keys line up with the next one (see `ensure_join`).
    fn exists_via(
        &mut self,
        hop: &str,
        body: impl FnOnce(&mut Self) -> Result<SExpr>,
    ) -> Result<SExpr> {
        let cur = self.scope();
        let cur_model = self.model(cur.model);
        let (rel, target) = cur_model.relation(hop).map_err(query_err)?;
        let mut path = cur.path.clone();
        path.push(hop.to_owned());
        let outer_alias = cur.alias.clone();
        let cur_model_idx = cur.model;

        let mut sub = Query::select();
        sub.expr(SExpr::val(1));
        let alias = self.add_hop(&mut sub, true, (&outer_alias, cur_model_idx), rel, target, "t")?;
        self.scopes.push(Scope { path, model: target, alias });
        let body = body(self);
        self.scopes.pop();
        sub.and_where(body?);
        Ok(SExpr::exists(sub))
    }

    fn cond(&mut self, e: &Expr) -> Result<SExpr> {
        match e {
            Expr::And { items } | Expr::Or { items } => {
                let is_and = matches!(e, Expr::And { .. });
                // Children that live entirely below the same relation share one subquery.
                let mut groups: Vec<(&str, Vec<&Expr>)> = vec![];
                let mut parts = vec![];
                for item in items {
                    match self.pure_hop(item) {
                        Some(h) => match groups.iter_mut().find(|(g, _)| *g == h) {
                            Some((_, members)) => members.push(item),
                            None => groups.push((h, vec![item])),
                        },
                        None => parts.push(self.cond(item)?),
                    }
                }
                for (hop, members) in groups {
                    parts.push(self.exists_via(hop, |p| {
                        let inner = members.iter().map(|m| p.cond(m)).collect::<Result<Vec<_>>>()?;
                        Ok(fold(inner, is_and))
                    })?);
                }
                Ok(fold(parts, is_and))
            }
            Expr::Not { item } => Ok(self.cond(item)?.not()),
            Expr::Const { value } => Ok(SExpr::cust(if *value { "TRUE" } else { "FALSE" })),
            leaf => match self.any_hop(leaf) {
                Some(hop) => self.exists_via(hop, |p| p.cond(leaf)),
                None => self.leaf(leaf),
            },
        }
    }

    fn leaf(&mut self, e: &Expr) -> Result<SExpr> {
        Ok(match e {
            Expr::Cmp { op, l, r } => {
                if self.target.dialect == Dialect::Sqlite && matches!(op, CmpOp::Contains | CmpOp::ContainedBy | CmpOp::Overlaps) {
                    return Err(Error::query("sqlite does not support PostgreSQL containment or overlap operators"));
                }
                let hint = self.hint_of(l).or(self.hint_of(r));
                let l = self.value(l, hint)?;
                let r = self.value(r, hint)?;
                match op {
                    CmpOp::Eq => l.eq(r),
                    CmpOp::Ne => l.ne(r),
                    CmpOp::Lt => l.lt(r),
                    CmpOp::Le => l.lte(r),
                    CmpOp::Gt => l.gt(r),
                    CmpOp::Ge => l.gte(r),
                    CmpOp::Contains => SExpr::cust_with_exprs("$1 @> $2", [l, r]),
                    CmpOp::ContainedBy => SExpr::cust_with_exprs("$1 <@ $2", [l, r]),
                    CmpOp::Overlaps => SExpr::cust_with_exprs("$1 && $2", [l, r]),
                }
            }
            Expr::In { item, values, neg } => {
                let hint = self.hint_of(item);
                let item = self.value(item, hint)?;
                let values = values.iter().map(|v| self.value(v, hint)).collect::<Result<Vec<_>>>()?;
                if *neg {
                    item.is_not_in(values)
                } else {
                    item.is_in(values)
                }
            }
            Expr::InSelect { item, select, neg } => {
                let hint = self.hint_of(item);
                let item = self.value(item, hint)?;
                Self::one_column(select, "in_()")?;
                let sub = self.subselect(select, false)?;
                if *neg {
                    item.not_in_subquery(sub)
                } else {
                    item.in_subquery(sub)
                }
            }
            Expr::IsNull { item, neg } => {
                let item = self.value(item, Hint::default())?;
                if *neg {
                    item.is_not_null()
                } else {
                    item.is_null()
                }
            }
            Expr::Like { item, pattern, ci, neg } => {
                let item = self.value(item, Hint::default())?;
                let text = match pattern.as_ref() {
                    Expr::Param { i } => self.params.text(self.param(*i)?)?,
                    _ => return Err(Error::query("LIKE pattern must be a string parameter")),
                };
                if *ci && !self.caps.ilike {
                    // LOWER(x) LIKE LOWER(pattern), for databases without ILIKE.
                    let lowered = template(self.target.dialect, "LOWER($1)", vec![item]);
                    let pattern = LikeExpr::new(text.to_lowercase());
                    return Ok(if *neg { lowered.not_like(pattern) } else { lowered.like(pattern) });
                }
                let pattern = LikeExpr::new(text);
                match (ci, neg) {
                    (false, false) => item.like(pattern),
                    (false, true) => item.not_like(pattern),
                    #[cfg(feature = "postgres")]
                    (true, false) => item.ilike(pattern),
                    #[cfg(feature = "postgres")]
                    (true, true) => item.not_ilike(pattern),
                    #[cfg(not(feature = "postgres"))]
                    (true, _) => return Err(Error::query("ILIKE requires the postgres native capability")),
                }
            }
            other => self.value(other, Hint::ty(ColType::Bool))?,
        })
    }

    // -- value expressions --------------------------------------------------------------

    /// `i`, checked against the parameter list.
    fn param(&self, i: usize) -> Result<usize> {
        if i < self.params.len() {
            Ok(i)
        } else {
            Err(Error::query(format!("parameter {i} out of range")))
        }
    }

    fn outer_field(&self, depth: usize, name: &str) -> Result<(&str, &'s FieldIr)> {
        if depth == 0 || depth > self.outer.len() {
            return Err(Error::query(format!("outer() column {name:?} has no enclosing query at that depth")));
        }
        let (alias, model) = &self.outer[self.outer.len() - depth];
        #[cfg(feature = "composition")]
        self.reject_computed(self.model(*model), name)?;
        #[cfg(feature = "composition")]
        if self.model(*model).resolved_fields[self.model(*model).field_pos(name).map_err(query_err)?].storage.owner != self.model(*model).owner {
            return Err(Error::query("outer() on inherited storage requires an explicit owner projection"));
        }
        Ok((alias, self.model(*model).field(name).map_err(query_err)?))
    }

    #[cfg(feature = "composition")]
    fn reject_computed(&self, model: &Model, name: &str) -> Result<()> {
        if model.native.computed().contains(&model.field_pos(name).map_err(query_err)?) {
            return Err(Error::query("native computed fields cannot be filtered, ordered, aggregated, or used in expressions"));
        }
        Ok(())
    }

    fn hint_of(&self, e: &Expr) -> Hint<'s> {
        match e {
            Expr::Col { path, name } => {
                let field = self.walk(self.root, path).ok().and_then(|m| self.model(m).field(name).ok());
                Hint { ty: field.map(|f| f.value_type()), field }
            }
            Expr::Excluded { name } => {
                let field = self.model(self.root).field(name).ok();
                Hint { ty: field.map(|f| f.value_type()), field }
            }
            Expr::Outer { depth, name } => {
                let field = self.outer_field(*depth, name).ok().map(|(_, f)| f);
                Hint { ty: field.map(|f| f.value_type()), field }
            }
            Expr::CteCol { cte, name } => {
                let field = self.model_idx(cte).ok().and_then(|m| self.model(m).field(name).ok());
                Hint { ty: field.map(|f| f.value_type()), field }
            }
            // Arithmetic results are plain values: no write_sql cast.
            Expr::Arith { l, r, .. } => Hint { ty: self.hint_of(l).or(self.hint_of(r)).ty, field: None },
            Expr::Func { .. } | Expr::Subquery { .. } | Expr::Window { .. } => {
                Hint { ty: self.expr_type(e).ok(), field: None }
            }
            _ => Hint::default(),
        }
    }

    fn resolve(&self, path: &[String], name: &str) -> Result<SExpr> {
        let (alias, column) = self.resolve_parts(path, name)?;
        #[cfg(feature = "composition")]
        if let Some(scope) = self.scopes.iter().rev().chain(self.joins.iter().map(|(s, _)| s)).find(|s| s.path == path) {
            if scope.model < self.schema.models.len() {
                let model = self.model(scope.model);
                return crate::ownership::column(self.schema, model, &alias, model.field(name).map_err(query_err)?);
            }
        }
        Ok(col(&alias, &column))
    }

    #[cfg(not(feature = "composition"))]
    fn read_field(&self, _model: usize, alias: &str, field: &FieldIr) -> Result<SExpr> {
        Ok(read_col(alias, field))
    }

    #[cfg(feature = "composition")]
    fn read_field(&self, model: usize, alias: &str, field: &FieldIr) -> Result<SExpr> {
        let value = self.stored_field(model, alias, field)?;
        Ok(match &field.read_sql { Some(t) => SExpr::cust_with_expr(t.replace("{}", "$1"), value), None => value })
    }

    /// The stored value, without `read_sql`: CTE columns hold stored values, and the outer read applies `read_sql`.
    fn stored_field(&self, model: usize, alias: &str, field: &FieldIr) -> Result<SExpr> {
        #[cfg(feature = "composition")]
        if model < self.schema.models.len() {
            return crate::ownership::column(self.schema, self.model(model), alias, field);
        }
        #[cfg(not(feature = "composition"))]
        let _ = model;
        Ok(col(alias, &field.column))
    }

    fn value(&mut self, e: &Expr, hint: Hint<'s>) -> Result<SExpr> {
        Ok(match e {
            Expr::Col { path, name } => {
                #[cfg(feature = "composition")]
                {
                    let m = self.model(self.walk(self.root, path)?);
                    if m.native.computed().contains(&m.field_pos(name).map_err(query_err)?) {
                        return Err(Error::query("native computed fields cannot be filtered, ordered, aggregated, or used in expressions"));
                    }
                }
                self.resolve(path, name)?
            },
            Expr::Param { i } => bind(self.params.value(self.param(*i)?, hint.ty)?, hint.field),
            Expr::Const { value } => SExpr::val(*value),
            Expr::Int { value } => SExpr::cust(value.to_string()),
            Expr::Text { value } => bind(sea_query::Value::from(value.clone()), hint.field),
            Expr::Excluded { name } => {
                if !self.allow_excluded {
                    return Err(Error::query("excluded() can only be used in on_conflict(...).do_update()"));
                }
                #[cfg(feature = "composition")]
                self.reject_computed(self.model(self.root), name)?;
                let f = self.model(self.root).field(name).map_err(query_err)?;
                col("excluded", &f.column)
            }
            Expr::Outer { depth, name } => {
                let (alias, f) = self.outer_field(*depth, name)?;
                col(alias, &f.column)
            }
            Expr::CteCol { cte, name } => {
                let (alias, f) = self.cte_col(cte, name)?;
                col(&alias, &f.column)
            }
            Expr::Exists { select } => SExpr::exists(self.subselect(select, true)?),
            Expr::Subquery { select } => {
                Self::one_column(select, "as_scalar()")?;
                SExpr::SubQuery(None, Box::new(self.subselect(select, false)?.into()))
            }
            Expr::Window { func, base, partition_by, order_by, frame } => {
                self.window(func, base.as_deref(), partition_by, order_by, frame)?
            }
            Expr::Func { name, args, rel, distinct } => self.func(name, args, rel.as_deref(), *distinct)?,
            Expr::Arith { op, l, r } => {
                let inner = self.hint_of(l).or(self.hint_of(r));
                let hint = match op {
                    ArithOp::Concat => Hint { ty: Some(ValueType::scalar(ColType::Text)), field: None },
                    _ => Hint { ty: inner.ty.or(hint.ty), field: None },
                };
                let l = self.value(l, hint)?;
                let r = self.value(r, hint)?;
                match op {
                    ArithOp::Add => l.add(r),
                    ArithOp::Sub => l.sub(r),
                    ArithOp::Mul => l.mul(r),
                    ArithOp::Div => l.div(r),
                    ArithOp::Concat => template(self.target.dialect, "$1 || $2", vec![l, r]),
                }
            }
            cond => self.cond(cond)?,
        })
    }

    // -- functions ----------------------------------------------------------------------

    /// The column type of an expression's result, which decodes it.
    fn expr_type(&self, e: &Expr) -> Result<ValueType> {
        let scalar = ValueType::scalar;
        Ok(match e {
            Expr::Col { path, name } => {
                let m = self.walk(self.root, path)?;
                self.model(m).field(name).map_err(query_err)?.value_type()
            }
            Expr::Outer { depth, name } => self.outer_field(*depth, name)?.1.value_type(),
            Expr::CteCol { cte, name } => {
                self.model(self.model_idx(cte)?).field(name).map_err(query_err)?.value_type()
            }
            Expr::Int { .. } => scalar(ColType::BigInt),
            Expr::Text { .. } => scalar(ColType::Text),
            Expr::Subquery { select } => self.child(select)?.expr_type(Self::one_column(select, "as_scalar()")?)?,
            Expr::Window { func, .. } => self.expr_type(func)?,
            Expr::Arith { op: ArithOp::Concat, .. } => scalar(ColType::Text),
            Expr::Arith { l, r, .. } => match self.expr_type(l) {
                Ok(t) => t,
                Err(_) => self.expr_type(r)?,
            },
            Expr::Func { name, args, .. } => {
                let first = || match args.first() {
                    Some(a) => self.expr_type(a),
                    None => Err(Error::query(format!("{name}() needs an argument"))),
                };
                match name.as_str() {
                    "count" | "row_number" | "rank" | "dense_rank" => scalar(ColType::BigInt),
                    "percent_rank" | "cume_dist" => scalar(ColType::Float),
                    // AVG of decimals stays exact; of anything else it is a float
                    "avg" => match first()?.ty {
                        ColType::Decimal => scalar(ColType::Decimal),
                        _ => scalar(ColType::Float),
                    },
                    "length" | "ntile" | "cardinality" | "strpos" => scalar(ColType::Int),
                    "lower" | "upper" | "concat" | "trim" | "ltrim" | "rtrim" | "replace" | "substr" => scalar(ColType::Text),
                    "element" | "unnest" if self.target.dialect == Dialect::Sqlite => {
                        return Err(Error::query("sqlite does not support array element access or unnest()"))
                    }
                    "element" | "unnest" => match first()? {
                        t if t.array => t.element(),
                        _ => return Err(Error::query(format!("{} needs an array", if name == "element" { "an index" } else { "unnest()" }))),
                    },
                    "now" => scalar(ColType::DateTime),
                    // SUM of integers is cast to bigint (see `func`).
                    "sum" => match first()? {
                        t if matches!(t.ty, ColType::Int | ColType::BigInt) => scalar(ColType::BigInt),
                        t => t,
                    },
                    _ => first()?,
                }
            }
            Expr::Cmp { .. }
            | Expr::And { .. }
            | Expr::Or { .. }
            | Expr::Not { .. }
            | Expr::In { .. }
            | Expr::IsNull { .. }
            | Expr::Like { .. }
            | Expr::Const { .. }
            | Expr::InSelect { .. }
            | Expr::Exists { .. } => scalar(ColType::Bool),
            Expr::Param { .. } => {
                return Err(Error::query("select() takes columns and expressions, not plain values"))
            }
            Expr::Excluded { .. } => return Err(Error::query("excluded() is only valid in do_update()")),
        })
    }

    /// `name(args)`. An aggregate whose arguments (or `rel`) go through relations below
    /// the current scope is computed per row, in a correlated subquery over those
    /// relations: `func.count(User.posts)` is `(SELECT COUNT(*) FROM posts WHERE
    /// posts.author_id = users.id)`, never a JOIN that would multiply rows.
    fn func(&mut self, name: &str, args: &[Expr], rel: Option<&[String]>, distinct: bool) -> Result<SExpr> {
        if WINDOW_FUNCS.contains(&name) {
            return Err(Error::query(format!("{name}() is a window function: add .over(...)")));
        }
        if !is_aggregate(name) && !SCALAR_FUNCS.contains(&name) {
            return Err(Error::query(format!("unknown function {name}()")));
        }
        if is_aggregate(name) {
            let base = self.scope().path.clone();
            let mut paths = vec![];
            let mut has_not = false;
            for a in args {
                col_paths_all(a, &mut paths, &mut has_not);
            }
            let below: Vec<&[String]> = match rel {
                Some(r) => vec![r],
                None => paths.into_iter().filter(|p| p.len() > base.len() && p.starts_with(&base)).collect(),
            };
            if let Some(path) = below.first().map(|p| p.to_vec()) {
                if below.iter().any(|p| *p != path.as_slice()) {
                    return Err(Error::query(format!(
                        "{name}() over relations needs all its columns on one relation path"
                    )));
                }
                return self.aggregate_subquery(name, args, &path, rel.is_some(), distinct);
            }
            if rel.is_none() && args.is_empty() && name != "count" {
                return Err(Error::query(format!("{name}() needs an argument")));
            }
        }
        self.call(name, args, distinct)
    }

    /// `<func> OVER (PARTITION BY ... ORDER BY ... <frame>)`, over this query's rows.
    fn window(
        &mut self,
        func: &Expr,
        base: Option<&str>,
        partition_by: &[Expr],
        order_by: &[Order],
        frame: &Option<Frame>,
    ) -> Result<SExpr> {
        if !self.allow_window {
            return Err(query_err(
                "window functions can only be used in select() and order_by(), not in filters, \
                 group_by(), having() or updates; select them in a CTE to filter on them"
                    .into(),
            ));
        }
        let Expr::Func { name, args, rel, distinct } = func else {
            return Err(Error::query("over() applies to a function"));
        };
        if rel.is_some() || !(is_aggregate(name) || WINDOW_FUNCS.contains(&name.as_str())) {
            return Err(Error::query(format!("{name}() can't be used as a window function")));
        }
        let (call, cast) = self.call_parts(name, args, *distinct)?;
        let mut exprs = vec![call];
        let mut clauses = vec![];
        if let Some(b) = base {
            if !self.windows.iter().any(|w| w == b) {
                return Err(Error::query(format!("window {b:?} is not declared by this query")));
            }
            if !partition_by.is_empty() {
                return Err(Error::query("over(window) can't add partition_by: it comes from the window"));
            }
            clauses.push(b.to_owned());
        }
        if !partition_by.is_empty() {
            let mut slots = vec![];
            for p in partition_by {
                exprs.push(self.value(p, Hint::default())?);
                slots.push(format!("${}", exprs.len()));
            }
            clauses.push(format!("PARTITION BY {}", slots.join(", ")));
        }
        if !order_by.is_empty() {
            let mut slots = vec![];
            for o in order_by {
                exprs.push(self.value(&o.expr, Hint::default())?);
                slots.push(format!("${}{}", exprs.len(), if o.desc { " DESC" } else { "" }));
            }
            clauses.push(format!("ORDER BY {}", slots.join(", ")));
        }
        if let Some(f) = frame {
            let kind = match f.kind {
                FrameKind::Rows => "ROWS",
                FrameKind::Range => "RANGE",
            };
            clauses.push(format!("{kind} BETWEEN {} AND {}", frame_bound(f.start, true), frame_bound(f.end, false)));
        }
        let sql = match (base, clauses.len()) {
            (Some(b), 1) => format!("$1 OVER {b}"),
            _ => format!("$1 OVER ({})", clauses.join(" ")),
        };
        Ok(template(
            self.target.dialect,
            match cast {
                Some(ty) => format!("CAST({sql} AS {ty})"),
                None => sql,
            },
            exprs,
        ))
    }

    /// The SQL call itself, arguments planned in the current scope.
    fn call(&mut self, name: &str, args: &[Expr], distinct: bool) -> Result<SExpr> {
        let (e, cast) = self.call_parts(name, args, distinct)?;
        Ok(match cast {
            Some(ty) => template(self.target.dialect, format!("CAST($1 AS {ty})"), vec![e]),
            None => e,
        })
    }

    /// The call, and the type its result is cast to (outside a window's `OVER`).
    fn call_parts(&mut self, name: &str, args: &[Expr], distinct: bool) -> Result<(SExpr, Option<&'static str>)> {
        if self.target.dialect == Dialect::Sqlite && name == "cardinality" {
            return Err(Error::query("sqlite does not support cardinality()"));
        }
        if self.target.dialect == Dialect::Sqlite && matches!(name, "element" | "unnest") {
            return Err(Error::query("sqlite does not support array element access or unnest()"));
        }
        if name == "unnest" && !self.allow_unnest {
            return Err(Error::query("unnest() returns several rows: it can only be a select() column"));
        }
        let hint = args.first().map(|a| self.hint_of(a)).unwrap_or_default();
        let hint = Hint { ty: if TEXT_FUNCS.contains(&name) { Some(ValueType::scalar(ColType::Text)) } else { hint.ty }, field: None };
        let planned = args.iter().map(|a| self.value(a, hint)).collect::<Result<Vec<_>>>()?;
        let d = if distinct { "DISTINCT " } else { "" };
        let n = planned.len();
        let dialect = self.target.dialect;
        let call = |sql: &str, planned: Vec<SExpr>| -> Result<SExpr> {
            let slots = (1..=planned.len()).map(|i| format!("${i}")).collect::<Vec<_>>().join(", ");
            Ok(template(dialect, format!("{sql}({slots})"), planned))
        };
        let one = |tpl: &str, planned: Vec<SExpr>| -> Result<SExpr> {
            match <[SExpr; 1]>::try_from(planned) {
                Ok([a]) => Ok(template(dialect, tpl, vec![a])),
                Err(_) => Err(Error::query(format!("{name}() takes one argument"))),
            }
        };
        let mut cast = None;
        let e = match name {
            "count" if planned.is_empty() => SExpr::cust("COUNT(*)"),
            "count" => one(&format!("COUNT({d}$1)"), planned)?,
            "sum" => {
                let ty = args.first().map(|a| self.expr_type(a)).transpose()?;
                if matches!(ty.map(|t| t.ty), Some(ColType::Int | ColType::BigInt)) {
                    cast = Some("BIGINT");
                }
                one(&format!("SUM({d}$1)"), planned)?
            }
            "avg" => {
                let ty = args.first().map(|a| self.expr_type(a)).transpose()?;
                if ty.map(|t| t.ty) != Some(ColType::Decimal) {
                    cast = Some("DOUBLE PRECISION");
                }
                one(&format!("AVG({d}$1)"), planned)?
            }
            "min" => one("MIN($1)", planned)?,
            "max" => one("MAX($1)", planned)?,
            "lower" => one("LOWER($1)", planned)?,
            "upper" => one("UPPER($1)", planned)?,
            "length" => one("LENGTH($1)", planned)?,
            "cardinality" => one("CARDINALITY($1)", planned)?,
            "abs" => one("ABS($1)", planned)?,
            "concat" if !planned.is_empty() => call("CONCAT", planned)?,
            "trim" => one("TRIM($1)", planned)?,
            "ltrim" => one("LTRIM($1)", planned)?,
            "rtrim" => one("RTRIM($1)", planned)?,
            "replace" if n == 3 => call("REPLACE", planned)?,
            "substr" if (2..=3).contains(&n) => call("SUBSTR", planned)?,
            "strpos" if n == 2 => call(if dialect == Dialect::Sqlite { "INSTR" } else { "STRPOS" }, planned)?,
            // The index is an `Int`, written into the SQL text.
            "element" => match args.get(1) {
                Some(Expr::Int { value }) if n == 2 => one(&format!("($1)[{value}]"), planned.into_iter().take(1).collect())?,
                _ => return Err(Error::query("wrong arguments for an array index")),
            },
            "unnest" => one("UNNEST($1)", planned)?,
            "now" if planned.is_empty() => SExpr::cust("CURRENT_TIMESTAMP"),
            "coalesce" if !planned.is_empty() => call("COALESCE", planned)?,
            "row_number" | "rank" | "dense_rank" | "percent_rank" | "cume_dist" if planned.is_empty() => {
                SExpr::cust(format!("{}()", name.to_uppercase()))
            }
            "ntile" | "first_value" | "last_value" => one(&format!("{}($1)", name.to_uppercase()), planned)?,
            "lag" | "lead" if (1..=3).contains(&n) => call(&name.to_uppercase(), planned)?,
            "nth_value" if n == 2 => call("NTH_VALUE", planned)?,
            _ => return Err(Error::query(format!("wrong arguments for {name}()"))),
        };
        Ok((e, cast))
    }

    /// `(SELECT <agg> FROM <hop 1> a1 [JOIN <hop 2> a2 ON ...] WHERE a1.to = <scope>.from)`.
    fn aggregate_subquery(
        &mut self,
        name: &str,
        args: &[Expr],
        path: &[String],
        count_rows: bool,
        distinct: bool,
    ) -> Result<SExpr> {
        let base = self.scope().path.clone();
        let hops = &path[base.len()..];
        let mut sub = Query::select();
        let mut outer = (self.scope().alias.clone(), self.scope().model);
        let pushed = self.scopes.len();
        for (k, hop) in hops.iter().enumerate() {
            let (rel, target) = self.model(outer.1).relation(hop).map_err(query_err)?;
            let alias = self.add_hop(&mut sub, k == 0, (&outer.0, outer.1), rel, target, "a")?;
            self.scopes.push(Scope { path: path[..base.len() + k + 1].to_vec(), model: target, alias: alias.clone() });
            outer = (alias, target);
        }
        let agg = if count_rows && args.is_empty() { self.call("count", &[], false) } else { self.call(name, args, distinct) };
        self.scopes.truncate(pushed);
        sub.expr(agg?);
        Ok(SExpr::SubQuery(None, Box::new(sub.into())))
    }

    // -- select(...) ----------------------------------------------------------------------

    /// `select(...)` columns. In a CTE (`cte`), columns are named after the CTE's columns
    /// and the model's are stored as they are (`read_sql` applies when they are read).
    fn select_columns(&mut self, q: &Select, items: &[SelectItem], cte: bool) -> Result<SelectPlan> {
        self.enter(q)?;
        if !q.select_related.is_empty() || !q.prefetch.is_empty() {
            return Err(Error::query("select() can't be combined with select_related / prefetch_related"));
        }
        if items.is_empty() {
            return Err(Error::query("select() needs at least one column"));
        }
        let root = self.model(self.root);
        let alias = self.root_alias().to_owned();
        let mut stmt = Query::select();
        let mut types = vec![];
        let mut shape = vec![];
        #[cfg(feature = "composition")]
        let mut computations = vec![];
        let mut aggregated = !q.group_by.is_empty() || !q.having.is_empty();
        let mut windowed = false;
        for item in items {
            match item {
                SelectItem::Model => {
                    #[cfg(feature = "composition")]
                    if cte && !root.native.computed().is_empty() { return Err(Error::query("native computed model outputs cannot be used in a CTE")); }
                    if self.root >= self.schema.models.len() {
                        return Err(Error::query("a CTE without a model has no model to select"));
                    }
                    #[cfg(feature = "composition")]
                    computations.extend(crate::behavior::model_computations(root.native, types.len()));
                    for (_position, f) in root.fields().iter().enumerate() {
            #[cfg(not(feature = "composition"))]
            let _ = _position;
                        #[cfg(feature = "composition")]
                        if root.native.computed().contains(&_position) { stmt.expr(SExpr::cust("NULL")); types.push(f.value_type()); continue; }
                        if cte {
                            stmt.expr_as(self.stored_field(self.root, &alias, f)?, Alias::new(&f.column));
                        } else {
                            stmt.expr(self.read_field(self.root, &alias, f)?);
                        }
                        types.push(f.value_type());
                    }
                    shape.push(Some(root.fields().len()));
                }
                SelectItem::Expr { expr, name } => {
                    self.join_paths(expr, "select()")?;
                    aggregated |= has_local_aggregate(expr);
                    windowed |= has_window(expr);
                    types.push(self.expr_type(expr)?);
                    self.allow_window = true;
                    self.allow_unnest = true;
                    #[cfg(feature = "composition")]
                    let computed = match expr {
                        Expr::Col { path, name } => {
                            let m = self.model(self.walk(self.root, path)?);
                            let position = m.field_pos(name).map_err(query_err)?;
                            if m.native.computed().contains(&position) {
                                if cte { return Err(Error::query("native computed fields cannot be used in a CTE")); }
                                computations.push(crate::behavior::Computation { kind: m.native, field: position, column: types.len() - 1, dependency: types.len() - 1 });
                                true
                            } else { false }
                        }
                        _ => false,
                    };
                    #[cfg(feature = "composition")]
                    let e = if computed { match expr { Expr::Col { path, name } => self.resolve(path, name), _ => unreachable!() } } else { self.value(expr, Hint::default()) };
                    #[cfg(not(feature = "composition"))]
                    let e = self.value(expr, Hint::default());
                    self.allow_window = false;
                    self.allow_unnest = false;
                    let mut e = e?;
                    let read_sql = match expr {
                        Expr::Col { path, name } => {
                            let m = self.walk(self.root, path)?;
                            self.model(m).field(name).map_err(query_err)?.read_sql.as_ref()
                        }
                        Expr::CteCol { cte, name } => {
                            self.model(self.model_idx(cte)?).field(name).map_err(query_err)?.read_sql.as_ref()
                        }
                        _ => None,
                    };
                    if let Some(t) = read_sql {
                        e = SExpr::cust_with_expr(t.replace("{}", "$1"), e);
                    }
                    match (cte, name) {
                        (true, Some(n)) => stmt.expr_as(e, Alias::new(n)),
                        (true, None) => return Err(Error::query("CTE columns need names")),
                        (false, _) => stmt.expr(e),
                    };
                    shape.push(None);
                }
            }
        }
        for g in &q.group_by {
            self.join_paths(g, "group_by()")?;
            let e = self.value(g, Hint::default())?;
            stmt.add_group_by([e]);
        }
        for h in &q.having {
            let e = self.cond(h)?;
            stmt.and_having(e);
        }
        if !q.distinct_on.is_empty() {
            self.require(self.caps.distinct_on, "distinct(on=...)")?;
            let mut cols = vec![];
            for e in &q.distinct_on {
                let Expr::Col { path, name } = e else {
                    return Err(Error::query("distinct(on=...) takes columns"));
                };
                self.join_paths(e, "distinct(on=...)")?;
                #[cfg(feature = "composition")]
                self.reject_computed(self.model(self.walk(self.root, path)?), name)?;
                #[cfg(feature = "composition")]
                {
                    let model = self.model(self.walk(self.root, path)?);
                    if model.resolved_fields[model.field_pos(name).map_err(query_err)?].storage.owner != model.owner { return Err(Error::query("distinct(on=...) on inherited storage requires an explicit owner projection")); }
                }
                let (alias, column) = self.resolve_parts(path, name)?;
                cols.push((Alias::new(alias), Alias::new(column)));
            }
            stmt.distinct_on(cols);
        } else if q.distinct {
            stmt.distinct();
        }
        #[cfg(feature = "composition")]
        if !computations.is_empty() && (aggregated || q.distinct || !q.distinct_on.is_empty()) {
            return Err(Error::query("native computed outputs cannot use SQL grouping or distinct; select stored dependencies instead"));
        }
        windowed |= q.order.iter().any(|o| has_window(&o.expr));
        self.base_select(q, &mut stmt, true)?;
        self.declare_windows(q, &mut stmt)?;
        self.apply_joins(&mut stmt);
        if let Some(lock) = q.lock {
            if aggregated || windowed || q.distinct || !q.distinct_on.is_empty() {
                return Err(query_err(
                    "lock() can't be used with aggregates, window functions, group_by() or distinct()".into(),
                ));
            }
            self.apply_lock(&mut stmt, lock)?;
        }
        Ok(SelectPlan { stmt, types, output: Output::Rows { model: self.root, items: shape }, prefetch: vec![],
            #[cfg(feature = "composition")] computations })
    }

    /// LEFT JOINs for the to-one paths `e` reads outside aggregates.
    fn join_paths(&mut self, e: &Expr, why: &str) -> Result<()> {
        let (mut paths, mut has_not) = (vec![], false);
        col_paths(e, &mut paths, &mut has_not);
        for p in paths.into_iter().filter(|p| !p.is_empty()) {
            self.ensure_join(p, why).map_err(|_| {
                Error::query(format!(
                    "{why} can follow only to-one relations, {} goes through a to-many one: \
                     aggregate it instead, e.g. func.count(...)",
                    p.join(".")
                ))
            })?;
        }
        Ok(())
    }

    fn resolve_parts(&self, path: &[String], name: &str) -> Result<(String, String)> {
        let s = self
            .scopes
            .iter()
            .rev()
            .chain(self.joins.iter().map(|(s, _)| s))
            .find(|s| s.path == path)
            .ok_or_else(|| {
                Error::query(format!(
                    "{}.{name} is not reachable here: one comparison can follow only one relation path",
                    path.join(".")
                ))
            })?;
        let f = self.model(s.model).field(name).map_err(query_err)?;
        Ok((s.alias.clone(), f.column.clone()))
    }

    // -- joins (select_related / order_by) ----------------------------------------------

    /// Adds LEFT JOINs for every prefix of `path`; returns (alias, model) of the last hop.
    ///
    /// TODO: shortcut joins. In `A -r1-> B -r2-> C` with `r2.from == r1.to` (e.g.
    /// `Order.shop.config`, where `ShopConfig.shop_id` is the key and a FK to `Shop.id`),
    /// B can be skipped and C linked as `C.r2.to = A.r1.from`
    /// (`shop_configs.shop_id = orders.shop_id`), when nothing else of B is read
    /// (reading `B.<r1.to>` itself resolves to `A.<r1.from>`) and `r1` or `r2` is a FK, so
    /// B's row is known to exist. Same for `exists_via`. Postgres doesn't do this itself:
    /// it derives `orders.shop_id = shop_configs.shop_id` for inner joins but still joins
    /// `shops` (it doesn't trust FKs for join removal), and for LEFT JOINs it derives
    /// nothing. `select_related` of B must keep the join.
    fn ensure_join(&mut self, path: &[String], why: &str) -> Result<(String, usize)> {
        let (mut alias, mut model) = (self.root_alias().to_owned(), self.root);
        for i in 0..path.len() {
            let prefix = &path[..=i];
            if let Some((s, _)) = self.joins.iter().find(|(s, _)| s.path == prefix) {
                (alias, model) = (s.alias.clone(), s.model);
                continue;
            }
            let m = self.model(model);
            let (rel, target) = m.relation(&path[i]).map_err(query_err)?;
            if rel.kind != RelKind::One {
                return Err(Error::query(format!(
                    "{why} can only follow to-one relations; {} is to-many",
                    prefix.join(".")
                )));
            }
            let from_col = &m.field(&rel.from).map_err(query_err)?.column;
            let to_col = &self.model(target).field(&rel.to).map_err(query_err)?.column;
            let new_alias = self.alias("j");
            let on = col(&new_alias, to_col).eq(col(&alias, from_col));
            #[cfg(feature = "query-defaults")]
            let on = match self.model(target).query_defaults.filter.as_ref().filter(|_| !self.policy_bypass) {
                Some(filter) => on.and(self.target_default(target, &new_alias, filter)?), None => on,
            };
            self.joins.push((Scope { path: prefix.to_vec(), model: target, alias: new_alias.clone() }, on));
            (alias, model) = (new_alias, target);
        }
        Ok((alias, model))
    }

    #[cfg(feature = "query-defaults")]
    fn target_default(&self, model: usize, alias: &str, filter: &Expr) -> Result<SExpr> {
        let mut planner = Planner::new(self.schema, self.virt, self.target, &self.model(model).ir.name, None, self.params, vec![], 0)?;
        planner.scopes[0].alias = alias.into();
        planner.cond(filter)
    }

    fn apply_joins(&self, stmt: &mut SelectStatement) {
        for (s, on) in &self.joins {
            stmt.join_as(
                JoinType::LeftJoin,
                Alias::new(self.model(s.model).table()),
                Alias::new(&s.alias),
                on.clone(),
            );
        }
    }

    /// `FROM <source>`, filters, ordering (when `order`) and slicing.
    fn base_select(&mut self, q: &Select, stmt: &mut SelectStatement, order: bool) -> Result<()> {
        let alias = self.root_alias().to_owned();
        if alias == self.source {
            stmt.from(Alias::new(&self.source));
        } else {
            stmt.from_as(Alias::new(&self.source), Alias::new(&alias));
        }
        for j in &q.joins {
            let on = self.cond(&j.on)?;
            let kind = if j.outer { JoinType::LeftJoin } else { JoinType::InnerJoin };
            stmt.join(kind, Alias::new(&j.cte), on);
        }
        for w in self.apply_filters(&q.filters, q.without_defaults)? {
            stmt.and_where(w);
        }
        for o in q.order.iter().filter(|_| order) {
            let (mut paths, mut has_not) = (vec![], false);
            col_paths(&o.expr, &mut paths, &mut has_not);
            for p in paths.into_iter().filter(|p| !p.is_empty()) {
                self.ensure_join(p, "order_by")?;
            }
            self.allow_window = true;
            let e = self.value(&o.expr, Hint::default());
            self.allow_window = false;
            stmt.order_by_expr(e?, if o.desc { SOrder::Desc } else { SOrder::Asc });
        }
        if let Some(n) = q.limit {
            stmt.limit(count(self.params, n)?);
        }
        if let Some(n) = q.offset {
            stmt.offset(count(self.params, n)?);
        }
        if let Some((name, true)) = &self.recursive {
            if !<[String]>::contains(&self.joined, name) {
                stmt.from(Alias::new(name));
            }
        }
        Ok(())
    }

    fn apply_lock(&self, stmt: &mut SelectStatement, lock: Lock) -> Result<()> {
        if self.source != self.model(self.root).table() {
            return Err(Error::query("lock() can't be used on a query reading a CTE"));
        }
        let c = self.caps;
        if lock.exclusive {
            self.require(c.lock_exclusive, "lock() (SELECT ... FOR UPDATE)")?;
        } else {
            self.require(c.lock_shared, "lock(exclusive=False) (SELECT ... FOR SHARE)")?;
        }
        self.require(c.lock_nowait || !lock.nowait, "lock(nowait=True)")?;
        self.require(c.lock_skip_locked || !lock.skip_locked, "lock(skip_locked=True)")?;
        self.require(c.lock_of || self.joins.is_empty(), "lock() together with select_related")?;
        apply_lock(stmt, lock, c.lock_of.then(|| self.root_alias()));
        Ok(())
    }

    // -- statements ---------------------------------------------------------------------

    /// The output shape for public `names` plus private `extra` helpers; `None` keeps the
    /// whole-model row and its fast materialization path.
    fn instance_shape(&self, model: usize, names: Option<&[String]>, extra: &[String]) -> Result<Option<orm_core::behavior::ResultShape>> {
        use orm_core::behavior::{FieldId, ModelId, ResultField, ResultShape};
        let Some(names) = names else { return Ok(None) };
        let m = self.model(model);
        let mut positions = Vec::new();
        for name in names {
            let pos = m.field_pos(name).map_err(query_err)?;
            if positions.as_slice().contains(&pos) { return Err(Error::query("duplicate model field")); }
            positions.push(pos);
        }
        if positions.len() == m.fields().len() && positions.iter().enumerate().all(|(i, &p)| i == p) { return Ok(None); }
        let public = positions.len();
        // Identity and relation keys remain private when omitted by the caller.
        let mut helpers = vec![m.pk];
        for r in &m.ir.relations { helpers.push(m.field_pos(&r.from).map_err(query_err)?); }
        for source in &self.schema.models {
            for relation in &source.ir.relations {
                if relation.target == m.ir.name { helpers.push(m.field_pos(&relation.to).map_err(query_err)?); }
            }
        }
        for name in extra { helpers.push(m.field_pos(name).map_err(query_err)?); }
        for pos in helpers { if !positions.as_slice().contains(&pos) { positions.push(pos); } }
        // Computed fields need their inputs, including computed helpers.
        #[cfg(feature = "composition")]
        for i in 0..positions.len() { if let Some(d) = m.native.dependency(positions[i]) { if !positions.contains(&d) { positions.push(d); } } }
        Ok(Some(ResultShape { model: ModelId(model), fields: positions.into_iter().enumerate().map(|(slot, field)| ResultField {
            field: FieldId { model: ModelId(model), position: field }, physical: Some(slot), public: slot < public,
            dependencies: {
                #[cfg(feature = "composition")]
                { m.native.dependency(field).map(|position| FieldId { model: ModelId(model), position }).into_iter().collect() }
                #[cfg(not(feature = "composition"))]
                { vec![] }
            },
        }).collect() }))
    }

    /// A SELECT of instances (with `select_related` and `prefetch`) or of `select(...)`
    /// columns.
    fn build_select(&mut self, q: &Select) -> Result<SelectPlan> {
        #[cfg(feature = "query-defaults")]
        let q = &{
            let mut q = q.clone();
            let defaults = &self.model(self.root).query_defaults;
            if !q.without_defaults {
                if q.model_fields.is_none() { q.model_fields = defaults.fields.clone(); }
                if !q.without_related && q.columns.is_none() {
                    for path in orm_core::selection::expanded_related(self.schema.models.as_slice(), self.root).map_err(query_err)? {
                        if !q.select_related.contains(&path) { q.select_related.push(path); }
                    }
                    for prefix in q.select_related.clone() {
                        let target = self.schema.walk(self.root, &prefix).map_err(query_err)?;
                        for suffix in orm_core::selection::expanded_related(self.schema.models.as_slice(), target).map_err(query_err)? {
                            let mut path = prefix.clone(); path.extend(suffix);
                            if !q.select_related.contains(&path) { q.select_related.push(path); }
                        }
                    }
                }
            }
            q
        };
        if let Some(items) = &q.columns {
            return self.select_columns(q, items, false);
        }
        if !q.group_by.is_empty() || !q.having.is_empty() || q.distinct || !q.distinct_on.is_empty() {
            return Err(Error::query("group_by / having / distinct need select(...)"));
        }
        if self.root >= self.schema.models.len() {
            return Err(Error::query("a CTE without a model is read with select(...)"));
        }
        self.enter(q)?;
        let root = self.model(self.root);
        let alias = self.root_alias().to_owned();
        let mut stmt = Query::select();
        let mut types = vec![];
        let shape = self.instance_shape(self.root, q.model_fields.as_deref(), &q.model_helpers)?;
        let positions = shape_positions(root, shape.as_ref());
        for position in positions.iter() {
            let f = &root.fields()[position];
            #[cfg(feature = "composition")]
            if root.native.computed().contains(&position) { stmt.expr(SExpr::cust("NULL")); types.push(f.value_type()); continue; }
            stmt.expr(self.read_field(self.root, &alias, f)?);
            types.push(f.value_type());
        }
        let mut joins: Vec<JoinShape> = vec![];
        for path in &q.select_related {
            let (alias, model) = self.ensure_join(path, "select_related")?;
            let m = self.model(model);
            let parent = match path.len() {
                0 => return Err(Error::query("select_related needs a relation")),
                1 => None,
                n => Some(
                    q.select_related
                        .iter()
                        .position(|p| p.as_slice() == &path[..n - 1])
                        .ok_or_else(|| Error::query("select_related paths must list their prefixes first"))?,
                ),
            };
            #[cfg(feature = "query-defaults")]
            let joined_shape = self.instance_shape(model, if q.without_defaults { None } else { m.query_defaults.fields.as_deref() }, &[])?;
            #[cfg(not(feature = "query-defaults"))]
            let joined_shape = None;
            let joined_positions = shape_positions(m, joined_shape.as_ref());
            joins.push(JoinShape {
                parent,
                attr: path[path.len() - 1].clone(),
                model,
                start: types.len(),
                pk_pos: joined_positions.iter().position(|f| f == m.pk).expect("identity selected"),
                shape: joined_shape,
            });
            for position in joined_positions.iter() {
                let f = &m.fields()[position];
                #[cfg(feature = "composition")]
                if m.native.computed().contains(&position) { stmt.expr(SExpr::cust("NULL")); types.push(f.value_type()); continue; }
                stmt.expr(self.read_field(model, &alias, f)?);
                types.push(f.value_type());
            }
        }
        self.base_select(q, &mut stmt, true)?;
        self.declare_windows(q, &mut stmt)?;
        self.apply_joins(&mut stmt);
        if q.order.iter().any(|o| has_window(&o.expr)) && q.lock.is_some() {
            return Err(Error::query("lock() can't be used with window functions"));
        }
        if let Some(lock) = q.lock {
            self.apply_lock(&mut stmt, lock)?;
        }
        let mut prefetch = vec![];
        for node in &q.prefetch {
            let mut plan = plan_prefetch(self.schema, self.target, self.params, self.root, node, q.without_defaults)?;
            plan.key_pos = positions.iter().position(|f| f == plan.key_pos).expect("selected relation helper");
            prefetch.push(plan);
        }
        #[cfg(feature = "composition")]
        let computations = {
            let mut out = crate::behavior::shape_computations(root.native, 0, &positions.to_vec());
            for j in &joins { out.extend(crate::behavior::shape_computations(self.model(j.model).native, j.start, &shape_positions(self.model(j.model), j.shape.as_ref()).to_vec())); }
            out
        };
        Ok(SelectPlan { stmt, types, output: Output::Instances { model: self.root, shape, joins }, prefetch,
            #[cfg(feature = "composition")] computations })
    }

    fn sliced_inner(&mut self, q: &Select) -> Result<SelectStatement> {
        self.enter(q)?;
        let mut inner = Query::select();
        inner.expr(SExpr::val(1));
        self.base_select(q, &mut inner, true)?;
        self.apply_joins(&mut inner);
        Ok(inner)
    }

    pub fn count(&mut self, q: &Select) -> Result<SelectStatement> {
        no_lock(q, "count")?;
        let mut stmt = Query::select();
        if q.limit.is_some() || q.offset.is_some() {
            let inner = self.sliced_inner(q)?;
            stmt.expr(SExpr::cust("COUNT(*)")).from_subquery(inner, Alias::new("sliced"));
        } else {
            self.enter(q)?;
            stmt.expr(SExpr::cust("COUNT(*)"));
            self.base_select(q, &mut stmt, false)?;
            self.apply_joins(&mut stmt);
        }
        Ok(stmt)
    }

    pub fn exists(&mut self, q: &Select) -> Result<SelectStatement> {
        no_lock(q, "exists")?;
        let mut inner = self.sliced_inner(q)?;
        if q.limit.is_none() {
            inner.limit(1);
        }
        let mut stmt = Query::select();
        stmt.expr(SExpr::exists(inner));
        Ok(stmt)
    }

    #[cfg(feature = "model-composition")]
    fn composed_update_values(&mut self, q: &Update) -> Result<Vec<SExpr>> {
        if q.set.is_empty() { return Err(Error::query("update() needs at least one field")); }
        self.enter(&serde_json::from_value(serde_json::json!({"model":q.model})).map_err(|e| query_err(e.to_string()))?)?;
        let root = self.model(self.root);
        let positions = q.set.iter().map(|a| root.field_pos(&a.field).map_err(query_err)).collect::<Result<Vec<_>>>()?;
        if positions.iter().collect::<std::collections::BTreeSet<_>>().len() != positions.len() { return Err(Error::query("duplicate update field")); }
        self.set_values(q)
    }

    /// `SET` values in `q.set` order, after the model's native field and record checks.
    #[cfg(feature = "composition")]
    fn set_values(&mut self, q: &Update) -> Result<Vec<SExpr>> {
        let root = self.model(self.root);
        let positions: Vec<_> = q.set.iter().map(|a| root.field_pos(&a.field).map_err(query_err)).collect::<Result<_>>()?;
        if positions.iter().any(|p| root.native.computed().contains(p)) { return Err(Error::query("computed fields are read-only")); }
        let map = if root.native.has_records() { crate::behavior::input_map(root.fields().len(), &positions)? } else { vec![] };
        let mut values = Vec::with_capacity(q.set.len());
        for (a, &position) in q.set.iter().zip(&positions) {
            let f = &root.fields()[position];
            values.push(if let Expr::Param { i } = &a.value {
                let mut value = self.params.value(self.param(*i)?, Some(f.value_type()))?;
                crate::behavior::field(root.native, position, &mut value)?;
                Some(value)
            } else {
                crate::behavior::expression(root.native, position)?;
                None
            });
        }
        crate::behavior::record(root.native, &map, values.as_slice())?;
        q.set.iter().zip(values).map(|(a, value)| {
            let f = root.field(&a.field).map_err(query_err)?;
            match value { Some(value) => Ok(bind(value, Some(f))), None => self.value(&a.value, Hint { ty: Some(f.value_type()), field: Some(f) }) }
        }).collect()
    }

    fn return_shape(&self, names: Option<&[String]>, _without_defaults: bool) -> Result<Option<orm_core::behavior::ResultShape>> {
        #[cfg(feature = "query-defaults")]
        let names = names.or_else(|| if _without_defaults { None } else { self.model(self.root).query_defaults.fields.as_deref() });
        self.instance_shape(self.root, names, &[])
    }

    pub fn update(&mut self, q: &Update) -> Result<(UpdateStatement, Option<ReturnColumns>)> {
        let root = self.model(self.root);
        #[cfg(feature = "composition")]
        crate::ownership::require_local_write(root)?;
        let mut stmt = Query::update();
        stmt.table(Alias::new(root.table()));
        if q.set.is_empty() {
            return Err(Error::query("update() needs at least one field"));
        }
        #[cfg(feature = "file-storage")]
        for assignment in &q.set {
            if root.is_file_field(&assignment.field) {
                let position = root.field_pos(&assignment.field).map_err(query_err)?;
                let Expr::Param { i } = assignment.value else {
                    return Err(Error::query("file-field updates require a durable reference, not an expression"));
                };
                let field = root.field(&assignment.field).map_err(query_err)?;
                crate::file_storage::value(root, position, &self.params.value(self.param(i)?, Some(field.value_type()))?)?;
            }
        }
        #[cfg(feature = "composition")]
        for (a, v) in q.set.iter().zip(self.set_values(q)?) {
            stmt.value(Alias::new(&root.field(&a.field).map_err(query_err)?.column), v);
        }
        #[cfg(not(feature = "composition"))]
        for a in &q.set {
            let f = root.field(&a.field).map_err(query_err)?;
            let v = self.value(&a.value, Hint { ty: Some(f.value_type()), field: Some(f) })?;
            stmt.value(Alias::new(&f.column), v);
        }
        for w in self.apply_filters(&q.filters, q.without_defaults)? {
            stmt.and_where(w);
        }
        if !q.returning {
            return Ok((stmt, None));
        }
        self.require(self.caps.returning, "update().returning()")?;
        let shape = self.return_shape(q.model_fields.as_deref(), q.without_defaults)?;
        let positions = shape_positions(root, shape.as_ref());
        stmt.returning(Query::returning().exprs(positions.iter().map(|pos| {
            let f = &root.fields()[pos];
            #[cfg(feature = "composition")]
            if root.native.computed().contains(&pos) { return SExpr::cust("NULL"); }
            returning_col(f)
        })));
        Ok((stmt, Some((positions.iter().map(|pos| root.fields()[pos].value_type()).collect(), shape))))
    }

    pub fn delete(&mut self, q: &Delete) -> Result<(DeleteStatement, Option<ReturnColumns>)> {
        let root = self.model(self.root);
        #[cfg(feature = "composition")]
        crate::ownership::require_local_write(root)?;
        let mut stmt = Query::delete();
        stmt.from_table(Alias::new(root.table()));
        for w in self.apply_filters(&q.filters, q.without_defaults)? {
            stmt.and_where(w);
        }
        if !q.returning {
            return Ok((stmt, None));
        }
        self.require(self.caps.returning, "delete().returning()")?;
        let shape = self.return_shape(q.model_fields.as_deref(), q.without_defaults)?;
        let positions = shape_positions(root, shape.as_ref());
        stmt.returning(Query::returning().exprs(positions.iter().map(|pos| {
            let f = &root.fields()[pos];
            #[cfg(feature = "composition")]
            if root.native.computed().contains(&pos) { return SExpr::cust("NULL"); }
            returning_col(f)
        })));
        Ok((stmt, Some((positions.iter().map(|pos| root.fields()[pos].value_type()).collect(), shape))))
    }
}

/// A top-level SELECT with its `WITH` clause.
pub fn plan_select(schema: &Schema, target: Target, q: &Select, params: &dyn Params) -> Result<SelectPlan> {
    let virt = derive_ctes(schema, target, &q.with, params)?;
    let mut p = Planner::new(schema, &virt, target, &q.model, q.from.as_deref(), params, vec![], 0)?;
    let mut plan = p.build_select(q)?;
    if let Some(w) = p.with_clause(&q.with)? {
        plan.stmt.with_cte(w);
    }
    Ok(plan)
}

/// The query loading `node` (a relation of `parent`) for a set of parent keys. A slice
/// applies per parent: `ROW_NUMBER() OVER (PARTITION BY <key> ORDER BY ...)` numbers
/// the related rows and the outer query keeps the slice.
fn plan_prefetch(
    schema: &Schema,
    target: Target,
    params: &dyn Params,
    parent: usize,
    node: &Prefetch,
    without_defaults: bool,
) -> Result<PrefetchPlan> {
    let pm = schema.model(parent);
    let (rel, child) = pm.relation(&node.relation).map_err(query_err)?;
    let cm = schema.model(child);
    if node.query.model != cm.ir.name {
        return Err(Error::query(format!(
            "the query prefetching {}.{} must be over {}, not {}",
            pm.ir.name, node.relation, cm.ir.name, node.query.model
        )));
    }
    if node.query.columns.is_some() || node.query.lock.is_some() {
        return Err(Error::query("a prefetch query can't select columns or lock rows"));
    }
    let many = rel.kind == RelKind::Many;
    let mut q = node.query.clone();
    q.without_defaults |= without_defaults;
    let pk_order = Order { expr: Expr::Col { path: vec![], name: cm.pk_field().name.clone() }, desc: false };
    let sliced = q.limit.is_some() || q.offset.is_some();
    let slice = match sliced {
        false => None,
        true => Some((
            q.offset.map(|n| count(params, n)).transpose()?.unwrap_or(0),
            q.limit.map(|n| count(params, n)).transpose()?,
        )),
    };
    let mut window_order = vec![];
    if sliced {
        window_order = std::mem::take(&mut q.order);
        window_order.push(pk_order);
        (q.limit, q.offset) = (None, None);
    } else if q.order.is_empty() {
        q.order.push(pk_order);
    }
    let virt = derive_ctes(schema, target, &q.with, params)?;
    let mut p = Planner::new(schema, &virt, target, &q.model, q.from.as_deref(), params, vec![], 0)?;
    let mut plan = p.build_select(&q)?;
    // the parent key each row belongs to: the row's own `to` field, or (many-to-many)
    // the join row's source field, selected as an extra `_key` column
    let (key_field, key, child_key_pos) = match &rel.through {
        None => {
            let f = cm.field(&rel.to).map_err(query_err)?;
            (f, col(p.root_alias(), &f.column), match &plan.output { Output::Instances { shape, .. } => shape_positions(cm, shape.as_ref()).iter().position(|pos| pos == cm.field_pos(&rel.to).expect("validated relation")).ok_or_else(|| Error::query("missing prefetch key"))?, _ => unreachable!() })
        }
        Some(th) => {
            let jm = schema.model(schema.model_idx(&th.model).map_err(query_err)?);
            let (src, dst) = (jm.field(&th.source).map_err(query_err)?, jm.field(&th.target).map_err(query_err)?);
            let jalias = p.alias("m");
            let on = col(&jalias, &dst.column).eq(col(p.root_alias(), &cm.field(&rel.to).map_err(query_err)?.column));
            plan.stmt.join_as(JoinType::InnerJoin, Alias::new(jm.table()), Alias::new(&jalias), on);
            let key = col(&jalias, &src.column);
            plan.stmt.expr_as(key.clone(), Alias::new("_key"));
            plan.types.push(src.value_type());
            (src, key, plan.types.len() - 1)
        }
    };
    if sliced {
        // ROW_NUMBER() OVER (PARTITION BY <key> ORDER BY ...); the ORDER BY may follow
        // to-one relations not joined yet
        let joined = p.joins.len();
        let mut exprs = vec![key.clone()];
        let mut slots = vec![];
        for o in &window_order {
            p.join_paths(&o.expr, "a prefetch's order_by()")?;
            p.allow_window = true;
            let e = p.value(&o.expr, Hint::default());
            p.allow_window = false;
            exprs.push(e?);
            slots.push(format!("${}{}", exprs.len(), if o.desc { " DESC" } else { "" }));
        }
        let e = template(target.dialect, format!("ROW_NUMBER() OVER (PARTITION BY $1 ORDER BY {})", slots.join(", ")), exprs);
        for (s, on) in &p.joins[joined..] {
            plan.stmt.join_as(JoinType::LeftJoin, Alias::new(p.model(s.model).table()), Alias::new(&s.alias), on.clone());
        }
        plan.stmt.expr_as(e, Alias::new("_rn"));
    }
    if let Some(w) = p.with_clause(&q.with)? {
        plan.stmt.with_cte(w);
    }
    // has-many / has-one: the child's to-one relation back to the parent is set too
    let back = (!rel.foreign_key && rel.through.is_none())
        .then(|| {
            cm.ir.relations.iter().find(|r| {
                r.kind == RelKind::One
                    && r.foreign_key
                    && r.from == rel.to
                    && r.to == rel.from
                    && cm.relation(&r.name).map(|(_, t)| t) == Ok(parent)
            })
        })
        .flatten()
        .map(|r| r.name.clone());
    Ok(PrefetchPlan {
        attr: node.attr.clone().unwrap_or_else(|| node.relation.clone()),
        many,
        key_pos: pm.field_pos(&rel.from).map_err(query_err)?,
        key_type: pm.field(&rel.from).map_err(query_err)?.value_type(),
        child_key_pos,
        back,
        stmt: plan.stmt,
        key,
        key_field: key_field.clone(),
        slice,
        types: plan.types,
        output: plan.output,
        #[cfg(feature = "composition")] computations: plan.computations,
        children: plan.prefetch,
    })
}

/// The value of a `LIMIT` / `OFFSET`.
fn count(params: &dyn Params, c: Count) -> Result<u64> {
    match c {
        Count::Value(n) => Ok(n),
        Count::Param(ParamRef::Param { i }) => {
            if i >= params.len() {
                return Err(Error::query(format!("parameter {i} out of range")));
            }
            params.count(i)
        }
    }
}

/// `FOR UPDATE | FOR SHARE [OF <root>] [NOWAIT | SKIP LOCKED]`. `OF` limits the lock to
/// the model's own rows: rows joined by `select_related` stay unlocked (Postgres can't
/// lock the nullable side of a LEFT JOIN anyway).
fn apply_lock(stmt: &mut SelectStatement, lock: Lock, of: Option<&str>) {
    let ty = if lock.exclusive { LockType::Update } else { LockType::Share };
    let tables: Vec<Alias> = of.map(Alias::new).into_iter().collect();
    match (lock.nowait, lock.skip_locked) {
        (true, _) => stmt.lock_with_tables_behavior(ty, tables, LockBehavior::Nowait),
        (_, true) => stmt.lock_with_tables_behavior(ty, tables, LockBehavior::SkipLocked),
        _ => stmt.lock_with_tables(ty, tables),
    };
}

fn no_lock(q: &Select, what: &str) -> Result<()> {
    match q.lock {
        Some(_) => Err(Error::query(format!("{what}() can't lock rows; lock() applies to reading rows"))),
        None => Ok(()),
    }
}

/// What an insert does with rows that hit a unique constraint.
pub enum OnConflict {
    /// `ON CONFLICT (<fields>) DO NOTHING`: such rows are skipped (and not returned).
    Nothing(Vec<String>),
    /// `ON CONFLICT (<fields>) DO UPDATE SET <col> = EXCLUDED.<col>, ..., <field> = <expr>, ...`.
    /// The expressions see the existing row as the model's columns and the proposed
    /// row as `EXCLUDED`; their parameters are the ones passed to `plan_insert`.
    Update(Vec<String>, Vec<String>, Vec<Assignment>),
}

/// `INSERT INTO <table> (<fields>) VALUES ... [ON CONFLICT ...] RETURNING <all columns>`.
///
/// `rows` hold a value per field (converted by `field_types`); `None` is the SQL
/// `DEFAULT` keyword.
pub fn plan_insert(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: Vec<Vec<Option<sea_query::Value>>>,
    on_conflict: Option<OnConflict>,
    params: &dyn Params,
) -> Result<(InsertStatement, Vec<ValueType>)> {
    crate::db::require_dialect(target.dialect)?;
    let idx = schema.model_idx(model).map_err(query_err)?;
    let m = schema.model(idx);
    #[cfg(feature = "proxy-models")]
    let (fields, rows) = crate::proxy::insert_defaults(&schema.proxy_models[idx], m, fields, rows)?;
    #[cfg(feature = "proxy-models")]
    let fields = fields.as_ref();
    #[cfg(feature = "file-storage")]
    crate::file_storage::rows(m, fields, &rows)?;
    #[cfg(feature = "composition")]
    crate::ownership::require_local_write(m)?;
    #[cfg(feature = "composition")]
    let rows = {
        if on_conflict.is_some() { crate::behavior::upsert(m.native)?; }
        let mut rows = rows;
        let positions: Vec<_> = fields.iter().map(|name| m.field_pos(name).map_err(query_err)).collect::<Result<_>>()?;
        let map = crate::behavior::input_map(m.fields().len(), &positions)?;
        if positions.iter().any(|p| m.native.computed().contains(p)) { return Err(Error::query("computed fields are read-only")); }
        crate::behavior::insert_values(m.native, &map, &mut rows, fields.len())?;
        rows
    };
    let cols = fields.iter().map(|f| m.field(f)).collect::<std::result::Result<Vec<_>, _>>().map_err(query_err)?;
    let mut stmt = Query::insert();
    stmt.into_table(Alias::new(m.table()));
    if cols.is_empty() {
        if rows.len() != 1 {
            return Err(Error::query("rows without explicit values must be inserted one at a time"));
        }
        stmt.or_default_values();
    } else {
        stmt.columns(cols.iter().map(|c| Alias::new(&c.column)));
        for row in rows {
            if row.len() != cols.len() {
                return Err(Error::query("insert row length does not match fields"));
            }
            let values = cols.iter().zip(row).map(|(c, item)| match item {
                None if target.dialect == Dialect::Sqlite => {
                    let default = if c.default_now { "CURRENT_TIMESTAMP".to_owned() }
                    else if let Some(sql) = &c.default_sql { sql.clone() }
                    else if let Some(v) = &c.default {
                        match v {
                            serde_json::Value::String(s) => orm_core::migrate::model::quote_literal(s),
                            v if c.ty == ColType::Json => orm_core::migrate::model::quote_literal(&v.to_string()),
                            v => v.to_string(),
                        }
                    } else { "NULL".to_owned() };
                    SExpr::cust(default)
                }
                None => SExpr::cust("DEFAULT"),
                Some(v) => bind(v, Some(c)),
            });
            stmt.values(values.collect::<Vec<_>>()).map_err(|e| query_err(e.to_string()))?;
        }
    }
    let caps = target.caps;
    let require = |ok: bool, feature: &str| target.require(ok, feature).map_err(query_err);
    require(caps.returning, "insert ... RETURNING")?;
    if let Some(oc) = on_conflict {
        require(caps.on_conflict, "insert(...).on_conflict()")?;
        let columns = |names: &[String]| -> Result<Vec<Alias>> {
            names
                .iter()
                .map(|n| {
                    #[cfg(feature = "composition")]
                    if m.native.computed().contains(&m.field_pos(n).map_err(query_err)?) {
                        return Err(Error::query("computed fields cannot be conflict targets or write assignments"));
                    }
                    m.field(n).map(|f| Alias::new(&f.column)).map_err(query_err)
                })
                .collect()
        };
        let clause = match oc {
            OnConflict::Nothing(conflict) => sea_query::OnConflict::columns(columns(&conflict)?).do_nothing().to_owned(),
            OnConflict::Update(conflict, update, set) => {
                if update.is_empty() && set.is_empty() {
                    return Err(Error::query("on_conflict(...).do_update() has no columns to update"));
                }
                let mut clause = sea_query::OnConflict::columns(columns(&conflict)?);
                clause.update_columns(columns(&update)?);
                let mut planner = Planner::new(schema, &[], target, model, None, params, vec![], 0)?;
                planner.allow_excluded = true;
                for a in &set {
                    #[cfg(feature = "composition")]
                    if m.native.computed().contains(&m.field_pos(&a.field).map_err(query_err)?) {
                        return Err(Error::query("computed fields are read-only"));
                    }
                    #[cfg(feature = "file-storage")]
                    {
                        if m.is_file_field(&a.field) {
                            let position = m.field_pos(&a.field).map_err(query_err)?;
                            match &a.value {
                                Expr::Param { i } => {
                                    let field = m.field(&a.field).map_err(query_err)?;
                                    crate::file_storage::value(m, position, &params.value(planner.param(*i)?, Some(field.value_type()))?)?;
                                }
                                Expr::Excluded { name } if name == &a.field => {}
                                _ => return Err(Error::query("file conflict assignment requires a reference or the validated excluded file")),
                            }
                        }
                    }
                    let f = m.field(&a.field).map_err(query_err)?;
                    let v = planner.value(&a.value, Hint { ty: Some(f.value_type()), field: Some(f) })?;
                    clause.value(Alias::new(&f.column), v);
                }
                clause.to_owned()
            }
        };
        stmt.on_conflict(clause);
    }
    #[cfg(feature = "composition")]
    stmt.returning(Query::returning().exprs(m.fields().iter().enumerate().map(|(pos, f)| if m.native.computed().contains(&pos) { SExpr::cust("NULL") } else { returning_col(f) })));
    #[cfg(not(feature = "composition"))]
    stmt.returning(Query::returning().exprs(m.fields().iter().map(returning_col)));
    Ok((stmt, m.fields().iter().map(|f| f.value_type()).collect()))
}

/// `UPDATE ... SET` each row to its own values, matched by primary key, as one
/// statement per chunk of `rows`.
///
/// `fields[0]` is the primary key, the rest are the fields set; each row holds a value
/// per field. Where the dialect can, it joins a `VALUES` list:
///
/// ```sql
/// UPDATE posts SET title = v.column2, views = v.column3
/// FROM (VALUES ($1, $2, $3), ...) AS v WHERE posts.id = v.column1 AND <filters>
/// ```
///
/// otherwise it uses `SET title = CASE WHEN posts.id = $1 THEN $2 ... END ... WHERE
/// posts.id IN (...)`. `filters` (planned against `params`) further restrict the rows.
#[allow(clippy::too_many_arguments)]
pub fn plan_update_many(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: &[Vec<sea_query::Value>],
    chunk_rows: usize,
    filters: &[Expr],
    params: &dyn Params,
    returning: bool,
    without_defaults: bool,
) -> Result<(Vec<UpdateStatement>, Option<Vec<ValueType>>)> {
    let mut planner = Planner::new(schema, &[], target, model, None, params, vec![], 0)?;
    let m = schema.model(planner.root);
    #[cfg(feature = "composition")]
    crate::ownership::require_local_write(m)?;
    let table = m.table().to_owned();
    let cols = fields.iter().map(|f| m.field(f)).collect::<std::result::Result<Vec<_>, _>>().map_err(query_err)?;
    match cols.first() {
        Some(pk) if pk.primary_key && cols.len() > 1 => {}
        _ => return Err(Error::query("update_many needs the primary key followed by the fields to set")),
    }
    if returning {
        planner.require(planner.caps.returning, "update_many().returning()")?;
    }
    let where_ = planner.apply_filters(filters, without_defaults)?;
    let pk_col = col(&table, &cols[0].column);
    let mut stmts = vec![];
    for chunk in rows.chunks(chunk_rows.max(1)) {
        let mut stmt = Query::update();
        stmt.table(Alias::new(&table));
        if planner.caps.update_from_values {
            for (i, c) in cols.iter().enumerate().skip(1) {
                stmt.value(Alias::new(&c.column), write_expr(col("v", &format!("column{}", i + 1)), c));
            }
            let tuples = chunk.iter().map(|r| sea_query::ValueTuple::Many(r.clone())).collect();
            stmt.from(sea_query::TableRef::ValuesList(tuples, Alias::new("v").into_iden()));
            stmt.and_where(pk_col.clone().eq(col("v", "column1")));
        } else {
            for (i, c) in cols.iter().enumerate().skip(1) {
                let mut case = sea_query::CaseStatement::new();
                for r in chunk {
                    case = case.case(pk_col.clone().eq(bind(r[0].clone(), Some(cols[0]))), bind(r[i].clone(), Some(c)));
                }
                let case: SExpr = case.into();
                stmt.value(Alias::new(&c.column), case);
            }
            stmt.and_where(pk_col.clone().is_in(chunk.iter().map(|r| bind(r[0].clone(), Some(cols[0])))));
        }
        for w in &where_ {
            stmt.and_where(w.clone());
        }
        if returning {
            #[cfg(feature = "composition")]
            stmt.returning(Query::returning().exprs(m.fields().iter().enumerate().map(|(pos, f)| if m.native.computed().contains(&pos) { SExpr::cust("NULL") } else { read_col(&table, f) })));
            #[cfg(not(feature = "composition"))]
            stmt.returning(Query::returning().exprs(m.fields().iter().map(|f| read_col(&table, f))));
        }
        stmts.push(stmt);
    }
    Ok((stmts, returning.then(|| m.fields().iter().map(|f| f.value_type()).collect())))
}

/// An expression assigned to field `f`: through its `write_sql` template, if any.
fn write_expr(e: SExpr, f: &FieldIr) -> SExpr {
    match &f.write_sql {
        Some(t) => SExpr::cust_with_expr(t.replace("{}", "$1"), e),
        None => e,
    }
}

/// Selected field positions in output order; a whole-model row needs no allocation.
enum Positions {
    All(usize),
    Shaped(Vec<usize>),
}

impl Positions {
    fn iter(&self) -> std::iter::Chain<std::ops::Range<usize>, std::iter::Copied<std::slice::Iter<'_, usize>>> {
        let (all, shaped): (_, &[usize]) = match self { Positions::All(n) => (0..*n, &[]), Positions::Shaped(v) => (0..0, v) };
        all.chain(shaped.iter().copied())
    }

    #[cfg(feature = "composition")]
    fn to_vec(&self) -> Vec<usize> { self.iter().collect() }
}

fn shape_positions(model: &Model, shape: Option<&orm_core::behavior::ResultShape>) -> Positions {
    match shape { Some(shape) => Positions::Shaped(shape.fields.iter().map(|f| f.field.position).collect()), None => Positions::All(model.fields().len()) }
}

pub type Returned = (usize, Vec<ValueType>, Option<orm_core::behavior::ResultShape>);
pub type ReturnColumns = (Vec<ValueType>, Option<orm_core::behavior::ResultShape>);

/// Plans `op`, an update, without running it, and tells whether its filters pin one
/// row: an `and`-reachable `field == param` on the primary key or a unique field, with
/// a non-null parameter. Packages that must change exactly one row check this first.
pub fn unique_row_update(schema: &Schema, target: Target, op: &Operation, params: &dyn Params) -> Result<bool> {
    let Operation::Update(update) = op else { return Err(Error::query("unique_row_update takes an update")) };
    Planner::plan(schema, target, op, params)?;
    let model = schema.model(schema.model_idx(&update.model).map_err(query_err)?);
    fn proves(e: &Expr, model: &orm_core::schema::Model, params: &dyn Params) -> Result<bool> {
        match e {
            Expr::And { items } => {
                for item in items { if proves(item, model, params)? { return Ok(true); } }
                Ok(false)
            }
            Expr::Cmp { op: CmpOp::Eq, l, r } => {
                for (column, parameter) in [(l, r), (r, l)] {
                    let (Expr::Col { path, name }, Expr::Param { i }) = (column.as_ref(), parameter.as_ref()) else { continue };
                    let Ok(field) = model.field(name) else { continue };
                    if !path.is_empty() || !(field.primary_key || field.unique) || *i >= params.len() { continue; }
                    let value = params.value(*i, Some(field.value_type()))?;
                    if value != value.as_null() { return Ok(true); }
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }
    for filter in &update.filters {
        if proves(filter, model, params)? { return Ok(true); }
    }
    Ok(false)
}
