//! Host planning for prepared physical owner identities.
use orm_core::{schema::{Model, Schema}, ir::FieldIr};
use sea_query::{Alias, Expr, ExprTrait, JoinType, Query, SimpleExpr, SubQueryStatement};
use crate::{error::{query_err, Error, Result}};

/// A logical column read through its immutable storage owner. Shared-key ancestor
/// reads use ordinary bound SQL; there is no extension callback or hidden I/O.
pub fn column(schema: &Schema, model: &Model, alias: &str, field: &FieldIr) -> Result<SimpleExpr> {
    let resolved = &model.resolved_fields[model.field_pos(&field.name).map_err(query_err)?];
    let owners = &schema.physical().models;
    if resolved.storage.owner == model.owner {
        let column = &owners[resolved.storage.owner.0].fields()[resolved.storage.column].column;
        return Ok(Expr::col((Alias::new(alias), Alias::new(column))).into());
    }
    let mut current = model.owner;
    let mut current_alias = alias.to_owned();
    let mut query = Query::select();
    let mut first = true;
    while current != resolved.storage.owner {
        let link = schema.owner_links.iter().find(|link| link.child == current).ok_or_else(|| Error::query("missing prepared owner link"))?;
        let parent = &owners[link.parent.0];
        let parent_alias = format!("{alias}_owner_{}", link.parent.0);
        let on = Expr::col((Alias::new(&parent_alias), Alias::new(&parent.fields()[link.parent_key].column)))
            .eq(Expr::col((Alias::new(&current_alias), Alias::new(&owners[current.0].fields()[link.child_key].column))));
        if first { query.from_as(Alias::new(parent.table()), Alias::new(&parent_alias)).and_where(on); }
        else { query.join_as(JoinType::InnerJoin, Alias::new(parent.table()), Alias::new(&parent_alias), on); }
        first = false;
        current = link.parent;
        current_alias = parent_alias;
    }
    query.expr(Expr::col((Alias::new(current_alias), Alias::new(&owners[current.0].fields()[resolved.storage.column].column))));
    Ok(SimpleExpr::SubQuery(None, Box::new(SubQueryStatement::SelectStatement(query))))
}

pub fn require_local_write(model: &Model) -> Result<()> {
    if model.resolved_fields.iter().any(|f| f.storage.owner != model.owner) {
        return Err(Error::query("multi-owner model writes require a prepared owner statement sequence; use the storage-composition API"));
    }
    Ok(())
}

use orm_core::behavior::{ModelId, OwnerId, Supplied, WriteContract, WriteMode, WriteValue};
use crate::{db::{Cell, DbResult, Executor, RowSet}, params::{null_of, NoParams}, exec::{self, Outcome}};
use orm_core::{dialect::Target, ir::ValueType};
use sea_query::Value;

struct OwnerStep {
    owner: OwnerId,
    model: String,
    fields: Vec<String>,
    values: Vec<Option<WriteValue<Value>>>,
}
/// Prepared host statement sequence. Extension policy supplies typed owners and
/// key-copy references; the host owns binding, execution and transaction scope.
pub struct PreparedWrite {
    steps: Vec<OwnerStep>,
    model: ModelId,
    returning: bool,
}

