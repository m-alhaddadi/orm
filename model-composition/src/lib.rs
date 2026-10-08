//! Optional shared-primary-key schema lowering. The engine `composed` module executes writes.
use orm_contracts::{
    extension::{capture_storage, FieldStorage, OwnerLink},
    ir::*,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
struct Composition {
    child: String,
    parent: String,
    parent_ref: String,
    child_ref: String,
}

/// Work on a candidate so a rejected declaration cannot partially mutate storage.
pub fn lower(ir: &mut SchemaIr) -> Result<(), String> {
    let mut candidate: SchemaIr =
        serde_json::from_value(serde_json::to_value(&*ir).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    lower_candidate(&mut candidate)?;
    *ir = candidate;
    Ok(())
}

fn lower_candidate(ir: &mut SchemaIr) -> Result<(), String> {
    let mut pending = BTreeMap::new();
    for d in ir
        .behavior
        .declarations
        .iter()
        .filter(|d| !d.lowered && d.attribute == "composition.model")
    {
        let arg = |key: &str| {
            d.arguments
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("{}: composition requires {key}", d.model))
        };
        if d.field.is_some()
            || !d.positional.is_empty()
            || d.arguments
                .keys()
                .any(|k| !["parent", "parentRef", "childRef"].contains(&k.as_str()))
        {
            return Err(
                "composition.model takes only parent, parentRef and childRef on a model".into(),
            );
        }
        let c = Composition {
            child: d.model.clone(),
            parent: arg("parent")?,
            parent_ref: arg("parentRef")?,
            child_ref: arg("childRef")?,
        };
        if pending.insert(c.child.clone(), c).is_some() {
            return Err(format!("{}: multiple composed parents", d.model));
        }
    }
    if pending.is_empty() {
        return Ok(());
    }
    capture_storage(ir)?;
    let names: BTreeSet<_> = ir.models.iter().map(|m| m.name.clone()).collect();
    for c in pending.values() {
        if !names.contains(&c.child) || !names.contains(&c.parent) {
            return Err(format!(
                "{}: unknown composition parent {}",
                c.child, c.parent
            ));
        }
        if ir.behavior.owner_links.iter().any(|l| l.child == c.child) {
            return Err(format!("{}: storage parent already assigned", c.child));
        }
    }
    // Roots first, independent of declaration order, with explicit cycle rejection.
    while !pending.is_empty() {
        let next = pending
            .values()
            .find(|c| !pending.contains_key(&c.parent))
            .cloned()
            .ok_or("cyclic model composition")?;
        pending.remove(&next.child);
        lower_one(ir, &next)?;
    }
    Ok(())
}

fn relation(name: &str, target: &str, key: &str, foreign_key: bool) -> RelationIr {
    RelationIr {
        name: name.into(),
        kind: RelKind::One,
        target: target.into(),
        from: key.into(),
        to: key.into(),
        foreign_key,
        on_delete: foreign_key.then_some(OnDelete::Cascade),
        on_update: None,
        deferrable: None,
        fk_name: None,
        through: None,
    }
}
fn check_member(model: &ModelIr, name: &str) -> Result<(), String> {
    if model.fields.iter().any(|f| f.name == name) || model.relations.iter().any(|r| r.name == name)
    {
        return Err(format!(
            "{}.{}: generated relation name collides",
            model.name, name
        ));
    }
    if !name
        .chars()
        .enumerate()
        .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || i > 0 && c.is_ascii_digit())
    {
        return Err(format!("invalid relation name {name}"));
    }
    Ok(())
}
fn lower_one(ir: &mut SchemaIr, c: &Composition) -> Result<(), String> {
    let index = |name: &str| {
        ir.models
            .iter()
            .position(|m| m.name == name)
            .ok_or_else(|| format!("{name}: unknown composition model"))
    };
    let parent_idx = index(&c.parent)?;
    let child_idx = index(&c.child)?;
    let parent = &ir.models[parent_idx];
    let child = &ir.models[child_idx];
    if parent.table == child.table {
        return Err(format!("{}: composition needs a separate table", c.child));
    }
    check_member(parent, &c.child_ref)?;
    check_member(child, &c.parent_ref)?;
    let keys: Vec<_> = parent.fields.iter().filter(|f| f.primary_key).collect();
    if keys.len() != 1 {
        return Err(format!(
            "{}: composition requires one parent primary key",
            c.parent
        ));
    }
    let root_key = keys[0].clone();
    if root_key.nullable || root_key.array {
        return Err("composition primary key must be a required scalar".into());
    }
    let mut key = root_key.clone();
    key.auto_increment = false;
    key.default = None;
    key.default_now = false;
    key.default_sql = None;
    key.unique = false;
    key.index = false;
    key.renamed_from = None;
    for hint in [
        "composition.child",
        "composition.local",
        "composition.key-default",
    ] {
        key.hints.remove(hint);
    }
    if child
        .fields
        .iter()
        .any(|f| !f.primary_key && (f.name == key.name || f.column == key.column))
    {
        return Err(format!(
            "{}: local field collides with shared primary key",
            c.child
        ));
    }
    let child_keys: Vec<_> = child.fields.iter().filter(|f| f.primary_key).collect();
    if child_keys.len() > 1 {
        return Err("composition requires one child primary key".into());
    }
    if let Some(declared) = child_keys.first() {
        if declared.name != key.name
            || declared.column != key.column
            || declared.value_type() != key.value_type()
            || declared.db_type != key.db_type
            || declared.enum_name != key.enum_name
            || declared.max_length != key.max_length
            || declared.nullable
            || declared.auto_increment
            || declared.default.is_some()
            || declared.default_sql.is_some()
            || declared.default_now
        {
            return Err(format!(
                "{}: child primary key must match parent without an independent default",
                c.child
            ));
        }
    }
    let mut inherited = vec![];
    for f in &parent.fields {
        if f.primary_key {
            continue;
        }
        if child
            .fields
            .iter()
            .any(|local| local.name == f.name || local.column == f.column)
            || child.relations.iter().any(|r| r.name == f.name)
        {
            return Err(format!(
                "{}.{}: inherited field override or ambiguity",
                c.child, f.name
            ));
        }
        if f.name == c.parent_ref {
            return Err(format!(
                "{}.{}: inherited field collides with parent reference",
                c.child, f.name
            ));
        }
        let owner = ir
            .behavior
            .field_storage
            .iter()
            .find(|m| m.model == c.parent && m.field == f.name);
        ir.behavior.field_storage.push(FieldStorage {
            model: c.child.clone(),
            field: f.name.clone(),
            owner: owner.map(|m| m.owner.clone()).unwrap_or(c.parent.clone()),
            column: owner.map(|m| m.column.clone()).unwrap_or(f.column.clone()),
        });
        let mut inherited_field = f.clone();
        inherited_field.hints.remove("composition.local");
        inherited.push(inherited_field);
    }
    let child = &mut ir.models[child_idx];
    child.fields.retain(|f| !f.primary_key);
    for field in &mut child.fields {
        field
            .hints
            .insert("composition.local".into(), "true".into());
    }
    let mut logical_key = key.clone();
    logical_key
        .hints
        .insert("composition.child".into(), "true".into());
    if root_key.auto_increment
        || root_key.default.is_some()
        || root_key.default_now
        || root_key.default_sql.is_some()
        || root_key
            .hints
            .get("composition.key-default")
            .is_some_and(|v| v == "true")
    {
        logical_key
            .hints
            .insert("composition.key-default".into(), "true".into());
    }
    child.fields.insert(0, logical_key);
    child.fields.extend(inherited);
    child
        .relations
        .push(relation(&c.parent_ref, &c.parent, &key.name, true));
    ir.models[parent_idx]
        .relations
        .push(relation(&c.child_ref, &c.child, &key.name, false));
    let physical = &mut ir
        .behavior
        .storage
        .as_mut()
        .ok_or("composition requires captured physical storage")?
        .models;
    let parent = physical
        .iter_mut()
        .find(|m| m.name == c.parent)
        .ok_or("composition parent has no physical owner")?;
    parent
        .relations
        .push(relation(&c.child_ref, &c.child, &key.name, false));
    let child = physical
        .iter_mut()
        .find(|m| m.name == c.child)
        .ok_or("composition child has no physical owner")?;
    child.fields.retain(|f| !f.primary_key);
    child.fields.insert(0, key.clone());
    child
        .relations
        .push(relation(&c.parent_ref, &c.parent, &key.name, true));
    ir.behavior.owner_links.push(OwnerLink {
        child: c.child.clone(),
        parent: c.parent.clone(),
        child_key: key.name.clone(),
        parent_key: key.name,
    });
    Ok(())
}
