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
use sea_orm::sea_query::{
    extension::postgres::PgExpr, Alias, DeleteStatement, Expr as SExpr, ExprTrait, InsertStatement,
    JoinType, LikeExpr, Order as SOrder, Query, SelectStatement, UpdateStatement,
};

use crate::convert::py_to_value;
use crate::errors::query_err;
use crate::ir::{ArithOp, CmpOp, ColType, Delete, Expr, Operation, RelKind, Select, Update};
use crate::schema::Schema;

pub struct PrefetchPlan {
    pub name: String,
    /// Position of the relation's `from` field in the root row.
    pub key_pos: usize,
    pub key_type: ColType,
    /// `SELECT <target columns> FROM <target> ORDER BY <pk>`; the executor adds the
    /// `WHERE <to> IN (...)` once the keys are known.
    pub stmt: SelectStatement,
    pub to_table: String,
    pub to_column: String,
    pub types: Vec<ColType>,
}

pub struct SelectPlan {
    pub stmt: SelectStatement,
    pub types: Vec<ColType>,
    pub prefetch: Vec<PrefetchPlan>,
}

pub enum Plan {
    Select(SelectPlan),
    Count(SelectStatement),
    Exists(SelectStatement),
    Update(UpdateStatement),
    Delete(DeleteStatement),
}

struct Scope {
    path: Vec<String>,
    model: usize,
    alias: String,
}

fn col(alias: &str, column: &str) -> SExpr {
    SExpr::col((Alias::new(alias), Alias::new(column)))
}

fn fold(items: Vec<SExpr>, and: bool) -> SExpr {
    let mut it = items.into_iter();
    match it.next() {
        None => SExpr::cust(if and { "TRUE" } else { "FALSE" }),
        Some(first) => it.fold(first, |acc, e| if and { acc.and(e) } else { acc.or(e) }),
    }
}

/// Collects every column path referenced by `e`; sets `has_not` if `e` contains a `NOT`.
fn col_paths<'e>(e: &'e Expr, out: &mut Vec<&'e [String]>, has_not: &mut bool) {
    match e {
        Expr::Col { path, .. } => out.push(path),
        Expr::Param { .. } | Expr::Const { .. } => {}
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

pub struct Planner<'s, 'py> {
    schema: &'s Schema,
    params: &'s [Bound<'py, PyAny>],
    root: usize,
    /// Innermost last. `scopes[0]` is the root table, referenced by its own name so the
    /// same filters work in SELECT, UPDATE and DELETE.
    scopes: Vec<Scope>,
    /// Top-level to-one LEFT JOINs (select_related / order_by), keyed by path.
    joins: Vec<(Scope, SExpr)>,
    next_alias: usize,
}

