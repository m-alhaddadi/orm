//! Prepare physical ownership once; planners consume numeric identities.
use crate::{behavior::{FieldId, FieldStorage, ModelId, OwnerId, OwnerLink, PreparedOwnerLink, ResolvedField, StorageId}, ir::ColType, schema::{Model, Schema}};
use std::collections::{BTreeMap, BTreeSet};

pub fn resolve(models: &mut [Model], physical: Option<&Schema>, mappings: &[FieldStorage], links: &[OwnerLink]) -> Result<Vec<PreparedOwnerLink>, String> {
    if physical.is_none() && (!mappings.is_empty() || !links.is_empty()) { return Err("storage ownership contributions require an explicit physical schema".into()); }
    let owners = physical.map(|s| s.models.as_slice()).unwrap_or(models);
    let named_owner = |name: &str| owners.iter().position(|m| m.ir.name == name).ok_or_else(|| format!("unknown physical owner {name}"));
    let mut prepared = vec![];
    let mut children = BTreeSet::new();
    for link in links {
        let child = named_owner(&link.child)?;
        let parent = named_owner(&link.parent)?;
        if child == parent || !children.insert(child) { return Err("each storage owner must have at most one distinct parent".into()); }
        let child_key = owners[child].field_pos(&link.child_key)?;
        let parent_key = owners[parent].field_pos(&link.parent_key)?;
        if child_key != owners[child].pk || parent_key != owners[parent].pk || owners[child].pk_field().value_type() != owners[parent].pk_field().value_type() {
            return Err("owner links require compatible shared primary keys".into());
        }
        if !owners[child].ir.relations.iter().any(|r| r.foreign_key && r.target == owners[parent].ir.name && r.from == link.child_key && r.to == link.parent_key) {
            return Err("owner links require a declared physical foreign key".into());
        }
        prepared.push(PreparedOwnerLink { child: OwnerId(child), parent: OwnerId(parent), child_key, parent_key });
    }
    for owner in 0..owners.len() {
        let mut visited = BTreeSet::new();
        let mut current = OwnerId(owner);
        while let Some(link) = prepared.iter().find(|l| l.child == current) {
            if !visited.insert(current.0) { return Err("cyclic physical owner links".into()); }
            current = link.parent;
        }
    }
    let mut by_field = BTreeMap::new();
    for mapping in mappings {
        let model = models.iter().find(|m| m.ir.name == mapping.model).ok_or_else(|| format!("unknown logical model {}", mapping.model))?;
        model.field(&mapping.field)?;
        if by_field.insert((mapping.model.as_str(), mapping.field.as_str()), mapping).is_some() { return Err(format!("{}.{}: duplicate storage ownership", mapping.model, mapping.field)); }
        named_owner(&mapping.owner)?;
    }
    let mut resolved = vec![];
    for (logical, model) in models.iter().enumerate() {
        let local = owners.iter().position(|o| o.table() == model.table()).ok_or_else(|| format!("{}: no physical owner for {}; declare storage explicitly", model.ir.name, model.table()))?;
        if model.pk_field().column != owners[local].pk_field().column { return Err(format!("{}: logical primary key differs from physical identity", model.ir.name)); }
        let mut fields = vec![];
        let mut writable = BTreeSet::new();
        for (position, field) in model.fields().iter().enumerate() {
            let source = model.native.dependency(position).map(|p| &model.fields()[p]).unwrap_or(field);
            let (owner, column) = match by_field.get(&(model.ir.name.as_str(), source.name.as_str())) {
                Some(mapping) => (named_owner(&mapping.owner)?, mapping.column.as_str()),
                None => (local, source.column.as_str()),
            };
            if owner == local && source.column != column { return Err("local storage mappings must agree with the logical column encoding".into()); }
            let column_pos = owners[owner].fields().iter().position(|f| f.column == column).ok_or_else(|| format!("{}.{}: no physical column {column}; contribute storage explicitly", model.ir.name, field.name))?;
            if !model.native.computed().contains(&position) && !writable.insert((owner, column_pos)) {
                return Err(format!("{}: duplicate writable ownership of a physical column", model.ir.name));
            }
            let stored = &owners[owner].fields()[column_pos];
            if (stored.ty != field.ty && !(matches!(stored.ty, ColType::String | ColType::Text) && matches!(field.ty, ColType::String | ColType::Text))) || stored.array != field.array || stored.db_type != field.db_type {
                return Err(format!("{}.{}: incompatible logical/physical encoding", model.ir.name, field.name));
            }
            let mut current = OwnerId(local);
            while current != OwnerId(owner) {
                current = prepared.iter().find(|l| l.child == current).ok_or_else(|| format!("{}.{}: storage owner is not an ancestor", model.ir.name, field.name))?.parent;
            }
            if position == model.pk && owner != local { return Err("logical identity must belong to its local storage owner".into()); }
            fields.push(ResolvedField { logical: FieldId { model: ModelId(logical), position }, storage: StorageId { owner: OwnerId(owner), column: column_pos }, logical_type: field.value_type(), physical_type: stored.value_type() });
        }
        // Relation links are local for this primitive. Explicit inherited relations
        // need a prepared relation-owner contribution rather than guessed columns.
        for rel in &model.ir.relations {
            if fields[model.field_pos(&rel.from)?].storage.owner != OwnerId(local) { return Err("inherited relation links require a relation-owner primitive".into()); }
        }
        resolved.push((OwnerId(local), fields));
    }
    for (model, (owner, fields)) in models.iter_mut().zip(resolved) { model.owner = owner; model.resolved_fields = fields; }
    Ok(prepared)
}
