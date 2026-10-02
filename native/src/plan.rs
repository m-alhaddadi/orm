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
    ArithOp, Assignment, CmpOp, ColType, Delete, Expr, FieldIr, Lock, Operation, RelKind, Select, Update,
};
use orm_core::dialect::{Capabilities, Target};
use orm_core::schema::Schema;

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
    /// Column types of the returned rows when the update has `RETURNING`.
    Update(UpdateStatement, Option<Vec<ColType>>),
    /// Column types of the returned rows when the delete has `RETURNING`.
    Delete(DeleteStatement, Option<Vec<ColType>>),
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

/// Collects every column path referenced by `e`; sets `has_not` if `e` contains a `NOT`.
fn col_paths<'e>(e: &'e Expr, out: &mut Vec<&'e [String]>, has_not: &mut bool) {
    match e {
        Expr::Col { path, .. } => out.push(path),
        Expr::Param { .. } | Expr::Const { .. } | Expr::Excluded { .. } => {}
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
    /// `EXCLUDED.<field>` is only valid in an upsert's `DO UPDATE SET`.
    allow_excluded: bool,
    target: Target,
    caps: Capabilities,
}

impl<'s, 'py> Planner<'s, 'py> {
    pub fn new(
        schema: &'s Schema,
        target: Target,
        model: &str,
        params: &'s [Bound<'py, PyAny>],
    ) -> PyResult<Self> {
        let root = schema.model_idx(model).map_err(query_err)?;
        let scope = Scope { path: vec![], model: root, alias: schema.model(root).table().to_owned() };
        Ok(Planner {
            schema,
            params,
            root,
            scopes: vec![scope],
            joins: vec![],
            next_alias: 0,
            allow_excluded: false,
            target,
            caps: target.caps,
        })
    }

    pub fn plan(
        schema: &'s Schema,
        target: Target,
        op: &Operation,
        params: &'s [Bound<'py, PyAny>],
    ) -> PyResult<Plan> {
        let new = |model: &str| Planner::new(schema, target, model, params);
        Ok(match op {
            Operation::Select(q) => Plan::Select(new(&q.model)?.select(q)?),
            Operation::Count(q) => Plan::Count(new(&q.model)?.count(q)?),
            Operation::Exists(q) => Plan::Exists(new(&q.model)?.exists(q)?),
            Operation::Update(q) => {
                let (stmt, types) = new(&q.model)?.update(q)?;
                Plan::Update(stmt, types)
            }
            Operation::Delete(q) => {
                let (stmt, types) = new(&q.model)?.delete(q)?;
                Plan::Delete(stmt, types)
            }
        })
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

    fn hint_of(&self, e: &Expr) -> Hint<'s> {
        match e {
            Expr::Col { path, name } => {
                let schema = self.schema;
                let field = schema.walk(self.root, path).ok().and_then(|m| schema.model(m).field(name).ok());
                Hint { ty: field.map(|f| f.ty), field }
            }
            Expr::Excluded { name } => {
                let field = self.schema.model(self.root).field(name).ok();
                Hint { ty: field.map(|f| f.ty), field }
            }
            // Arithmetic results are plain values: no write_sql cast.
            Expr::Arith { l, r, .. } => Hint { ty: self.hint_of(l).or(self.hint_of(r)).ty, field: None },
            _ => Hint::default(),
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

    fn value(&mut self, e: &Expr, hint: Hint<'s>) -> PyResult<SExpr> {
        Ok(match e {
            Expr::Col { path, name } => self.resolve(path, name)?,
            Expr::Param { i } => bind(py_to_value(self.param(*i)?, hint.ty)?, hint.field),
            Expr::Const { value } => SExpr::val(*value),
            Expr::Excluded { name } => {
                if !self.allow_excluded {
                    return Err(query_err("excluded() can only be used in on_conflict(...).do_update()".into()));
                }
                let f = self.schema.model(self.root).field(name).map_err(query_err)?;
                col("excluded", &f.column)
            }
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
            let e = self.value(&o.expr, Hint::default())?;
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
            stmt.expr(read_col(root.table(), f));
            types.push(f.ty);
        }
        for path in &q.select_related {
            let (alias, model) = self.ensure_join(path, "select_related")?;
            for f in schema.model(model).fields() {
                stmt.expr(read_col(&alias, f));
                types.push(f.ty);
            }
        }
        self.base_select(q, &mut stmt, true)?;
        self.apply_joins(&mut stmt);
        if let Some(lock) = q.lock {
            let c = self.caps;
            if lock.exclusive {
                self.require(c.lock_exclusive, "lock() (SELECT ... FOR UPDATE)")?;
            } else {
                self.require(c.lock_shared, "lock(exclusive=False) (SELECT ... FOR SHARE)")?;
            }
            self.require(c.lock_nowait || !lock.nowait, "lock(nowait=True)")?;
            self.require(c.lock_skip_locked || !lock.skip_locked, "lock(skip_locked=True)")?;
            self.require(c.lock_of || self.joins.is_empty(), "lock() together with select_related")?;
            apply_lock(&mut stmt, lock, c.lock_of.then(|| root.table()));
        }

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
                sub.expr(read_col(tm.table(), f));
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

    pub fn exists(mut self, q: &Select) -> PyResult<SelectStatement> {
        no_lock(q, "exists")?;
        let mut inner = self.sliced_inner(q)?;
        if q.limit.is_none() {
            inner.limit(1);
        }
        let mut stmt = Query::select();
        stmt.expr(SExpr::exists(inner));
        Ok(stmt)
    }

    pub fn update(mut self, q: &Update) -> PyResult<(UpdateStatement, Option<Vec<ColType>>)> {
        let root = self.schema.model(self.root);
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

    pub fn delete(mut self, q: &Delete) -> PyResult<(DeleteStatement, Option<Vec<ColType>>)> {
        let root = self.schema.model(self.root);
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
                let mut planner = Planner::new(schema, target, model, params)?;
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
    let mut planner = Planner::new(schema, target, model, params)?;
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