impl<'s, 'py> Planner<'s, 'py> {
    pub fn new(schema: &'s Schema, model: &str, params: &'s [Bound<'py, PyAny>]) -> PyResult<Self> {
        let root = schema.model_idx(model).map_err(query_err)?;
        let scope = Scope { path: vec![], model: root, alias: schema.model(root).table().to_owned() };
        Ok(Planner { schema, params, root, scopes: vec![scope], joins: vec![], next_alias: 0 })
    }

    pub fn plan(schema: &'s Schema, op: &Operation, params: &'s [Bound<'py, PyAny>]) -> PyResult<Plan> {
        Ok(match op {
            Operation::Select(q) => Plan::Select(Planner::new(schema, &q.model, params)?.select(q)?),
            Operation::Count(q) => Plan::Count(Planner::new(schema, &q.model, params)?.count(q)?),
            Operation::Exists(q) => Plan::Exists(Planner::new(schema, &q.model, params)?.exists(q)?),
            Operation::Update(q) => Plan::Update(Planner::new(schema, &q.model, params)?.update(q)?),
            Operation::Delete(q) => Plan::Delete(Planner::new(schema, &q.model, params)?.delete(q)?),
        })
    }

    fn alias(&mut self, prefix: &str) -> String {
        self.next_alias += 1;
        format!("{prefix}{}", self.next_alias)
    }

    fn scope(&self) -> &Scope {
        self.scopes.last().expect("root scope")
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
        let schema = self.schema;
        let cur = self.scope();
        let cur_model = schema.model(cur.model);
        let (rel, target) = cur_model.relation(hop).map_err(query_err)?;
        let from_col = &cur_model.field(&rel.from).map_err(query_err)?.column;
        let target_model = schema.model(target);
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
                let hint = self.type_of(l).or_else(|| self.type_of(r));
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
                let hint = self.type_of(item);
                let item = self.value(item, hint)?;
                let values = values.iter().map(|v| self.value(v, hint)).collect::<PyResult<Vec<_>>>()?;
                if *neg {
                    item.is_not_in(values)
                } else {
                    item.is_in(values)
                }
            }
            Expr::IsNull { item, neg } => {
                let item = self.value(item, None)?;
                if *neg {
                    item.is_not_null()
                } else {
                    item.is_null()
                }
            }
            Expr::Like { item, pattern, ci, neg } => {
                let item = self.value(item, None)?;
                let pattern = match pattern.as_ref() {
                    Expr::Param { i } => LikeExpr::new(self.param(*i)?.extract::<String>()?),
                    _ => return Err(query_err("LIKE pattern must be a string parameter".into())),
                };
                match (ci, neg) {
                    (false, false) => item.like(pattern),
                    (false, true) => item.not_like(pattern),
                    (true, false) => item.ilike(pattern),
                    (true, true) => item.not_ilike(pattern),
                }
            }
            other => self.value(other, Some(ColType::Bool))?,
        })
    }

    // -- value expressions --------------------------------------------------------------

    fn param(&self, i: usize) -> PyResult<&Bound<'py, PyAny>> {
        self.params.get(i).ok_or_else(|| query_err(format!("parameter {i} out of range")))
    }

    fn type_of(&self, e: &Expr) -> Option<ColType> {
        match e {
            Expr::Col { path, name } => self.schema.col_type(self.root, path, name).ok(),
            Expr::Arith { l, r, .. } => self.type_of(l).or_else(|| self.type_of(r)),
            _ => None,
        }
    }

    fn resolve(&self, path: &[String], name: &str) -> PyResult<SExpr> {
        let found = self
            .scopes
            .iter()
            .rev()
            .chain(self.joins.iter().map(|(s, _)| s))
            .find(|s| s.path == path);
        match found {
            Some(s) => {
                let f = self.schema.model(s.model).field(name).map_err(query_err)?;
                Ok(col(&s.alias, &f.column))
            }
            None => Err(query_err(format!(
                "{}.{name} is not reachable here: one comparison can follow only one \
                 relation path",
                path.join(".")
            ))),
        }
    }

    fn value(&mut self, e: &Expr, hint: Option<ColType>) -> PyResult<SExpr> {
        Ok(match e {
            Expr::Col { path, name } => self.resolve(path, name)?,
            Expr::Param { i } => SExpr::val(py_to_value(self.param(*i)?, hint)?),
            Expr::Const { value } => SExpr::val(*value),
            Expr::Arith { op, l, r } => {
                let hint = self.type_of(l).or_else(|| self.type_of(r)).or(hint);
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

    // -- joins (select_related / order_by) ----------------------------------------------

    /// Adds LEFT JOINs for every prefix of `path`; returns (alias, model) of the last hop.
    fn ensure_join(&mut self, path: &[String], why: &str) -> PyResult<(String, usize)> {
        let schema = self.schema;
        let (mut alias, mut model) = (self.scopes[0].alias.clone(), self.root);
        for i in 0..path.len() {
            let prefix = &path[..=i];
            if let Some((s, _)) = self.joins.iter().find(|(s, _)| s.path == prefix) {
                (alias, model) = (s.alias.clone(), s.model);
                continue;
            }
            let m = schema.model(model);
            let (rel, target) = m.relation(&path[i]).map_err(query_err)?;
            if rel.kind != RelKind::One {
                return Err(query_err(format!(
                    "{why} can only follow to-one relations; {} is to-many",
                    prefix.join(".")
                )));
            }
            let from_col = &m.field(&rel.from).map_err(query_err)?.column;
            let to_col = &schema.model(target).field(&rel.to).map_err(query_err)?.column;
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
                Alias::new(self.schema.model(s.model).table()),
                Alias::new(&s.alias),
                on.clone(),
            );
        }
    }

    fn base_select(&mut self, q: &Select, stmt: &mut SelectStatement, order: bool) -> PyResult<()> {
        let root = self.schema.model(self.root);
        stmt.from(Alias::new(root.table()));
        for w in self.apply_filters(&q.filters)? {
            stmt.and_where(w);
        }
        for o in q.order.iter().filter(|_| order) {
            let (mut paths, mut has_not) = (vec![], false);
            col_paths(&o.expr, &mut paths, &mut has_not);
            for p in paths.into_iter().filter(|p| !p.is_empty()) {
                self.ensure_join(p, "order_by")?;
            }
            let e = self.value(&o.expr, None)?;
            stmt.order_by_expr(e, if o.desc { SOrder::Desc } else { SOrder::Asc });
        }
        if let Some(n) = q.limit {
            stmt.limit(n);
        }
        if let Some(n) = q.offset {
            stmt.offset(n);
        }
        Ok(())
    }

    // -- statements ---------------------------------------------------------------------

    pub fn select(mut self, q: &Select) -> PyResult<SelectPlan> {
        let schema = self.schema;
        let root = schema.model(self.root);
        let mut stmt = Query::select();
        let mut types = vec![];
        for f in root.fields() {
            stmt.expr(col(root.table(), &f.column));
            types.push(f.ty);
        }
        for path in &q.select_related {
            let (alias, model) = self.ensure_join(path, "select_related")?;
            for f in schema.model(model).fields() {
                stmt.expr(col(&alias, &f.column));
                types.push(f.ty);
            }
        }
        self.base_select(q, &mut stmt, true)?;
        self.apply_joins(&mut stmt);

        let mut prefetch = vec![];
        for name in &q.prefetch {
            let (rel, target) = root.relation(name).map_err(query_err)?;
            if rel.kind != RelKind::Many {
                return Err(query_err(format!(
                    "prefetch_related expects a to-many relation; use select_related for {name}"
                )));
            }
            let tm = schema.model(target);
            let mut sub = Query::select();
            for f in tm.fields() {
                sub.expr(col(tm.table(), &f.column));
            }
            sub.from(Alias::new(tm.table()))
                .order_by_expr(col(tm.table(), &tm.pk_field().column), SOrder::Asc);
            prefetch.push(PrefetchPlan {
                name: name.clone(),
                key_pos: root.field_pos(&rel.from).map_err(query_err)?,
                key_type: root.field(&rel.from).map_err(query_err)?.ty,
                stmt: sub,
                to_table: tm.table().to_owned(),
                to_column: tm.field(&rel.to).map_err(query_err)?.column.clone(),
                types: tm.fields().iter().map(|f| f.ty).collect(),
            });
        }
        Ok(SelectPlan { stmt, types, prefetch })
    }

    fn sliced_inner(&mut self, q: &Select) -> PyResult<SelectStatement> {
        let mut inner = Query::select();
        inner.expr(SExpr::val(1));
        self.base_select(q, &mut inner, true)?;
        self.apply_joins(&mut inner);
        Ok(inner)
    }

    pub fn count(mut self, q: &Select) -> PyResult<SelectStatement> {
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

    pub fn exists(mut self, q: &Select) -> PyResult<SelectStatement> {
        let mut inner = self.sliced_inner(q)?;
        if q.limit.is_none() {
            inner.limit(1);
        }
        let mut stmt = Query::select();
        stmt.expr(SExpr::exists(inner));
        Ok(stmt)
    }

    pub fn update(mut self, q: &Update) -> PyResult<UpdateStatement> {
        let root = self.schema.model(self.root);
        let mut stmt = Query::update();
        stmt.table(Alias::new(root.table()));
        if q.set.is_empty() {
            return Err(query_err("update() needs at least one field".into()));
        }
        for a in &q.set {
            let f = root.field(&a.field).map_err(query_err)?;
            let v = self.value(&a.value, Some(f.ty))?;
            stmt.value(Alias::new(&f.column), v);
        }
        for w in self.apply_filters(&q.filters)? {
            stmt.and_where(w);
        }
        Ok(stmt)
    }

    pub fn delete(mut self, q: &Delete) -> PyResult<DeleteStatement> {
        let root = self.schema.model(self.root);
        let mut stmt = Query::delete();
        stmt.from_table(Alias::new(root.table()));
        for w in self.apply_filters(&q.filters)? {
            stmt.and_where(w);
        }
        Ok(stmt)
    }
}

/// `INSERT INTO <table> (<fields>) VALUES ... RETURNING <all columns>`.
///
/// `rows` is a list of sequences aligned with `fields`; the `DEFAULT` marker becomes the
/// SQL `DEFAULT` keyword.
pub fn plan_insert(
    schema: &Schema,
    model: &str,
    fields: &[String],
    rows: &Bound<'_, PyList>,
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
                    SExpr::val(py_to_value(&item, Some(c.ty))?)
                });
            }
            if values.len() != cols.len() {
                return Err(query_err("insert row length does not match fields".into()));
            }
            stmt.values(values).map_err(|e| query_err(e.to_string()))?;
        }
    }
    stmt.returning(Query::returning().columns(m.fields().iter().map(|f| Alias::new(&f.column))));
    Ok((stmt, m.fields().iter().map(|f| f.ty).collect()))
}