pub fn prepare_write(schema: &Schema, contract: &WriteContract<'_, WriteValue<Value>>) -> Result<PreparedWrite> {
    if std::ptr::eq(schema, schema.physical()) { return Err(Error::query("owner statement sequences require an explicit physical schema")); }
    if contract.mode != WriteMode::Insert { return Err(Error::query("this owner statement primitive supports insert; other modes require an explicit host strategy")); }
    let logical = schema.models.get(contract.model.0).ok_or_else(|| Error::query("unknown logical write model"))?;
    let owners = &schema.physical().models;
    let mut steps: Vec<OwnerStep> = vec![];
    let mut seen = std::collections::BTreeSet::new();
    let mut supplied_positions = vec![];
    let mut supplied_values = vec![];
    for (index, write) in contract.owners.iter().enumerate() {
        let owner = owners.get(write.owner.0).ok_or_else(|| Error::query("unknown write owner"))?;
        let mut ancestor = logical.owner;
        while ancestor != write.owner {
            ancestor = schema.owner_links.iter().find(|link| link.child == ancestor)
                .ok_or_else(|| Error::query("write owner is not an ancestor of the logical model"))?.parent;
        }
        if !seen.insert(write.owner.0) || write.fields.len() != write.values.len() { return Err(Error::query("duplicate owner or invalid owner value shape")); }
        let mut fields = vec![];
        let mut values = vec![];
        let mut columns = std::collections::BTreeSet::new();
        for (&field, value) in write.fields.iter().zip(write.values) {
            if field.owner != write.owner || !columns.insert(field.column) { return Err(Error::query("invalid or duplicate owner column")); }
            let physical = owner.fields().get(field.column).ok_or_else(|| Error::query("unknown owner column"))?;
            // Shared identity belongs to the child logically, but ancestor inserts
            // may supply that same identity explicitly before propagation.
            let position = logical.resolved_fields.iter().position(|f| f.storage == field)
                .or_else(|| (field.column == owner.pk).then_some(logical.pk))
                .ok_or_else(|| Error::query("owner write column is not exposed by the logical model"))?;
            if logical.native.computed().contains(&position) { return Err(Error::query("computed fields are read-only")); }
            let mut value = match value {
                Supplied::Omitted => { crate::behavior::omitted(logical.native, position)?; None },
                Supplied::Null => Some(WriteValue::Value(null_of(Some(physical.value_type())))),
                Supplied::Value(value) => Some(value.clone()),
            };
            match &mut value {
                Some(WriteValue::Value(value)) => {
                    let expected = null_of(Some(physical.value_type()));
                    if std::mem::discriminant(value) != std::mem::discriminant(&expected) || matches!((&*value, &expected), (Value::Array(a, _), Value::Array(b, _)) if a != b) {
                        return Err(Error::query("owner value does not match its prepared physical type"));
                    }
                    #[cfg(feature = "file-storage")]
                    crate::file_storage::value(logical, position, value)?;
                    crate::behavior::field(logical.native, position, value)?;
                    supplied_positions.push(position); supplied_values.push(value.clone());
                }
                Some(WriteValue::Returned { step, column }) => {
                    if *step >= index || steps[*step].owner != column.owner { return Err(Error::query("key-copy references must point to an earlier owner")); }
                    let source = owners[column.owner.0].fields().get(column.column).ok_or_else(|| Error::query("unknown returned key column"))?;
                    if source.value_type() != physical.value_type() || column.column != owners[column.owner.0].pk || field.column != owner.pk { return Err(Error::query("key propagation requires compatible shared primary keys")); }
                    let link = schema.owner_links.iter().find(|l| l.child == write.owner).ok_or_else(|| Error::query("key propagation needs a prepared owner link"))?;
                    if link.parent != column.owner { return Err(Error::query("key-copy source must be the declared parent")); }
                    crate::behavior::expression(logical.native, position)?;
                }
                None => {}
            }
            fields.push(physical.name.clone()); values.push(value);
        }
        if let Some(link) = schema.owner_links.iter().find(|l| l.child == write.owner) {
            let key = write.fields.iter().position(|f| f.column == link.child_key).and_then(|p| values[p].as_ref());
            if !matches!(key, Some(WriteValue::Returned { column, .. }) if column.owner == link.parent && column.column == link.parent_key) {
                return Err(Error::query("a composed insert must copy its parent's returned identity"));
            }
        }
        steps.push(OwnerStep { owner: write.owner, model: owner.ir.name.clone(), fields, values });
    }
    if !seen.contains(&logical.owner.0) { return Err(Error::query("write sequence omits the logical model's local owner")); }
    for field in contract.validation_dependencies {
        if field.model != contract.model || !supplied_positions.contains(&field.position) { return Err(Error::query("missing supplied validation dependency")); }
    }
    let map = crate::behavior::input_map(logical.fields().len(), &supplied_positions)?;
    crate::behavior::insert_fields(logical.native, &map)?;
    crate::behavior::record(logical.native, &map, supplied_values.as_slice())?;
    if let Some(shape) = contract.returning {
        if shape.model != contract.model || shape.fields.len() != logical.fields().len() || shape.fields.iter().enumerate().any(|(pos,f)| f.field.model != contract.model || f.field.position != pos || !f.public) {
            return Err(Error::query("owner writes currently return the complete logical model shape"));
        }
        for field in &logical.resolved_fields { if !seen.contains(&field.storage.owner.0) { return Err(Error::query("returning shape references an unwritten owner")); } }
    }
    Ok(PreparedWrite { steps, model: contract.model, returning: contract.returning.is_some() })
}

