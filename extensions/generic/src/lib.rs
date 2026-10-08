//! Compiler contribution for type/key references. Execution requires the native
//! generic-relations host capability; this crate alone never promises loading.
use orm_contracts::{extension::Declaration, generic::{GenericRelation, GenericReverse}, ir::{ColType, ConstraintIr, FieldIr, IndexIr, SchemaIr}};

fn error(d: &Declaration, message: impl std::fmt::Display) -> String {
    let at = if d.field.is_some() { "@" } else { "@@" };
    format!("{}:{}:{}: {at}{}: {message}", d.location.file, d.location.line, d.location.column, d.attribute)
}
fn string(d: &Declaration, key: &str) -> Result<String, String> {
    d.arguments.get(key).and_then(|v| v.as_str()).map(str::to_owned).ok_or_else(|| error(d, format!("{key} must be a string")))
}
fn name(d: &Declaration) -> Result<String, String> {
    match d.positional.as_slice() {
        [value] => value.as_str().filter(|n| !n.is_empty()).map(str::to_owned).ok_or_else(|| error(d, "relation name must be a nonempty string")),
        _ => Err(error(d, "requires one relation name")),
    }
}
fn ident(s: &str) -> String { format!("\"{}\"", s.replace('"', "\"\"")) }
fn compatible(a: &FieldIr, b: &FieldIr) -> bool {
    a.ty == b.ty && !a.array && !b.array && a.enum_name.is_none() && b.enum_name.is_none()
        && a.db_type == b.db_type && a.read_sql == b.read_sql && a.write_sql == b.write_sql && a.max_length == b.max_length
}
fn check_name(ir: &SchemaIr, model: &str, name: &str) -> Result<(), String> {
    let m = ir.models.iter().find(|m| m.name == model).ok_or_else(|| format!("unknown model {model}"))?;
    if name.chars().enumerate().any(|(i, c)| !(c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))) || name.starts_with('_') {
        return Err(format!("invalid generic relation name {name}"));
    }
    if ["pk", "objects", "update", "delete", "refresh", "then"].contains(&name)
        || m.fields.iter().any(|f| f.name == name) || m.relations.iter().any(|r| r.name == name)
        || ir.behavior.generic_relations.iter().any(|r| r.model == model && r.name == name)
        || ir.behavior.generic_reverse.iter().any(|r| r.model == model && r.name == name)
        || ir.behavior.methods.iter().any(|r| r.model == model && r.name == name) {
        return Err(format!("{model}.{name}: generic relation member collision"));
    }
    Ok(())
}

