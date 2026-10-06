//! Engine-managed writes for shared-primary-key model composition.
use crate::{
    db::{self, Executor, RowSet, Transaction},
    error::{query_err, Error, Result},
    exec::{ChainedRows, Outcome},
    params::NoParams,
    plan,
};
use orm_core::{
    behavior::{OwnerId, PreparedOwnerLink},
    dialect::Target,
    ir::{Delete, FieldIr, Select, Update, ValueType},
    schema::Schema,
};
use sea_query::{
    Alias, Expr, ExprTrait, JoinType, LockType, Query, SelectStatement, SimpleExpr, Value,
};

struct OwnerInsert {
    table: String,
    fields: Vec<FieldIr>,
    values: Vec<Option<Value>>,
    key: FieldIr,
}
/// Immutable bound insert input, owned independently of the defining registry.
pub struct Insert {
    rows: Vec<Vec<OwnerInsert>>,
    read: SelectStatement,
    table: String,
    key: FieldIr,
    model: usize,
    types: Vec<ValueType>,
}

pub fn is_composed(schema: &Schema, model: &str) -> Result<bool> {
    let m = schema.model(schema.model_idx(model).map_err(query_err)?);
    Ok(schema.owner_links.iter().any(|l| l.child == m.owner))
}

/// The owner links from `owner` up to the root, leaf first.
fn owner_chain(schema: &Schema, owner: OwnerId) -> Vec<&PreparedOwnerLink> {
    let mut chain = vec![];
    let mut current = owner;
    while let Some(link) = schema.owner_links.iter().find(|l| l.child == current) {
        chain.push(link);
        current = link.parent;
    }
    chain
}

/// An unfiltered select of the whole logical model.
fn model_select(model: &str) -> Result<Select> {
    serde_json::from_value(serde_json::json!({ "model": model })).map_err(|e| query_err(e.to_string()))
}