struct OwnerRows {
    rows: Vec<Box<dyn RowSet>>,
    fields: Vec<(usize, usize, ValueType)>,
}
impl RowSet for OwnerRows {
    fn len(&self) -> usize { 1 }
    fn cell(&self, row: usize, col: usize, _: ValueType) -> DbResult<Cell<'_>> { let (step, column, ty) = self.fields[col]; self.rows[step].cell(row, column, ty) }
    fn value(&self, row: usize, col: usize, _: ValueType) -> DbResult<Value> { let (step, column, ty) = self.fields[col]; self.rows[step].value(row, column, ty) }
    fn get_i64(&self, row: usize, col: usize) -> DbResult<i64> { let (step,column,_) = self.fields[col]; self.rows[step].get_i64(row,column) }
    fn get_bool(&self, row: usize, col: usize) -> DbResult<bool> { let (step,column,_) = self.fields[col]; self.rows[step].get_bool(row,column) }
}

/// Own one transaction or savepoint; errors roll back this scope, including an
/// earlier parent insert. Returned identities never cross a language callback.
pub async fn run_write(conn: &dyn Executor, schema: &Schema, target: Target, plan: PreparedWrite) -> Result<Outcome> {
    let tx = conn.begin().await?;
    let result = async {
        let mut returned: Vec<Box<dyn RowSet>> = vec![];
        for step in &plan.steps {
            let mut values = vec![];
            for value in &step.values {
                values.push(match value {
                    None => None,
                    Some(WriteValue::Value(value)) => Some(value.clone()),
                    Some(WriteValue::Returned { step, column }) => Some(returned[*step].value(0, column.column, schema.physical().models[column.owner.0].fields()[column.column].value_type())?),
                });
            }
            let statement = exec::plan_insert(schema.physical(), target, &step.model, &step.fields, vec![values], None, &NoParams)?;
            let Outcome::Rows { rows, .. } = exec::run(tx.as_ref(), target, statement).await? else { return Err(Error::query("owner insert must return its identity")); };
            if rows.len() != 1 { return Err(Error::query("owner insert must return exactly one row")); }
            returned.push(rows);
        }
        if !plan.returning { return Ok(Outcome::Affected(1)); }
        let logical = schema.model(plan.model.0);
        let fields = logical.resolved_fields.iter().map(|field| {
            let step = plan.steps.iter().position(|s| s.owner == field.storage.owner).expect("prepared returning owner");
            (step, field.storage.column, field.physical_type)
        }).collect();
        Ok(Outcome::Rows { model: plan.model.0, rows: Box::new(OwnerRows { rows: returned, fields }), types: logical.fields().iter().map(|f| f.value_type()).collect(), shape: None })
    }.await;
    match result {
        Ok(out) => { tx.commit().await?; Ok(out) },
        Err(error) => { tx.rollback().await?; Err(error) },
    }
}