/// The generic relation `@generic.relation` declares on a `Generic` field: its name, its
/// pair fields and their nullability. The pair fields are created when `type:`/`key:` omit them.
fn field_form(ir: &mut SchemaIr, d: &Declaration, targets: &[String]) -> Result<(String, String, String), String> {
    let name = d.field.clone().expect("field declaration");
    let nullable = match d.field_type.as_deref() {
        Some("Generic") => false,
        Some("Generic?") => true,
        _ => return Err(error(d, "must be on a field of type Generic or Generic?")),
    };
    // Proxy and composition passes ran first; a model they made with this field holds the placeholder.
    let keeps = |model: &str| ir.models.iter().any(|m| m.name == model && m.fields.iter().any(|f| f.name == name));
    let copy = ir.behavior.proxy_models.iter().filter(|p| p.storage_owner == d.model).map(|p| &p.model)
        .chain(ir.behavior.owner_links.iter().filter(|l| l.parent == d.model).map(|l| &l.child))
        .find(|m| keeps(m));
    if let Some(copy) = copy {
        return Err(error(d, format!("{}.{name}: {copy} keeps the Generic field; exclude it there or use @@generic.relation with explicit type and key fields", d.model)));
    }
    let mut created = vec![];
    for (argument, suffix) in [("type", "type"), ("key", "id")] {
        match d.arguments.get(argument).and_then(|v| v.as_str()) {
            None => created.push((argument, format!("{name}_{suffix}"))),
            Some(existing) => {
                let model = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| error(d, "unknown source model"))?;
                if model.fields.iter().find(|f| f.name == existing).is_some_and(|f| f.nullable != nullable) {
                    let mark = if nullable { "Generic?" } else { "Generic" };
                    return Err(error(d, format!("{argument} field {existing} must match the nullability of {mark}")));
                }
            }
        }
    }
    let mut keys = vec![];
    for target in targets {
        let target_model = ir.models.iter().find(|m| &m.name == target).ok_or_else(|| error(d, format!("unknown target {target}")))?;
        match target_model.fields.iter().filter(|f| f.primary_key).collect::<Vec<_>>().as_slice() {
            [pk] => keys.push((*pk).clone()),
            _ => return Err(error(d, format!("{target} needs one scalar primary key"))),
        }
    }
    if keys.windows(2).any(|w| !compatible(&w[0], &w[1])) {
        return Err(error(d, "targets' primary keys have different types; use @@generic.relation with an explicit key field"));
    }
    let mut models = vec![&mut ir.models];
    if let Some(storage) = &mut ir.behavior.storage { models.push(&mut storage.models); }
    for list in models {
        let model = list.iter_mut().find(|m| m.name == d.model).ok_or_else(|| error(d, "source must have its own physical storage model"))?;
        // Lowering kept the `Generic` field as a placeholder at its position.
        let at = model.fields.iter().position(|f| f.name == name).ok_or_else(|| error(d, "unknown Generic field"))?;
        model.fields.remove(at);
        let mut fields = vec![];
        for (argument, field) in &created {
            if model.fields.iter().any(|f| &f.name == field || &f.column == field) || model.relations.iter().any(|r| &r.name == field) {
                return Err(error(d, format!("{}.{field}: the created {argument} field collides with a member; name an existing field with {argument}:", d.model)));
            }
            let mut f = if *argument == "type" {
                let mut f = FieldIr::plain(field, ColType::Int);
                f.enum_name = Some("ContentType".into());
                f
            } else {
                // Only the storage encoding of the target key; never its identity, defaults or hints.
                let key = &keys[0];
                let mut f = FieldIr::plain(field, key.ty);
                (f.db_type, f.max_length, f.read_sql, f.write_sql, f.requires) = (key.db_type.clone(), key.max_length, key.read_sql.clone(), key.write_sql.clone(), key.requires.clone());
                f
            };
            f.nullable = nullable;
            fields.push(f);
        }
        model.fields.splice(at..at, fields);
    }
    let field = |argument: &str| d.arguments.get(argument).and_then(|v| v.as_str()).map(str::to_owned)
        .or_else(|| created.iter().find(|(a, _)| *a == argument).map(|(_, f)| f.clone()));
    let (type_field, key_field) = (field("type").ok_or_else(|| error(d, "type must be a string"))?, field("key").ok_or_else(|| error(d, "key must be a string"))?);
    if d.arguments.get("index").is_none_or(|v| v.as_bool() != Some(false)) {
        let index = || serde_json::from_value::<IndexIr>(serde_json::json!({"columns": [{"field": &type_field}, {"field": &key_field}]})).map_err(|e| e.to_string());
        let same = |i: &IndexIr| i.columns.iter().all(|c| c.expr.is_none()) && i.columns.iter().map(|c| c.field.as_deref()).eq([Some(type_field.as_str()), Some(key_field.as_str())]);
        for model in ir.models.iter_mut().chain(ir.behavior.storage.iter_mut().flat_map(|s| s.models.iter_mut())).filter(|m| m.name == d.model) {
            if !model.indexes.iter().any(same) { model.indexes.push(index()?); }
        }
    }
    Ok((name, type_field, key_field))
}

