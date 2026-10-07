//! Host planning for prepared physical owner identities.
use orm_core::{schema::{Model, Schema}, ir::FieldIr};
use sea_query::{Alias, Expr, ExprTrait, JoinType, Query, SimpleExpr, SubQueryStatement};
use crate::{error::{query_err, Error, Result}};

/// A logical column read through its immutable storage owner. Shared-key ancestor
/// reads use ordinary bound SQL; there is no extension callback or hidden I/O.
pub fn column(schema: &Schema, model: &Model, alias: &str, field: &FieldIr) -> Result<SimpleExpr> {
    column_at(schema, model, alias, model.field_pos(&field.name).map_err(query_err)?)
}

/// `column` for the field at `position` of `model`.
pub fn column_at(schema: &Schema, model: &Model, alias: &str, position: usize) -> Result<SimpleExpr> {
    let resolved = &model.resolved_fields[position];
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
