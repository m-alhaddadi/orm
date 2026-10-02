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
//! * `prefetch` loads to-many relations with one extra `WHERE fk IN (...)` query each.

use pyo3::prelude::*;
use pyo3::types::PyList;
use sea_query::{
    self,
    extension::postgres::PgExpr, Alias, IntoIden, DeleteStatement, Expr as SExpr, ExprTrait, InsertStatement,
    JoinType, LikeExpr, LockBehavior, LockType, Order as SOrder, Query, SelectStatement, UpdateStatement,
};

use crate::convert::py_to_value;
use crate::errors::query_err;
use orm_core::ir::{
    ArithOp, Assignment, CmpOp, ColType, Cte, Delete, Expr, FieldIr, Frame, FrameKind, Lock, Operation, Order,
    Prefetch, RelKind, Select, SelectItem, Update,
};
use orm_core::dialect::{Capabilities, Target};
use orm_core::schema::{Model, Schema};

/// A `select_related` object inside each row: its columns start at `start` and it is
/// attached to the root object (`parent` None) or to an earlier join's object.
pub struct JoinShape {
    pub parent: Option<usize>,
    pub attr: String,
    pub model: usize,
    pub start: usize,
    pub pk_pos: usize,
}

/// How a statement's rows become objects.
pub enum Output {
    /// Instances of `model` (its fields first), with `select_related` objects attached.
    Instances { model: usize, joins: Vec<JoinShape> },
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
    pub key_type: ColType,
    /// Position of the relation's `to` field in the related rows.
    pub child_key_pos: usize,
    /// The related model's to-one relation back to the parent, filled in too.
    pub back: Option<String>,
    stmt: SelectStatement,
    key: SExpr,
    key_field: FieldIr,
    /// A slice per parent: (offset, limit) over the `_rn` column `stmt` computes.
    slice: Option<(u64, Option<u64>)>,
    pub types: Vec<ColType>,
    pub output: Output,
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
    pub types: Vec<ColType>,
    pub output: Output,
    pub prefetch: Vec<PrefetchPlan>,
}

pub enum Plan {
    Select(SelectPlan),
    Count(SelectStatement),
    Exists(SelectStatement),
    /// The model and column types of the returned rows when the update has `RETURNING`.
    Update(UpdateStatement, Option<(usize, Vec<ColType>)>),
    /// The model and column types of the returned rows when the delete has `RETURNING`.
    Delete(DeleteStatement, Option<(usize, Vec<ColType>)>),
}

/// What a bound value is compared with or assigned to: its type drives the conversion
/// and its field the `write_sql` template.
#[derive(Clone, Copy, Default)]
struct Hint<'s> {
    ty: Option<ColType>,
    field: Option<&'s FieldIr>,
}