pub fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations: Vec<_> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute.starts_with("generic.")).cloned().collect();
    if declarations.is_empty() { return Ok(()); }
    let identities = ir.identities.as_ref().ok_or("generic relations require frozen identity metadata; run orm identities")?;
    identities.validate()?;
    let active: std::collections::BTreeMap<_, _> = identities.active().map(|e| (e.model.clone(), e.id)).collect();
    for d in declarations.iter().filter(|d| d.attribute == "generic.relation") {
        let list = d.arguments.get("targets").and_then(|v| v.as_array()).ok_or_else(|| error(d, "targets must be a list"))?;
        let mut targets: Vec<String> = list.iter().map(|v| v.as_str().map(str::to_owned).ok_or_else(|| error(d, "targets must contain model names"))).collect::<Result<_, _>>()?;
        targets.sort();
        if targets.is_empty() || targets.windows(2).any(|w| w[0] == w[1]) { return Err(error(d, "targets must be nonempty and unique")); }
        let (relation_name, type_field, key_field) = if d.field.is_some() {
            field_form(ir, d, &targets)?
        } else {
            (name(d)?, string(d, "type")?, string(d, "key")?)
        };
        check_name(ir, &d.model, &relation_name).map_err(|e| error(d, e))?;
        if type_field == key_field { return Err(error(d, "type and key must use distinct fields")); }
        let model = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| error(d, "unknown source model"))?;
        let ty = model.fields.iter().find(|f| f.name == type_field).ok_or_else(|| error(d, "unknown discriminator field"))?;
        let key = model.fields.iter().find(|f| f.name == key_field).ok_or_else(|| error(d, "unknown object ID field"))?;
        if ty.ty != ColType::Int || ty.enum_name.as_deref() != Some("ContentType") || ty.array || ty.db_type.is_some() || ty.read_sql.is_some() || ty.write_sql.is_some() || ty.primary_key || key.primary_key {
            return Err(error(d, "discriminator must be scalar ContentType integer; neither pair field may be a primary key"));
        }
        if ty.nullable != key.nullable { return Err(error(d, "type and key must have matching nullability")); }
        if ty.default.is_some() || key.default.is_some() || ty.default_now || key.default_now || ty.default_sql.is_some() || key.default_sql.is_some() || ty.auto_increment || key.auto_increment {
            return Err(error(d, "generic pairs cannot have independent server defaults"));
        }
        let mut ids = vec![];
        for target in &targets {
            let id = *active.get(target).ok_or_else(|| error(d, format!("{target} is not an active concrete model identity")))?;
            let target_model = ir.models.iter().find(|m| &m.name == target).ok_or_else(|| error(d, format!("unknown target {target}")))?;
            let pks: Vec<_> = target_model.fields.iter().filter(|f| f.primary_key).collect();
            let pk = match pks.as_slice() { [pk] => *pk, _ => return Err(error(d, format!("{target} needs one scalar primary key"))) };
            if !compatible(key, pk) || !matches!(pk.ty, ColType::Int | ColType::BigInt | ColType::Uuid | ColType::String | ColType::Text) {
                return Err(error(d, format!("{target} primary key has incompatible storage/binding representation")));
            }
            ids.push(id);
        }
        if ir.behavior.generic_relations.iter().any(|r| r.model == d.model && (r.type_field == type_field || r.key_field == key_field || r.type_field == key_field || r.key_field == type_field)) {
            return Err(error(d, "generic pairs must not share storage fields"));
        }
        ids.sort();
        let pair = format!("({} IS NULL) = ({} IS NULL)", ident(&ty.column), ident(&key.column));
        let allowed = format!("{} IN ({})", ident(&ty.column), ids.iter().map(i32::to_string).collect::<Vec<_>>().join(", "));
        let expr = format!("({pair}) AND ({allowed})");
        let check = || ConstraintIr::Check { name: None, expr: expr.clone() };
        ir.models.iter_mut().find(|m| m.name == d.model).unwrap().constraints.push(check());
        if let Some(storage) = &mut ir.behavior.storage {
            storage.models.iter_mut().find(|m| m.name == d.model).ok_or_else(|| error(d, "source must have its own physical storage model"))?.constraints.push(check());
        }
        ir.behavior.generic_relations.push(GenericRelation { model: d.model.clone(), name: relation_name, type_field, key_field, targets });
    }
    for d in declarations.iter().filter(|d| d.attribute == "generic.reverse") {
        let relation_name = d.field.clone().ok_or_else(|| error(d, "must be declared on a field"))?;
        let source = d.field_type.as_deref().and_then(|t| t.strip_suffix("[]")).ok_or_else(|| error(d, "must be on a list field of the source model, such as Tag[]"))?.to_owned();
        check_name(ir, &d.model, &relation_name).map_err(|e| error(d, e))?;
        let candidates: Vec<_> = ir.behavior.generic_relations.iter().filter(|r| r.model == source && r.targets.contains(&d.model)).collect();
        let forward = match d.arguments.get("relation") {
            Some(relation) => {
                let relation = relation.as_str().ok_or_else(|| error(d, "relation must be a string"))?;
                ir.behavior.generic_relations.iter().find(|r| r.model == source && r.name == relation)
                    .ok_or_else(|| error(d, format!("{source} has no generic relation {relation}")))?
            }
            None => match candidates.as_slice() {
                [forward] => forward,
                [] => return Err(error(d, format!("{source} has no generic relation with {} in its targets", d.model))),
                _ => return Err(error(d, format!("{source} has more than one generic relation to {}; name one with relation:", d.model))),
            },
        };
        if !forward.targets.contains(&d.model) { return Err(error(d, "reverse owner is not an allowed concrete target")); }
        let relation = forward.name.clone();
        ir.behavior.generic_reverse.push(GenericReverse { model: d.model.clone(), name: relation_name, source, relation });
    }
    if let Some(d) = declarations.iter().find(|d| !matches!(d.attribute.as_str(), "generic.relation" | "generic.reverse")) {
        return Err(error(d, "unknown generic attribute"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn schema() -> SchemaIr {
        serde_json::from_value(serde_json::json!({
            "identities": {"version":1,"entries":[{"id":1,"model":"Photo","retired":false},{"id":2,"model":"Post","retired":false},{"id":3,"model":"Tag","retired":false}]},
            "enums":[{"name":"ContentType","db_name":"contenttype","storage":"int","values":[{"name":"Photo","value":1},{"name":"Post","value":2},{"name":"Tag","value":3}]}],
            "models":[
                {"name":"Post","table":"posts","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]},
                {"name":"Photo","table":"photos","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]},
                {"name":"Tag","table":"tags","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"content_type","column":"kind","type":"int","enum":"ContentType","nullable":true},{"name":"object_id","column":"target_id","type":"int","nullable":true}]}
            ],
            "behavior":{"schema_contract":1,"declarations":[
                {"attribute":"generic.relation","model":"Tag","field":null,"positional":["target"],"arguments":{"type":"content_type","key":"object_id","targets":["Post","Photo"]},"location":{"file":"schema.prisma","line":10,"column":3}},
                {"attribute":"generic.reverse","model":"Post","field":"tags","field_type":"Tag[]","positional":[],"arguments":{"relation":"target"},"location":{"file":"schema.prisma","line":3,"column":3}}
            ]}
        })).unwrap()
    }
    /// Declared effects `behavior.generic_relations`, `behavior.generic_reverse` and
    /// `storage.generic_checks` (the pair check constraint, logical and physical).
    #[test]
    fn preparation_changes_only_its_declared_effects_once() {
        let effects = orm_contracts::extension::pass_effects(&schema(), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.generic_relations", "behavior.generic_reverse", "behavior.storage", "models"]);
    }
    #[test]
    fn preparation_contributes_pairs_and_reverse_without_foreign_keys() {
        let mut ir = schema();
        orm_contracts::extension::capture_storage(&mut ir).unwrap();
        prepare(&mut ir).unwrap();
        assert_eq!(ir.behavior.generic_relations[0].targets, ["Photo", "Post"]);
        assert_eq!(ir.behavior.generic_reverse[0].model, "Post");
        for model in [&ir.models[2], &ir.behavior.storage.as_ref().unwrap().models[2]] {
            assert!(model.relations.is_empty());
            let ConstraintIr::Check { expr, .. } = &model.constraints[0] else { panic!() };
            assert_eq!(expr, "((\"kind\" IS NULL) = (\"target_id\" IS NULL)) AND (\"kind\" IN (1, 2))");
        }
        assert!(orm_contracts::extension::check_requirements(&ir, &Default::default()).unwrap_err().contains("generic-relations"));
        for d in &mut ir.behavior.declarations { d.lowered = true; }
        prepare(&mut ir).unwrap();
        assert_eq!(ir.models[2].constraints.len(), 1);
    }
    #[test]
    fn incompatible_keys_half_nullable_and_unknown_targets_fail() {
        let mut ir = schema(); ir.models[0].fields[0].ty = ColType::Uuid;
        assert!(prepare(&mut ir).unwrap_err().contains("incompatible"));
        let mut ir = schema(); ir.models[2].fields[1].nullable = false;
        assert!(prepare(&mut ir).unwrap_err().contains("nullability"));
        let mut ir = schema(); ir.behavior.declarations[0].arguments.insert("targets".into(), serde_json::json!(["Unknown"]));
        assert!(prepare(&mut ir).unwrap_err().contains("active concrete"));
    }
    #[test]
    fn reverse_and_member_collisions_fail() {
        let mut ir = schema(); ir.behavior.declarations[1].model = "Tag".into();
        assert!(prepare(&mut ir).unwrap_err().contains("allowed concrete"));
        let mut ir = schema(); ir.behavior.declarations[0].positional = vec!["id".into()];
        assert!(prepare(&mut ir).unwrap_err().contains("collision"));
    }

    /// `target Generic? @generic.relation(targets: ["Post", "Photo"])` after lowering:
    /// a placeholder field at the position of `target`.
    fn field_schema(arguments: serde_json::Value, reverse: serde_json::Value) -> SchemaIr {
        let mut ir = schema();
        ir.models[2].fields = serde_json::from_value(serde_json::json!([
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"target","column":"target","type":"int","nullable":true},
            {"name":"note","column":"note","type":"string"}])).unwrap();
        let mut arguments = arguments;
        arguments["targets"] = serde_json::json!(["Post", "Photo"]);
        ir.behavior.declarations[0] = serde_json::from_value(serde_json::json!({"attribute":"generic.relation","model":"Tag","field":"target","field_type":"Generic?","positional":[],
            "arguments":arguments,"location":{"file":"schema.prisma","line":10,"column":3}})).unwrap();
        ir.behavior.declarations[1].arguments = serde_json::from_value(reverse).unwrap();
        orm_contracts::extension::capture_storage(&mut ir).unwrap();
        ir
    }
    fn columns(model: &orm_contracts::ir::ModelIr) -> Vec<(String, bool, Option<String>)> {
        model.fields.iter().map(|f| (f.name.clone(), f.nullable, f.enum_name.clone())).collect()
    }
    #[test]
    fn a_generic_field_creates_its_pair_and_index_at_its_position() {
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
        prepare(&mut ir).unwrap();
        let relation = &ir.behavior.generic_relations[0];
        assert_eq!((relation.name.as_str(), relation.type_field.as_str(), relation.key_field.as_str()), ("target", "target_type", "target_id"));
        assert_eq!(ir.behavior.generic_reverse[0].relation, "target");
        for model in [&ir.models[2], &ir.behavior.storage.as_ref().unwrap().models[2]] {
            assert_eq!(columns(model), [("id".into(), false, None), ("target_type".into(), true, Some("ContentType".into())), ("target_id".into(), true, None), ("note".into(), false, None)]);
            assert_eq!(model.fields[2].ty, ColType::Int);
            let fields: Vec<_> = model.indexes[0].columns.iter().map(|c| c.field.clone().unwrap()).collect();
            assert_eq!(fields, ["target_type", "target_id"]);
            assert_eq!(model.constraints.len(), 1);
        }
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
        ir.behavior.declarations[0].field_type = Some("Generic".into());
        prepare(&mut ir).unwrap();
        assert!(!ir.models[2].fields[1].nullable && !ir.models[2].fields[2].nullable);
        let mut ir = field_schema(serde_json::json!({"index": false}), serde_json::json!({}));
        prepare(&mut ir).unwrap();
        assert!(ir.models[2].indexes.is_empty() && ir.behavior.storage.as_ref().unwrap().models[2].indexes.is_empty());
    }
    #[test]
    fn type_and_key_name_existing_fields_like_the_explicit_form() {
        let mut explicit = schema();
        explicit.models[2].indexes = serde_json::from_value(serde_json::json!([{"columns":[{"field":"content_type"},{"field":"object_id"}]}])).unwrap();
        orm_contracts::extension::capture_storage(&mut explicit).unwrap();
        prepare(&mut explicit).unwrap();
        let mut ir = field_schema(serde_json::json!({"type": "content_type", "key": "object_id"}), serde_json::json!({}));
        ir.models[2].fields.splice(1..1, schema().models[2].fields.drain(1..));
        ir.behavior.storage = None;
        ir.models[2].fields.retain(|f| f.name != "note");
        orm_contracts::extension::capture_storage(&mut ir).unwrap();
        prepare(&mut ir).unwrap();
        assert_eq!(ir.behavior.generic_relations, explicit.behavior.generic_relations);
        assert_eq!(serde_json::to_value(&ir.models).unwrap(), serde_json::to_value(&explicit.models).unwrap());
        assert_eq!(serde_json::to_value(&ir.behavior.storage).unwrap(), serde_json::to_value(&explicit.behavior.storage).unwrap());
    }
    #[test]
    fn generic_fields_reject_mixed_keys_collisions_and_wrong_types() {
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
        ir.models[1].fields[0].ty = ColType::BigInt;
        assert!(prepare(&mut ir).unwrap_err().contains("different types"));
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
        ir.models[2].fields[2].name = "target_id".into();
        ir.behavior.storage.as_mut().unwrap().models[2].fields[2].name = "target_id".into();
        assert!(prepare(&mut ir).unwrap_err().contains("Tag.target_id: the created key field collides"));
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
        ir.behavior.declarations[0].field_type = Some("Int".into());
        let error = prepare(&mut ir).unwrap_err();
        assert!(error.starts_with("schema.prisma:10:3: @generic.relation: must be on a field of type Generic"), "{error}");
    }
    #[test]
    fn a_reverse_field_finds_its_relation_or_names_it() {
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({"relation": "target"}));
        prepare(&mut ir).unwrap();
        assert_eq!((ir.behavior.generic_reverse[0].source.as_str(), ir.behavior.generic_reverse[0].name.as_str()), ("Tag", "tags"));
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({"relation": "other"}));
        assert!(prepare(&mut ir).unwrap_err().contains("Tag has no generic relation other"));
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
        ir.behavior.declarations[1].field_type = Some("Photo[]".into());
        assert!(prepare(&mut ir).unwrap_err().contains("Photo has no generic relation with Post in its targets"));
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
        ir.behavior.declarations[1].field_type = Some("Tag".into());
        assert!(prepare(&mut ir).unwrap_err().contains("list field"));
        // A second relation to Post makes the reverse field name one.
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
        ir.models[2].fields.push(serde_json::from_value(serde_json::json!({"name":"owner","column":"owner","type":"int","nullable":true})).unwrap());
        ir.behavior.storage.as_mut().unwrap().models[2].fields.push(serde_json::from_value(serde_json::json!({"name":"owner","column":"owner","type":"int","nullable":true})).unwrap());
        let mut second = ir.behavior.declarations[0].clone();
        second.field = Some("owner".into());
        ir.behavior.declarations.insert(1, second);
        assert!(prepare(&mut ir).unwrap_err().contains("more than one generic relation to Post; name one with relation:"));
    }
    #[test]
    fn named_pair_fields_match_the_nullability_of_the_generic_field() {
        let mut ir = field_schema(serde_json::json!({"type": "content_type", "key": "object_id"}), serde_json::json!({}));
        ir.models[2].fields.splice(1..1, schema().models[2].fields.drain(1..));
        ir.behavior.storage = None;
        orm_contracts::extension::capture_storage(&mut ir).unwrap();
        ir.behavior.declarations[0].field_type = Some("Generic".into());
        let error = prepare(&mut ir).unwrap_err();
        assert!(error.contains("type field content_type must match the nullability of Generic"), "{error}");
    }
    #[test]
    fn the_key_field_takes_only_the_storage_encoding_of_the_target_key() {
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
        for model in &mut ir.models[..2] {
            let pk = &mut model.fields[0];
            pk.auto_increment = true;
            pk.default_sql = Some("1".into());
            pk.client_default = Some(orm_contracts::ir::ClientDefaultIr::Call(orm_contracts::ir::ClientCall::Uuid7));
            pk.hints.insert("python".into(), "int".into());
            pk.max_length = Some(9);
            (pk.db_type, pk.write_sql, pk.requires) = (Some("citext".into()), Some("lower(?)".into()), vec!["citext".into()]);
        }
        prepare(&mut ir).unwrap();
        let key = &ir.models[2].fields[2];
        assert_eq!(key.name, "target_id");
        assert!(!key.auto_increment && key.default_sql.is_none() && key.client_default.is_none() && key.hints.is_empty());
        assert_eq!((key.ty, key.max_length, key.nullable), (ColType::Int, Some(9), true));
        assert_eq!((key.db_type.as_deref(), key.write_sql.as_deref(), key.requires.as_slice()), (Some("citext"), Some("lower(?)"), ["citext".to_string()].as_slice()));
    }
    #[test]
    fn a_generic_field_kept_by_a_proxy_or_composed_child_names_the_explicit_form() {
        // `copy` is the model that the proxy or composition pass made from Tag; `keep` says whether it kept `target`.
        let with = |copy: &str, keep: bool, link: fn(&mut SchemaIr, &str)| {
            let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
            let mut model: orm_contracts::ir::ModelIr = serde_json::from_value(serde_json::to_value(&ir.models[2]).unwrap()).unwrap();
            model.name = copy.into();
            model.fields.retain(|f| keep || f.name != "target");
            ir.models.push(model);
            link(&mut ir, copy);
            prepare(&mut ir)
        };
        let proxy: fn(&mut SchemaIr, &str) = |ir, m| ir.behavior.proxy_models.push(orm_contracts::extension::ProxyModel { model: m.into(), parent: "Tag".into(), storage_owner: "Tag".into(), ..Default::default() });
        let child: fn(&mut SchemaIr, &str) = |ir, m| ir.behavior.owner_links.push(orm_contracts::extension::OwnerLink { child: m.into(), parent: "Tag".into(), child_key: "id".into(), parent_key: "id".into() });
        for (copy, link) in [("NotedTag", proxy), ("Child", child)] {
            let error = with(copy, true, link).unwrap_err();
            assert!(error.contains(&format!("Tag.target: {copy} keeps the Generic field; exclude it there or use @@generic.relation")), "{error}");
        }
        // A proxy that does not select the field copies no placeholder.
        with("NotedTag", false, proxy).unwrap();
    }
    #[test]
    fn an_existing_pair_index_is_not_repeated() {
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
        let mine = || serde_json::from_value::<Vec<IndexIr>>(serde_json::json!([{"columns":[{"field":"target_type"},{"field":"target_id"}], "name": "mine"}])).unwrap();
        ir.models[2].indexes = mine();
        ir.behavior.storage.as_mut().unwrap().models[2].indexes = mine();
        prepare(&mut ir).unwrap();
        for model in [&ir.models[2], &ir.behavior.storage.as_ref().unwrap().models[2]] {
            assert_eq!(model.indexes.iter().map(|i| i.name.as_deref()).collect::<Vec<_>>(), [Some("mine")]);
        }
    }
    #[test]
    fn created_fields_collide_with_relations_and_columns() {
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
        ir.models[2].relations = serde_json::from_value(serde_json::json!([{"name":"target_type","kind":"one","target":"Post","from":"note","to":"id"}])).unwrap();
        assert!(prepare(&mut ir).unwrap_err().contains("the created type field collides"));
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
        for model in std::iter::once(&mut ir.models[2]).chain(ir.behavior.storage.as_mut().unwrap().models.get_mut(2)) { model.fields[2].column = "target_id".into(); }
        assert!(prepare(&mut ir).unwrap_err().contains("the created key field collides"));
    }
    #[test]
    fn a_reverse_field_ignores_relations_without_its_model_in_their_targets() {
        let mut ir = field_schema(serde_json::json!({}), serde_json::json!({}));
        ir.models[2].fields.push(serde_json::from_value(serde_json::json!({"name":"owner","column":"owner","type":"int","nullable":true})).unwrap());
        ir.behavior.storage.as_mut().unwrap().models[2].fields.push(serde_json::from_value(serde_json::json!({"name":"owner","column":"owner","type":"int","nullable":true})).unwrap());
        let mut second = ir.behavior.declarations[0].clone();
        second.field = Some("owner".into());
        second.arguments.insert("targets".into(), serde_json::json!(["Photo"]));
        ir.behavior.declarations.insert(1, second);
        prepare(&mut ir).unwrap();
        assert_eq!(ir.behavior.generic_reverse[0].relation, "target");
    }
}