/// Ends `tx` by the result; a nested `tx` is a savepoint, so the outer scope stays open.
async fn finish<T>(tx: std::sync::Arc<dyn Transaction>, result: Result<T>) -> Result<T> {
    match result {
        Ok(out) => {
            tx.commit().await?;
            Ok(out)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

/// Complete-child creation. The physical identity is allocated only at the root.
pub fn prepare_insert(
    schema: &Schema,
    target: Target,
    model: &str,
    fields: &[String],
    rows: Vec<Vec<Option<Value>>>,
) -> Result<Insert> {
    let model_idx = schema.model_idx(model).map_err(query_err)?;
    let logical = schema.model(model_idx);
    let positions = fields
        .iter()
        .map(|f| logical.field_pos(f).map_err(query_err))
        .collect::<Result<Vec<_>>>()?;
    if positions
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != positions.len()
    {
        return Err(Error::query("duplicate insert field"));
    }
    if rows.iter().any(|r| r.len() != positions.len()) {
        return Err(Error::query("insert row length does not match fields"));
    }
    if positions
        .iter()
        .any(|p| logical.native.computed().contains(p))
    {
        return Err(Error::query("computed fields are read-only"));
    }
    let map = crate::behavior::input_map(logical.fields().len(), &positions)?;
    let mut rows = rows;
    crate::behavior::insert_values(logical.native, &map, &mut rows, fields.len())?;
    let mut chain = vec![logical.owner];
    chain.extend(owner_chain(schema, logical.owner).iter().map(|l| l.parent));
    chain.reverse();
    let owners = &schema.physical().models;
    let mut prepared = vec![];
    for row in rows {
        let mut steps = vec![];
        for (index, &owner) in chain.iter().enumerate() {
            let physical = &owners[owner.0];
            let mut columns = vec![];
            let mut values = vec![];
            for (&position, value) in positions.iter().zip(&row) {
                let resolved = &logical.resolved_fields[position];
                if position == logical.pk {
                    if index == 0 {
                        columns.push(physical.pk_field().clone());
                        values.push(value.clone());
                    }
                } else if resolved.storage.owner == owner {
                    columns.push(physical.fields()[resolved.storage.column].clone());
                    values.push(value.clone());
                }
            }
            steps.push(OwnerInsert {
                table: physical.table().into(),
                fields: columns,
                values,
                key: physical.pk_field().clone(),
            });
        }
        prepared.push(steps);
    }
    let read = plan::plan_select(schema, target, &model_select(model)?, &NoParams)?.stmt;
    Ok(Insert {
        rows: prepared,
        read,
        table: logical.table().into(),
        key: logical.pk_field().clone(),
        model: model_idx,
        types: logical.fields().iter().map(|f| f.value_type()).collect(),
    })
}

pub async fn run_insert(conn: &dyn Executor, target: Target, insert: Insert) -> Result<Outcome> {
    let tx = conn.begin().await?;
    let result = async {
        let mut fetched = vec![];
        for row in insert.rows {
            let mut identity = None;
            for step in row {
                let mut columns = vec![];
                let mut values = vec![];
                for (field, value) in step.fields.iter().zip(step.values) {
                    // Omitting a column uses its actual database default on both backends.
                    if let Some(value) = value {
                        columns.push(Alias::new(&field.column));
                        values.push(plan::bind(value, Some(field)));
                    }
                }
                if let Some(value) = identity.take() {
                    columns.push(Alias::new(&step.key.column));
                    values.push(plan::bind(value, Some(&step.key)));
                }
                let mut statement = Query::insert();
                statement.into_table(Alias::new(&step.table));
                if columns.is_empty() {
                    statement.or_default_values();
                } else {
                    statement
                        .columns(columns)
                        .values(values)
                        .map_err(|e| query_err(e.to_string()))?;
                }
                statement.returning(Query::returning().expr(plan::returning_col(&step.key)));
                let (sql, args) = db::build(target.dialect, &statement);
                let returned = tx.query(sql, args).await?;
                if returned.len() != 1 {
                    return Err(Error::query("composed insert must return one identity"));
                }
                identity = Some(returned.value(0, 0, step.key.value_type())?);
            }
            let mut read = insert.read.clone();
            read.and_where(
                Expr::col((Alias::new(&insert.table), Alias::new(&insert.key.column)))
                    .eq(plan::bind(identity.unwrap(), Some(&insert.key))),
            );
            let (sql, args) = db::build(target.dialect, &read);
            let returned = tx.query(sql, args).await?;
            if returned.len() != 1 {
                return Err(Error::query(
                    "composed insert could not read its stored row",
                ));
            }
            fetched.push(returned);
        }
        Ok(Outcome::Rows {
            model: insert.model,
            types: insert.types,
            rows: Box::new(ChainedRows::new(fetched)) as Box<dyn RowSet>,
            shape: None,
        })
    }
    .await;
    finish(tx, result).await
}

struct Assignment {
    table: String,
    key: FieldIr,
    field: FieldIr,
    position: usize,
}
/// One captured identity set governs every table mutation.
pub struct Mutation {
    matched: SelectStatement,
    read: SelectStatement,
    table: String,
    key: FieldIr,
    key_position: usize,
    model: usize,
    types: Vec<ValueType>,
    assignments: Vec<Assignment>,
    returning: bool,
    delete: bool,
}

#[allow(clippy::too_many_arguments)]
fn mutation(
    schema: &Schema,
    target: Target,
    model: &str,
    filters: Vec<orm_core::ir::Expr>,
    with: Vec<orm_core::ir::Cte>,
    params: &dyn crate::Params,
    returning: bool,
    delete: bool,
    prepared_match: Option<SelectStatement>,
) -> Result<Mutation> {
    let model_idx = schema.model_idx(model).map_err(query_err)?;
    let logical = schema.model(model_idx);
    let mut query = model_select(model)?;
    let read = plan::plan_select(schema, target, &query, &NoParams)?.stmt;
    query.filters = filters;
    query.with = with;
    let mut matched = match prepared_match {
        Some(statement) => statement,
        None => plan::plan_select(schema, target, &query, params)?.stmt,
    };
    // Lock the whole owner chain on PostgreSQL. SQLite's read transaction pins
    // the matched snapshot; an incompatible concurrent writer causes rollback.
    if target.dialect == orm_core::dialect::Dialect::Postgres {
        let owners = &schema.physical().models;
        let mut alias = logical.table().to_owned();
        let mut locks = vec![Alias::new(&alias)];
        for link in owner_chain(schema, logical.owner) {
            let parent = &owners[link.parent.0];
            let next_alias = format!("__write_owner_{}", link.parent.0);
            matched.join_as(
                JoinType::InnerJoin,
                Alias::new(parent.table()),
                Alias::new(&next_alias),
                Expr::col((
                    Alias::new(&next_alias),
                    Alias::new(&parent.fields()[link.parent_key].column),
                ))
                .eq(Expr::col((
                    Alias::new(&alias),
                    Alias::new(&owners[link.child.0].fields()[link.child_key].column),
                ))),
            );
            locks.push(Alias::new(&next_alias));
            alias = next_alias;
        }
        matched.lock_with_tables(LockType::Update, locks);
    }
    Ok(Mutation {
        matched,
        read,
        table: logical.table().into(),
        key: logical.pk_field().clone(),
        key_position: logical.pk,
        model: model_idx,
        types: logical.fields().iter().map(|f| f.value_type()).collect(),
        assignments: vec![],
        returning,
        delete,
    })
}

pub fn prepare_delete(
    schema: &Schema,
    target: Target,
    query: &Delete,
    params: &dyn crate::Params,
) -> Result<Mutation> {
    mutation(
        schema,
        target,
        &query.model,
        query.filters.clone(),
        query.with.clone(),
        params,
        query.returning,
        true,
        None,
    )
}

pub fn prepare_update(
    schema: &Schema,
    target: Target,
    query: &Update,
    params: &dyn crate::Params,
    expressions: Vec<SimpleExpr>,
    matched: SelectStatement,
) -> Result<Mutation> {
    let mut plan = mutation(
        schema,
        target,
        &query.model,
        query.filters.clone(),
        query.with.clone(),
        params,
        query.returning,
        false,
        Some(matched),
    )?;
    let logical = schema.model(plan.model);
    let owners = &schema.physical().models;
    for (offset, (assignment, expression)) in query.set.iter().zip(expressions).enumerate() {
        let position = logical.field_pos(&assignment.field).map_err(query_err)?;
        if position == logical.pk {
            return Err(Error::query(
                "composed update cannot change the shared primary key",
            ));
        }
        let storage = logical.resolved_fields[position].storage;
        let owner = &owners[storage.owner.0];
        let field = owner.fields()[storage.column].clone();
        let expression = match &field.read_sql {
            Some(t) => SimpleExpr::cust_with_expr(t.replace("{}", "$1"), expression),
            None => expression,
        };
        plan.matched.expr(expression);
        plan.assignments.push(Assignment {
            table: owner.table().into(),
            key: owner.pk_field().clone(),
            field,
            position: logical.fields().len() + offset,
        });
    }
    Ok(plan)
}

pub async fn run_mutation(conn: &dyn Executor, target: Target, plan: Mutation) -> Result<Outcome> {
    let tx = conn.begin().await?;
    let result = async {
        let (sql, args) = db::build(target.dialect, &plan.matched);
        let matched = tx.query(sql, args).await?;
        let mut keys = vec![];
        for row in 0..matched.len() {
            keys.push(matched.value(row, plan.key_position, plan.key.value_type())?);
        }
        for (row, key) in keys.iter().enumerate() {
            if plan.delete {
                let mut delete = Query::delete();
                delete.from_table(Alias::new(&plan.table)).and_where(
                    Expr::col(Alias::new(&plan.key.column))
                        .eq(plan::bind(key.clone(), Some(&plan.key))),
                );
                let (sql, args) = db::build(target.dialect, &delete);
                if tx.execute(sql, args).await? != 1 {
                    return Err(Error::query("captured composed identity disappeared"));
                }
            } else {
                let mut writes = std::collections::BTreeMap::new();
                for assignment in &plan.assignments {
                    let value =
                        matched.value(row, assignment.position, assignment.field.value_type())?;
                    let statement = writes.entry(assignment.table.clone()).or_insert_with(|| {
                        let mut update = Query::update();
                        update.table(Alias::new(&assignment.table)).and_where(
                            Expr::col(Alias::new(&assignment.key.column))
                                .eq(plan::bind(key.clone(), Some(&assignment.key))),
                        );
                        update
                    });
                    statement.value(
                        Alias::new(&assignment.field.column),
                        plan::bind(value, Some(&assignment.field)),
                    );
                }
                for statement in writes.into_values() {
                    let (sql, args) = db::build(target.dialect, &statement);
                    if tx.execute(sql, args).await? != 1 {
                        return Err(Error::query("captured composed owner disappeared"));
                    }
                }
            }
        }
        if !plan.returning {
            return Ok(Outcome::Affected(keys.len() as u64));
        }
        if plan.delete {
            return Ok(Outcome::Rows {
                model: plan.model,
                types: plan.types,
                rows: matched,
                shape: None,
            });
        }
        let mut returned = vec![];
        for key in keys {
            let mut read = plan.read.clone();
            read.and_where(
                Expr::col((Alias::new(&plan.table), Alias::new(&plan.key.column)))
                    .eq(plan::bind(key, Some(&plan.key))),
            );
            let (sql, args) = db::build(target.dialect, &read);
            let row = tx.query(sql, args).await?;
            if row.len() != 1 {
                return Err(Error::query("updated composed identity disappeared"));
            }
            returned.push(row);
        }
        Ok(Outcome::Rows {
            model: plan.model,
            types: plan.types,
            rows: Box::new(ChainedRows::new(returned)),
            shape: None,
        })
    }
    .await;
    finish(tx, result).await
}

/// Attach only local columns to a physically existing immediate parent. The FK
/// checks existence and races; no ancestor insert or update is executed.
pub fn prepare_attach(
    schema: &Schema,
    target: Target,
    model: &str,
    identity: Value,
    fields: &[String],
    rows: Vec<Vec<Option<Value>>>,
) -> Result<Insert> {
    if !is_composed(schema, model)? {
        return Err(Error::query("attach requires a composed child model"));
    }
    let logical = schema.model(schema.model_idx(model).map_err(query_err)?);
    for field in fields {
        let position = logical.field_pos(field).map_err(query_err)?;
        if position == logical.pk
            || logical.resolved_fields[position].storage.owner != logical.owner
        {
            return Err(Error::query(
                "attach accepts only local child values; parent values cannot be overwritten",
            ));
        }
    }
    let mut fields = fields.to_vec();
    fields.push(logical.pk_field().name.clone());
    let rows = rows
        .into_iter()
        .map(|mut row| {
            row.push(Some(identity.clone()));
            row
        })
        .collect();
    let mut insert = prepare_insert(schema, target, model, &fields, rows)?;
    for row in &mut insert.rows {
        let mut leaf = row.pop().expect("prepared composed leaf");
        // Complete creates place a supplied shared key at the root. Attach places
        // that same validated value directly in the sole child statement.
        let root = row.first().expect("composed ancestor");
        let value = root
            .fields
            .iter()
            .position(|f| f.primary_key)
            .and_then(|p| root.values[p].clone())
            .ok_or_else(|| Error::query("attach requires an explicit parent identity"))?;
        leaf.fields.push(leaf.key.clone());
        leaf.values.push(Some(value));
        *row = vec![leaf];
    }
    Ok(insert)
}