impl<'s> Hint<'s> {
    fn ty(ty: ColType) -> Self {
        Hint { ty: Some(ty), field: None }
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
fn returning_col(f: &FieldIr) -> SExpr {
    let c = SExpr::col(Alias::new(&f.column));
    match &f.read_sql {
        Some(t) => SExpr::cust_with_expr(t.replace("{}", "$1"), c),
        None => c,
    }
}

/// A bound value for a field: through the field's `write_sql` template, if any.
fn bind(v: sea_query::Value, f: Option<&FieldIr>) -> SExpr {
    match f.and_then(|f| f.write_sql.as_ref()) {
        Some(t) => SExpr::cust_with_expr(t.replace("{}", "$1"), SExpr::val(v)),
        None => SExpr::val(v),
    }
}

fn fold(items: Vec<SExpr>, and: bool) -> SExpr {
    let mut it = items.into_iter();
    match it.next() {
        None => SExpr::cust(if and { "TRUE" } else { "FALSE" }),
        Some(first) => it.fold(first, |acc, e| if and { acc.and(e) } else { acc.or(e) }),
    }
}

const AGGREGATES: [&str; 5] = ["count", "sum", "avg", "min", "max"];
const SCALAR_FUNCS: [&str; 6] = ["lower", "upper", "length", "abs", "coalesce", "now"];
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
pub fn derive_ctes(schema: &Schema, target: Target, ctes: &[Cte], params: &[Bound<'_, PyAny>]) -> PyResult<Vec<Model>> {
    let mut virt: Vec<Model> = vec![];
    for cte in ctes {
        if schema.model_idx(&cte.name).is_ok() || virt.iter().any(|m| m.ir.name == cte.name) {
            return Err(query_err(format!("CTE name {:?} is already taken", cte.name)));
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
                            let name = name.as_deref().ok_or_else(|| query_err("CTE columns need names".into()))?;
                            fields.push(FieldIr::plain(name, p.expr_type(expr)?));
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

pub struct Planner<'s, 'py> {
    schema: &'s Schema,
    /// The statement's CTEs, as models (see `derive_ctes`).
    virt: &'s [Model],
    params: &'s [Bound<'py, PyAny>],
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
    /// (which adds it to `FROM`).
    recursive: Option<(String, bool)>,
    next_alias: usize,
    /// `EXCLUDED.<field>` is only valid in an upsert's `DO UPDATE SET`.
    allow_excluded: bool,
    /// Window functions are only valid in the select list and `ORDER BY`.
    allow_window: bool,
    target: Target,
    caps: Capabilities,
}

impl<'s, 'py> Planner<'s, 'py> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        schema: &'s Schema,
        virt: &'s [Model],
        target: Target,
        model: &str,
        from: Option<&str>,
        params: &'s [Bound<'py, PyAny>],
        outer: Vec<(String, usize)>,
        next_alias: usize,
    ) -> PyResult<Self> {
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
            next_alias,
            allow_excluded: false,
            allow_window: false,
            target,
            caps: target.caps,
        };
        p.root = p.model_idx(model)?;
        p.source = match from {
            None => p.model(p.root).table().to_owned(),
            Some(cte) => {
                let cols = p.model(p.model_idx(cte)?);
                if p.root >= schema.models.len() || cols.ir.name != cte {
                    return Err(query_err(format!("{cte:?} is not a CTE")));
                }
                for f in p.model(p.root).fields() {
                    if cols.field(&f.name).is_err() {
                        return Err(query_err(format!(
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

    pub fn plan(schema: &'s Schema, target: Target, op: &Operation, params: &'s [Bound<'py, PyAny>]) -> PyResult<Plan> {
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
                let (mut stmt, types) = p.update(q)?;
                if let Some(w) = p.with_clause(&q.with)? {
                    stmt.with_cte(w);
                }
                Plan::Update(stmt, types.map(|t| (p.root, t)))
            }
            Operation::Delete(q) => {
                let virt = derive_ctes(schema, target, &q.with, params)?;
                let mut p = Planner::new(schema, &virt, target, &q.model, None, params, vec![], 0)?;
                let (mut stmt, types) = p.delete(q)?;
                if let Some(w) = p.with_clause(&q.with)? {
                    stmt.with_cte(w);
                }
                Plan::Delete(stmt, types.map(|t| (p.root, t)))
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

    fn model_idx(&self, name: &str) -> PyResult<usize> {
        match self.schema.model_idx(name) {
            Ok(i) => Ok(i),
            Err(e) => match self.virt.iter().position(|m| m.ir.name == name) {
                Some(i) => Ok(self.schema.models.len() + i),
                None => Err(query_err(e)),
            },
        }
    }

    fn walk(&self, root: usize, path: &[String]) -> PyResult<usize> {
        let mut cur = root;
        for hop in path {
            cur = self.model(cur).relation(hop).map_err(query_err)?.1;
        }
        Ok(cur)
    }

    /// `QueryError` unless the dialect supports `feature`.
    fn require(&self, supported: bool, feature: &str) -> PyResult<()> {
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

    // -- subqueries and CTEs ----------------------------------------------------------------

    /// A planner for a subquery: it sees this query's root (and the ones enclosing it)
    /// through `outer()`.
    fn child(&self, q: &Select) -> PyResult<Planner<'s, 'py>> {
        if !q.with.is_empty() {
            return Err(query_err("a subquery can't declare CTEs; declare them on the outermost query".into()));
        }
        let mut outer = self.outer.clone();
        outer.push((self.root_alias().to_owned(), self.root));
        Planner::new(self.schema, self.virt, self.target, &q.model, q.from.as_deref(), self.params, outer, self.next_alias)
    }

    /// The statement of a subquery. For `EXISTS` a query without columns selects `1`.
    fn subselect(&mut self, q: &Select, exists: bool) -> PyResult<SelectStatement> {
        if q.lock.is_some() {
            return Err(query_err("a subquery can't lock rows".into()));
        }
        let mut child = self.child(q)?;
        let stmt = if exists && q.columns.is_none() { child.sliced_inner(q)? } else { child.build_select(q)?.stmt };
        self.next_alias = child.next_alias;
        Ok(stmt)
    }

    /// The single column of a subquery used as a value.
    fn one_column<'q>(q: &'q Select, what: &str) -> PyResult<&'q Expr> {
        match q.columns.as_deref() {
            Some([SelectItem::Expr { expr, .. }]) => Ok(expr),
            _ => Err(query_err(format!("{what} takes a query that selects exactly one column"))),
        }
    }

    /// `WITH [RECURSIVE] <name> (<columns>) AS (...), ...` for this statement's CTEs.
    fn with_clause(&mut self, ctes: &[Cte]) -> PyResult<Option<sea_query::WithClause>> {
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
    fn cte_body(&mut self, q: &Select, recursive: Option<&str>) -> PyResult<SelectStatement> {
        if !q.with.is_empty() || !q.prefetch.is_empty() || !q.select_related.is_empty() || q.lock.is_some() {
            return Err(query_err("a CTE's query can't declare CTEs, prefetch, select_related or lock".into()));
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
                return Err(query_err(format!(
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
    fn cte_col(&mut self, cte: &str, name: &str) -> PyResult<(String, &'s FieldIr)> {
        let f = self.model(self.model_idx(cte)?).field(name).map_err(query_err)?;
        if cte == self.source {
            return Ok((self.root_alias().to_owned(), f));
        }
        if let Some((r, used)) = &mut self.recursive {
            if r == cte {
                *used = true;
                return Ok((cte.to_owned(), f));
            }
        }
        Err(query_err(format!(
            "{cte}.c.{name} is only available in queries reading {cte} (from_({cte}) or {cte}.select(...))"
        )))
    }

    // -- filters ------------------------------------------------------------------------

    fn apply_filters(&mut self, filters: &[Expr]) -> PyResult<Vec<SExpr>> {
        filters.iter().map(|f| self.cond(f)).collect()
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

    /// `EXISTS (SELECT 1 FROM <target> AS tN WHERE tN.to = <scope>.from AND <body>)`.
    fn exists_via(
        &mut self,
        hop: &str,
        body: impl FnOnce(&mut Self) -> PyResult<SExpr>,
    ) -> PyResult<SExpr> {
        let cur = self.scope();
        let cur_model = self.model(cur.model);
        let (rel, target) = cur_model.relation(hop).map_err(query_err)?;
        let from_col = &cur_model.field(&rel.from).map_err(query_err)?.column;
        let target_model = self.model(target);
        let to_col = &target_model.field(&rel.to).map_err(query_err)?.column;
        let mut path = cur.path.clone();
        path.push(hop.to_owned());
        let outer_alias = cur.alias.clone();
        let alias = self.alias("t");
        let link = col(&alias, to_col).eq(col(&outer_alias, from_col));

        self.scopes.push(Scope { path, model: target, alias: alias.clone() });
        let body = body(self);
        self.scopes.pop();

        let mut sub = Query::select();
        sub.expr(SExpr::val(1))
            .from_as(Alias::new(target_model.table()), Alias::new(&alias))
            .and_where(link)
            .and_where(body?);
        Ok(SExpr::exists(sub))
    }

    fn cond(&mut self, e: &Expr) -> PyResult<SExpr> {
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
                        let inner = members.iter().map(|m| p.cond(m)).collect::<PyResult<Vec<_>>>()?;
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

    fn leaf(&mut self, e: &Expr) -> PyResult<SExpr> {
        Ok(match e {
            Expr::Cmp { op, l, r } => {
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
                }
            }
            Expr::In { item, values, neg } => {
                let hint = self.hint_of(item);
                let item = self.value(item, hint)?;
                let values = values.iter().map(|v| self.value(v, hint)).collect::<PyResult<Vec<_>>>()?;
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
                    Expr::Param { i } => self.param(*i)?.extract::<String>()?,
                    _ => return Err(query_err("LIKE pattern must be a string parameter".into())),
                };
                if *ci && !self.caps.ilike {
                    // LOWER(x) LIKE LOWER(pattern), for databases without ILIKE.
                    let lowered = SExpr::cust_with_expr("LOWER($1)", item);
                    let pattern = LikeExpr::new(text.to_lowercase());
                    return Ok(if *neg { lowered.not_like(pattern) } else { lowered.like(pattern) });
                }
                let pattern = LikeExpr::new(text);
                match (ci, neg) {
                    (false, false) => item.like(pattern),
                    (false, true) => item.not_like(pattern),
                    (true, false) => item.ilike(pattern),
                    (true, true) => item.not_ilike(pattern),
                }
            }
            other => self.value(other, Hint::ty(ColType::Bool))?,
        })
    }

    // -- value expressions --------------------------------------------------------------

    fn param(&self, i: usize) -> PyResult<&Bound<'py, PyAny>> {
        self.params.get(i).ok_or_else(|| query_err(format!("parameter {i} out of range")))
    }

    fn outer_field(&self, depth: usize, name: &str) -> PyResult<(&str, &'s FieldIr)> {
        if depth == 0 || depth > self.outer.len() {
            return Err(query_err(format!("outer() column {name:?} has no enclosing query at that depth")));
        }
        let (alias, model) = &self.outer[self.outer.len() - depth];
        Ok((alias, self.model(*model).field(name).map_err(query_err)?))
    }

    fn hint_of(&self, e: &Expr) -> Hint<'s> {
        match e {
            Expr::Col { path, name } => {
                let field = self.walk(self.root, path).ok().and_then(|m| self.model(m).field(name).ok());
                Hint { ty: field.map(|f| f.ty), field }
            }
            Expr::Excluded { name } => {
                let field = self.model(self.root).field(name).ok();
                Hint { ty: field.map(|f| f.ty), field }
            }
            Expr::Outer { depth, name } => {
                let field = self.outer_field(*depth, name).ok().map(|(_, f)| f);
                Hint { ty: field.map(|f| f.ty), field }
            }
            Expr::CteCol { cte, name } => {
                let field = self.model_idx(cte).ok().and_then(|m| self.model(m).field(name).ok());
                Hint { ty: field.map(|f| f.ty), field }
            }
            // Arithmetic results are plain values: no write_sql cast.
            Expr::Arith { l, r, .. } => Hint { ty: self.hint_of(l).or(self.hint_of(r)).ty, field: None },
            Expr::Func { .. } | Expr::Subquery { .. } | Expr::Window { .. } => {
                Hint { ty: self.expr_type(e).ok(), field: None }
            }
            _ => Hint::default(),
        }
    }

    fn resolve(&self, path: &[String], name: &str) -> PyResult<SExpr> {
        let (alias, column) = self.resolve_parts(path, name)?;
        Ok(col(&alias, &column))
    }

    fn value(&mut self, e: &Expr, hint: Hint<'s>) -> PyResult<SExpr> {
        Ok(match e {
            Expr::Col { path, name } => self.resolve(path, name)?,
            Expr::Param { i } => bind(py_to_value(self.param(*i)?, hint.ty)?, hint.field),
            Expr::Const { value } => SExpr::val(*value),
            Expr::Int { value } => SExpr::cust(value.to_string()),
            Expr::Excluded { name } => {
                if !self.allow_excluded {
                    return Err(query_err("excluded() can only be used in on_conflict(...).do_update()".into()));
                }
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
            Expr::Window { func, partition_by, order_by, frame } => self.window(func, partition_by, order_by, frame)?,
            Expr::Func { name, args, rel, distinct } => self.func(name, args, rel.as_deref(), *distinct)?,
            Expr::Arith { op, l, r } => {
                let inner = self.hint_of(l).or(self.hint_of(r));
                let hint = Hint { ty: inner.ty.or(hint.ty), field: None };
                let l = self.value(l, hint)?;
                let r = self.value(r, hint)?;
                match op {
                    ArithOp::Add => l.add(r),
                    ArithOp::Sub => l.sub(r),
                    ArithOp::Mul => l.mul(r),
                    ArithOp::Div => l.div(r),
                }
            }
            cond => self.cond(cond)?,
        })
    }

    // -- functions ----------------------------------------------------------------------

    /// The column type of an expression's result, which decodes it.
    fn expr_type(&self, e: &Expr) -> PyResult<ColType> {
        Ok(match e {
            Expr::Col { path, name } => {
                let m = self.walk(self.root, path)?;
                self.model(m).field(name).map_err(query_err)?.ty
            }
            Expr::Outer { depth, name } => self.outer_field(*depth, name)?.1.ty,
            Expr::CteCol { cte, name } => self.model(self.model_idx(cte)?).field(name).map_err(query_err)?.ty,
            Expr::Int { .. } => ColType::BigInt,
            Expr::Subquery { select } => self.child(select)?.expr_type(Self::one_column(select, "as_scalar()")?)?,
            Expr::Window { func, .. } => self.expr_type(func)?,
            Expr::Arith { l, r, .. } => match self.expr_type(l) {
                Ok(t) => t,
                Err(_) => self.expr_type(r)?,
            },
            Expr::Func { name, args, .. } => {
                let first = || match args.first() {
                    Some(a) => self.expr_type(a),
                    None => Err(query_err(format!("{name}() needs an argument"))),
                };
                match name.as_str() {
                    "count" | "row_number" | "rank" | "dense_rank" => ColType::BigInt,
                    "avg" | "percent_rank" | "cume_dist" => ColType::Float,
                    "length" | "ntile" => ColType::Int,
                    "lower" | "upper" => ColType::Text,
                    "now" => ColType::DateTime,
                    // SUM of integers is cast to bigint (see `func`).
                    "sum" => match first()? {
                        ColType::Int | ColType::BigInt => ColType::BigInt,
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
            | Expr::Exists { .. } => ColType::Bool,
            Expr::Param { .. } => {
                return Err(query_err("select() takes columns and expressions, not plain values".into()))
            }
            Expr::Excluded { .. } => return Err(query_err("excluded() is only valid in do_update()".into())),
        })
    }

    /// `name(args)`. An aggregate whose arguments (or `rel`) go through relations below
    /// the current scope is computed per row, in a correlated subquery over those
    /// relations: `func.count(User.posts)` is `(SELECT COUNT(*) FROM posts WHERE
    /// posts.author_id = users.id)`, never a JOIN that would multiply rows.
    fn func(&mut self, name: &str, args: &[Expr], rel: Option<&[String]>, distinct: bool) -> PyResult<SExpr> {
        if WINDOW_FUNCS.contains(&name) {
            return Err(query_err(format!("{name}() is a window function: add .over(...)")));
        }
        if !is_aggregate(name) && !SCALAR_FUNCS.contains(&name) {
            return Err(query_err(format!("unknown function {name}()")));
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
                    return Err(query_err(format!(
                        "{name}() over relations needs all its columns on one relation path"
                    )));
                }
                return self.aggregate_subquery(name, args, &path, rel.is_some(), distinct);
            }
            if rel.is_none() && args.is_empty() && name != "count" {
                return Err(query_err(format!("{name}() needs an argument")));
            }
        }
        self.call(name, args, distinct)
    }

    /// `<func> OVER (PARTITION BY ... ORDER BY ... <frame>)`, over this query's rows.
    fn window(&mut self, func: &Expr, partition_by: &[Expr], order_by: &[Order], frame: &Option<Frame>) -> PyResult<SExpr> {
        if !self.allow_window {
            return Err(query_err(
                "window functions can only be used in select() and order_by(), not in filters, \
                 group_by(), having() or updates; select them in a CTE to filter on them"
                    .into(),
            ));
        }
        let Expr::Func { name, args, rel, distinct } = func else {
            return Err(query_err("over() applies to a function".into()));
        };
        if rel.is_some() || !(is_aggregate(name) || WINDOW_FUNCS.contains(&name.as_str())) {
            return Err(query_err(format!("{name}() can't be used as a window function")));
        }
        let (call, cast) = self.call_parts(name, args, *distinct)?;
        let mut exprs = vec![call];
        let mut clauses = vec![];
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
        let sql = format!("$1 OVER ({})", clauses.join(" "));
        Ok(SExpr::cust_with_exprs(
            match cast {
                Some(ty) => format!("CAST({sql} AS {ty})"),
                None => sql,
            },
            exprs,
        ))
    }

    /// The SQL call itself, arguments planned in the current scope.
    fn call(&mut self, name: &str, args: &[Expr], distinct: bool) -> PyResult<SExpr> {
        let (e, cast) = self.call_parts(name, args, distinct)?;
        Ok(match cast {
            Some(ty) => SExpr::cust_with_expr(format!("CAST($1 AS {ty})"), e),
            None => e,
        })
    }

    /// The call, and the type its result is cast to (outside a window's `OVER`).
    fn call_parts(&mut self, name: &str, args: &[Expr], distinct: bool) -> PyResult<(SExpr, Option<&'static str>)> {
        let hint = args.first().map(|a| self.hint_of(a)).unwrap_or_default();
        let hint = Hint { ty: hint.ty, field: None };
        let planned = args.iter().map(|a| self.value(a, hint)).collect::<PyResult<Vec<_>>>()?;
        let d = if distinct { "DISTINCT " } else { "" };
        let n = planned.len();
        let call = |sql: &str, planned: Vec<SExpr>| -> PyResult<SExpr> {
            let slots = (1..=planned.len()).map(|i| format!("${i}")).collect::<Vec<_>>().join(", ");
            Ok(SExpr::cust_with_exprs(format!("{sql}({slots})"), planned))
        };
        let one = |tpl: &str, planned: Vec<SExpr>| -> PyResult<SExpr> {
            match <[SExpr; 1]>::try_from(planned) {
                Ok([a]) => Ok(SExpr::cust_with_expr(tpl.to_owned(), a)),
                Err(_) => Err(query_err(format!("{name}() takes one argument"))),
            }
        };
        let mut cast = None;
        let e = match name {
            "count" if planned.is_empty() => SExpr::cust("COUNT(*)"),
            "count" => one(&format!("COUNT({d}$1)"), planned)?,
            "sum" => {
                let ty = args.first().map(|a| self.expr_type(a)).transpose()?;
                if matches!(ty, Some(ColType::Int | ColType::BigInt)) {
                    cast = Some("BIGINT");
                }
                one(&format!("SUM({d}$1)"), planned)?
            }
            "avg" => {
                cast = Some("DOUBLE PRECISION");
                one(&format!("AVG({d}$1)"), planned)?
            }
            "min" => one("MIN($1)", planned)?,
            "max" => one("MAX($1)", planned)?,
            "lower" => one("LOWER($1)", planned)?,
            "upper" => one("UPPER($1)", planned)?,
            "length" => one("LENGTH($1)", planned)?,
            "abs" => one("ABS($1)", planned)?,
            "now" if planned.is_empty() => SExpr::cust("CURRENT_TIMESTAMP"),
            "coalesce" if !planned.is_empty() => call("COALESCE", planned)?,
            "row_number" | "rank" | "dense_rank" | "percent_rank" | "cume_dist" if planned.is_empty() => {
                SExpr::cust(format!("{}()", name.to_uppercase()))
            }
            "ntile" | "first_value" | "last_value" => one(&format!("{}($1)", name.to_uppercase()), planned)?,
            "lag" | "lead" if (1..=3).contains(&n) => call(&name.to_uppercase(), planned)?,
            "nth_value" if n == 2 => call("NTH_VALUE", planned)?,
            _ => return Err(query_err(format!("wrong arguments for {name}()"))),
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
    ) -> PyResult<SExpr> {
        let base = self.scope().path.clone();
        let hops = &path[base.len()..];
        let mut sub = Query::select();
        let mut outer = (self.scope().alias.clone(), self.scope().model);
        let pushed = self.scopes.len();
        for (k, hop) in hops.iter().enumerate() {
            let m = self.model(outer.1);
            let (rel, target) = m.relation(hop).map_err(query_err)?;
            let from_col = &m.field(&rel.from).map_err(query_err)?.column;
            let tm = self.model(target);
            let to_col = &tm.field(&rel.to).map_err(query_err)?.column;
            let alias = self.alias("a");
            let link = col(&alias, to_col).eq(col(&outer.0, from_col));
            if k == 0 {
                sub.from_as(Alias::new(tm.table()), Alias::new(&alias)).and_where(link);
            } else {
                sub.join_as(JoinType::InnerJoin, Alias::new(tm.table()), Alias::new(&alias), link);
            }
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
    fn select_columns(&mut self, q: &Select, items: &[SelectItem], cte: bool) -> PyResult<SelectPlan> {
        if !q.select_related.is_empty() || !q.prefetch.is_empty() {
            return Err(query_err("select() can't be combined with select_related / prefetch_related".into()));
        }
        if items.is_empty() {
            return Err(query_err("select() needs at least one column".into()));
        }
        let root = self.model(self.root);
        let alias = self.root_alias().to_owned();
        let mut stmt = Query::select();
        let mut types = vec![];
        let mut shape = vec![];
        let mut aggregated = !q.group_by.is_empty() || !q.having.is_empty();
        let mut windowed = false;
        for item in items {
            match item {
                SelectItem::Model => {
                    if self.root >= self.schema.models.len() {
                        return Err(query_err("a CTE without a model has no model to select".into()));
                    }
                    for f in root.fields() {
                        if cte {
                            stmt.expr_as(col(&alias, &f.column), Alias::new(&f.column));
                        } else {
                            stmt.expr(read_col(&alias, f));
                        }
                        types.push(f.ty);
                    }
                    shape.push(Some(root.fields().len()));
                }
                SelectItem::Expr { expr, name } => {
                    self.join_paths(expr, "select()")?;
                    aggregated |= has_local_aggregate(expr);
                    windowed |= has_window(expr);
                    types.push(self.expr_type(expr)?);
                    self.allow_window = true;
                    let e = self.value(expr, Hint::default());
                    self.allow_window = false;
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
                        (true, None) => return Err(query_err("CTE columns need names".into())),
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
                    return Err(query_err("distinct(on=...) takes columns".into()));
                };
                self.join_paths(e, "distinct(on=...)")?;
                let (alias, column) = self.resolve_parts(path, name)?;
                cols.push((Alias::new(alias), Alias::new(column)));
            }
            stmt.distinct_on(cols);
        } else if q.distinct {
            stmt.distinct();
        }
        windowed |= q.order.iter().any(|o| has_window(&o.expr));
        self.base_select(q, &mut stmt, true)?;
        self.apply_joins(&mut stmt);
        if let Some(lock) = q.lock {
            if aggregated || windowed || q.distinct || !q.distinct_on.is_empty() {
                return Err(query_err(
                    "lock() can't be used with aggregates, window functions, group_by() or distinct()".into(),
                ));
            }
            self.apply_lock(&mut stmt, lock)?;
        }
        Ok(SelectPlan { stmt, types, output: Output::Rows { model: self.root, items: shape }, prefetch: vec![] })
    }

    /// LEFT JOINs for the to-one paths `e` reads outside aggregates.
    fn join_paths(&mut self, e: &Expr, why: &str) -> PyResult<()> {
        let (mut paths, mut has_not) = (vec![], false);
        col_paths(e, &mut paths, &mut has_not);
        for p in paths.into_iter().filter(|p| !p.is_empty()) {
            self.ensure_join(p, why).map_err(|_| {
                query_err(format!(
                    "{why} can follow only to-one relations, {} goes through a to-many one: \
                     aggregate it instead, e.g. func.count(...)",
                    p.join(".")
                ))
            })?;
        }
        Ok(())
    }

    fn resolve_parts(&self, path: &[String], name: &str) -> PyResult<(String, String)> {
        let s = self
            .scopes
            .iter()
            .rev()
            .chain(self.joins.iter().map(|(s, _)| s))
            .find(|s| s.path == path)
            .ok_or_else(|| {
                query_err(format!(
                    "{}.{name} is not reachable here: one comparison can follow only one relation path",
                    path.join(".")
                ))
            })?;
        let f = self.model(s.model).field(name).map_err(query_err)?;
        Ok((s.alias.clone(), f.column.clone()))
    }

    // -- joins (select_related / order_by) ----------------------------------------------

    /// Adds LEFT JOINs for every prefix of `path`; returns (alias, model) of the last hop.
    fn ensure_join(&mut self, path: &[String], why: &str) -> PyResult<(String, usize)> {
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
                return Err(query_err(format!(
                    "{why} can only follow to-one relations; {} is to-many",
                    prefix.join(".")
                )));
            }
            let from_col = &m.field(&rel.from).map_err(query_err)?.column;
            let to_col = &self.model(target).field(&rel.to).map_err(query_err)?.column;
            let new_alias = self.alias("j");
            let on = col(&new_alias, to_col).eq(col(&alias, from_col));
            self.joins.push((Scope { path: prefix.to_vec(), model: target, alias: new_alias.clone() }, on));
            (alias, model) = (new_alias, target);
        }
        Ok((alias, model))
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
    fn base_select(&mut self, q: &Select, stmt: &mut SelectStatement, order: bool) -> PyResult<()> {
        let alias = self.root_alias().to_owned();
        if alias == self.source {
            stmt.from(Alias::new(&self.source));
        } else {
            stmt.from_as(Alias::new(&self.source), Alias::new(&alias));
        }
        for w in self.apply_filters(&q.filters)? {
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
            stmt.limit(n);
        }
        if let Some(n) = q.offset {
            stmt.offset(n);
        }
        if let Some((name, true)) = &self.recursive {
            stmt.from(Alias::new(name));
        }
        Ok(())
    }

    fn apply_lock(&self, stmt: &mut SelectStatement, lock: Lock) -> PyResult<()> {
        if self.source != self.model(self.root).table() {
            return Err(query_err("lock() can't be used on a query reading a CTE".into()));
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

    /// A SELECT of instances (with `select_related` and `prefetch`) or of `select(...)`
    /// columns.
    fn build_select(&mut self, q: &Select) -> PyResult<SelectPlan> {
        if let Some(items) = &q.columns {
            return self.select_columns(q, items, false);
        }
        if !q.group_by.is_empty() || !q.having.is_empty() || q.distinct || !q.distinct_on.is_empty() {
            return Err(query_err("group_by / having / distinct need select(...)".into()));
        }
        if self.root >= self.schema.models.len() {
            return Err(query_err("a CTE without a model is read with select(...)".into()));
        }
        let root = self.model(self.root);
        let alias = self.root_alias().to_owned();
        let mut stmt = Query::select();
        let mut types = vec![];
        for f in root.fields() {
            stmt.expr(read_col(&alias, f));
            types.push(f.ty);
        }
        let mut joins: Vec<JoinShape> = vec![];
        for path in &q.select_related {
            let (alias, model) = self.ensure_join(path, "select_related")?;
            let m = self.model(model);
            let parent = match path.len() {
                0 => return Err(query_err("select_related needs a relation".into())),
                1 => None,
                n => Some(
                    q.select_related
                        .iter()
                        .position(|p| p.as_slice() == &path[..n - 1])
                        .ok_or_else(|| query_err("select_related paths must list their prefixes first".into()))?,
                ),
            };
            joins.push(JoinShape {
                parent,
                attr: path[path.len() - 1].clone(),
                model,
                start: types.len(),
                pk_pos: m.pk,
            });
            for f in m.fields() {
                stmt.expr(read_col(&alias, f));
                types.push(f.ty);
            }
        }
        self.base_select(q, &mut stmt, true)?;
        self.apply_joins(&mut stmt);
        if q.order.iter().any(|o| has_window(&o.expr)) && q.lock.is_some() {
            return Err(query_err("lock() can't be used with window functions".into()));
        }
        if let Some(lock) = q.lock {
            self.apply_lock(&mut stmt, lock)?;
        }
        let mut prefetch = vec![];
        for node in &q.prefetch {
            prefetch.push(plan_prefetch(self.schema, self.target, self.params, self.root, node)?);
        }
        Ok(SelectPlan { stmt, types, output: Output::Instances { model: self.root, joins }, prefetch })
    }

    fn sliced_inner(&mut self, q: &Select) -> PyResult<SelectStatement> {
        let mut inner = Query::select();
        inner.expr(SExpr::val(1));
        self.base_select(q, &mut inner, true)?;
        self.apply_joins(&mut inner);
        Ok(inner)
    }

    pub fn count(&mut self, q: &Select) -> PyResult<SelectStatement> {
        no_lock(q, "count")?;
        let mut stmt = Query::select();
        if q.limit.is_some() || q.offset.is_some() {
            let inner = self.sliced_inner(q)?;
            stmt.expr(SExpr::cust("COUNT(*)")).from_subquery(inner, Alias::new("sliced"));
        } else {
            stmt.expr(SExpr::cust("COUNT(*)"));
            self.base_select(q, &mut stmt, false)?;
            self.apply_joins(&mut stmt);
        }
        Ok(stmt)
    }

    pub fn exists(&mut self, q: &Select) -> PyResult<SelectStatement> {
        no_lock(q, "exists")?;
        let mut inner = self.sliced_inner(q)?;
        if q.limit.is_none() {
            inner.limit(1);
        }
        let mut stmt = Query::select();
        stmt.expr(SExpr::exists(inner));
        Ok(stmt)
    }

    pub fn update(&mut self, q: &Update) -> PyResult<(UpdateStatement, Option<Vec<ColType>>)> {
        let root = self.model(self.root);
        let mut stmt = Query::update();
        stmt.table(Alias::new(root.table()));
        if q.set.is_empty() {
            return Err(query_err("update() needs at least one field".into()));
        }
        for a in &q.set {
            let f = root.field(&a.field).map_err(query_err)?;
            let v = self.value(&a.value, Hint { ty: Some(f.ty), field: Some(f) })?;
            stmt.value(Alias::new(&f.column), v);
        }
        for w in self.apply_filters(&q.filters)? {
            stmt.and_where(w);
        }
        if !q.returning {
            return Ok((stmt, None));
        }
        self.require(self.caps.returning, "update().returning()")?;
        stmt.returning(Query::returning().exprs(root.fields().iter().map(returning_col)));
        Ok((stmt, Some(root.fields().iter().map(|f| f.ty).collect())))
    }

    pub fn delete(&mut self, q: &Delete) -> PyResult<(DeleteStatement, Option<Vec<ColType>>)> {
        let root = self.model(self.root);
        let mut stmt = Query::delete();
        stmt.from_table(Alias::new(root.table()));
        for w in self.apply_filters(&q.filters)? {
            stmt.and_where(w);
        }
        if !q.returning {
            return Ok((stmt, None));
        }
        self.require(self.caps.returning, "delete().returning()")?;
        stmt.returning(Query::returning().exprs(root.fields().iter().map(returning_col)));
        Ok((stmt, Some(root.fields().iter().map(|f| f.ty).collect())))
    }
}

/// A top-level SELECT with its `WITH` clause.
pub fn plan_select<'py>(schema: &Schema, target: Target, q: &Select, params: &[Bound<'py, PyAny>]) -> PyResult<SelectPlan> {
    let virt = derive_ctes(schema, target, &q.with, params)?;
    let mut p = Planner::new(schema, &virt, target, &q.model, q.from.as_deref(), params, vec![], 0)?;
    let mut plan = p.build_select(q)?;
    if let Some(w) = p.with_clause(&q.with)? {
        plan.stmt.with_cte(w);
    }
    Ok(plan)
}

fn rn_orders(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::Window { order_by, .. } => order_by.iter().map(|o| &o.expr).collect(),
        _ => vec![],
    }
}

/// The query loading `node` (a relation of `parent`) for a set of parent keys. A slice
/// applies per parent: `ROW_NUMBER() OVER (PARTITION BY <key> ORDER BY ...)` numbers
/// the related rows and the outer query keeps the slice.
fn plan_prefetch<'py>(
    schema: &Schema,
    target: Target,
    params: &[Bound<'py, PyAny>],
    parent: usize,
    node: &Prefetch,
) -> PyResult<PrefetchPlan> {
    let pm = schema.model(parent);
    let (rel, child) = pm.relation(&node.relation).map_err(query_err)?;
    let cm = schema.model(child);
    if node.query.model != cm.ir.name {
        return Err(query_err(format!(
            "the query prefetching {}.{} must be over {}, not {}",
            pm.ir.name, node.relation, cm.ir.name, node.query.model
        )));
    }
    if node.query.columns.is_some() || node.query.lock.is_some() {
        return Err(query_err("a prefetch query can't select columns or lock rows".into()));
    }
    let many = rel.kind == RelKind::Many;
    let mut q = node.query.clone();
    let pk_order = Order { expr: Expr::Col { path: vec![], name: cm.pk_field().name.clone() }, desc: false };
    let sliced = q.limit.is_some() || q.offset.is_some();
    let slice = sliced.then(|| (q.offset.unwrap_or(0), q.limit));
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
    let key_field = cm.field(&rel.to).map_err(query_err)?;
    let key = col(p.root_alias(), &key_field.column);
    if sliced {
        let rn = Expr::Window {
            func: Box::new(Expr::Func { name: "row_number".into(), args: vec![], rel: None, distinct: false }),
            partition_by: vec![Expr::Col { path: vec![], name: rel.to.clone() }],
            order_by: window_order,
            frame: None,
        };
        // The window's ORDER BY may follow to-one relations not joined yet.
        let joined = p.joins.len();
        for o in rn_orders(&rn) {
            p.join_paths(o, "a prefetch's order_by()")?;
        }
        p.allow_window = true;
        let e = p.value(&rn, Hint::default())?;
        for (s, on) in &p.joins[joined..] {
            plan.stmt.join_as(JoinType::LeftJoin, Alias::new(p.model(s.model).table()), Alias::new(&s.alias), on.clone());
        }
        plan.stmt.expr_as(e, Alias::new("_rn"));
    }
    if let Some(w) = p.with_clause(&q.with)? {
        plan.stmt.with_cte(w);
    }
    let back = many
        .then(|| {
            cm.ir.relations.iter().find(|r| {
                r.kind == RelKind::One
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
        key_type: pm.field(&rel.from).map_err(query_err)?.ty,
        child_key_pos: cm.field_pos(&rel.to).map_err(query_err)?,
        back,
        stmt: plan.stmt,
        key,
        key_field: key_field.clone(),
        slice,
        types: plan.types,
        output: plan.output,
        children: plan.prefetch,
    })
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

fn no_lock(q: &Select, what: &str) -> PyResult<()> {
    match q.lock {
        Some(_) => Err(query_err(format!("{what}() can't lock rows; lock() applies to reading rows"))),
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
/// `rows` is a list of sequences aligned with `fields`; the `DEFAULT` marker becomes the
/// SQL `DEFAULT` keyword.
pub fn plan_insert<'py>(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: &Bound<'py, PyList>,
    on_conflict: Option<OnConflict>,
    params: &[Bound<'py, PyAny>],
) -> PyResult<(InsertStatement, Vec<ColType>)> {
    let m = schema.model(schema.model_idx(model).map_err(query_err)?);
    let cols = fields.iter().map(|f| m.field(f)).collect::<Result<Vec<_>, _>>().map_err(query_err)?;
    let mut stmt = Query::insert();
    stmt.into_table(Alias::new(m.table()));
    if cols.is_empty() {
        if rows.len() != 1 {
            return Err(query_err("rows without explicit values must be inserted one at a time".into()));
        }
        stmt.or_default_values();
    } else {
        stmt.columns(cols.iter().map(|c| Alias::new(&c.column)));
        for row in rows.iter() {
            let mut values = Vec::with_capacity(cols.len());
            for (c, item) in cols.iter().zip(row.try_iter()?) {
                let item = item?;
                values.push(if item.is_instance_of::<crate::DefaultMarker>() {
                    SExpr::cust("DEFAULT")
                } else {
                    bind(py_to_value(&item, Some(c.ty))?, Some(c))
                });
            }
            if values.len() != cols.len() {
                return Err(query_err("insert row length does not match fields".into()));
            }
            stmt.values(values).map_err(|e| query_err(e.to_string()))?;
        }
    }
    let caps = target.caps;
    let require = |ok: bool, feature: &str| target.require(ok, feature).map_err(query_err);
    require(caps.returning, "insert ... RETURNING")?;
    if let Some(oc) = on_conflict {
        require(caps.on_conflict, "insert(...).on_conflict()")?;
        let columns = |names: &[String]| -> PyResult<Vec<Alias>> {
            names
                .iter()
                .map(|n| m.field(n).map(|f| Alias::new(&f.column)))
                .collect::<Result<_, _>>()
                .map_err(query_err)
        };
        let clause = match oc {
            OnConflict::Nothing(conflict) => sea_query::OnConflict::columns(columns(&conflict)?).do_nothing().to_owned(),
            OnConflict::Update(conflict, update, set) => {
                if update.is_empty() && set.is_empty() {
                    return Err(query_err("on_conflict(...).do_update() has no columns to update".into()));
                }
                let mut clause = sea_query::OnConflict::columns(columns(&conflict)?);
                clause.update_columns(columns(&update)?);
                let mut planner = Planner::new(schema, &[], target, model, None, params, vec![], 0)?;
                planner.allow_excluded = true;
                for a in &set {
                    let f = m.field(&a.field).map_err(query_err)?;
                    let v = planner.value(&a.value, Hint { ty: Some(f.ty), field: Some(f) })?;
                    clause.value(Alias::new(&f.column), v);
                }
                clause.to_owned()
            }
        };
        stmt.on_conflict(clause);
    }
    stmt.returning(Query::returning().exprs(m.fields().iter().map(returning_col)));
    Ok((stmt, m.fields().iter().map(|f| f.ty).collect()))
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
pub fn plan_update_many<'py>(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: &[Vec<sea_query::Value>],
    chunk_rows: usize,
    filters: &[Expr],
    params: &[Bound<'py, PyAny>],
    returning: bool,
) -> PyResult<(Vec<UpdateStatement>, Option<Vec<ColType>>)> {
    let mut planner = Planner::new(schema, &[], target, model, None, params, vec![], 0)?;
    let m = schema.model(planner.root);
    let table = m.table().to_owned();
    let cols = fields.iter().map(|f| m.field(f)).collect::<Result<Vec<_>, _>>().map_err(query_err)?;
    match cols.first() {
        Some(pk) if pk.primary_key && cols.len() > 1 => {}
        _ => return Err(query_err("update_many needs the primary key followed by the fields to set".into())),
    }
    if returning {
        planner.require(planner.caps.returning, "update_many().returning()")?;
    }
    let where_ = planner.apply_filters(filters)?;
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
            stmt.returning(Query::returning().exprs(m.fields().iter().map(|f| read_col(&table, f))));
        }
        stmts.push(stmt);
    }
    Ok((stmts, returning.then(|| m.fields().iter().map(|f| f.ty).collect())))
}

/// An expression assigned to field `f`: through its `write_sql` template, if any.
fn write_expr(e: SExpr, f: &FieldIr) -> SExpr {
    match &f.write_sql {
        Some(t) => SExpr::cust_with_expr(t.replace("{}", "$1"), e),
        None => e,
    }
}
