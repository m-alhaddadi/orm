//! Compiler contribution for type/key references. Execution requires the native
//! generic-relations host capability; this crate alone never promises loading.
use orm_contracts::{extension::Declaration, generic::{GenericRelation, GenericReverse}, ir::{ColType, ConstraintIr, FieldIr, SchemaIr}};

fn error(d: &Declaration, message: impl std::fmt::Display) -> String {
    format!("{}:{}:{}: @@{}: {message}", d.location.file, d.location.line, d.location.column, d.attribute)
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

pub fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations: Vec<_> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute.starts_with("generic.")).cloned().collect();
    if declarations.is_empty() { return Ok(()); }
    let identities = ir.identities.as_ref().ok_or("generic relations require frozen identity metadata; run orm identities")?;
    identities.validate()?;
    let active: std::collections::BTreeMap<_, _> = identities.active().map(|e| (e.model.clone(), e.id)).collect();
    for d in declarations.iter().filter(|d| d.attribute == "generic.relation") {
        if d.field.is_some() { return Err(error(d, "must be declared on a model")); }
        let relation_name = name(d)?;
        check_name(ir, &d.model, &relation_name).map_err(|e| error(d, e))?;
        let type_field = string(d, "type")?;
        let key_field = string(d, "key")?;
        if type_field == key_field { return Err(error(d, "type and key must use distinct fields")); }
        let list = d.arguments.get("targets").and_then(|v| v.as_array()).ok_or_else(|| error(d, "targets must be a list"))?;
        let mut targets: Vec<String> = list.iter().map(|v| v.as_str().map(str::to_owned).ok_or_else(|| error(d, "targets must contain model names"))).collect::<Result<_, _>>()?;
        targets.sort();
        if targets.is_empty() || targets.windows(2).any(|w| w[0] == w[1]) { return Err(error(d, "targets must be nonempty and unique")); }
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
        if d.field.is_some() { return Err(error(d, "must be declared on a model")); }
        let relation_name = name(d)?;
        check_name(ir, &d.model, &relation_name).map_err(|e| error(d, e))?;
        let source = string(d, "source")?;
        let relation = string(d, "relation")?;
        let forward = ir.behavior.generic_relations.iter().find(|r| r.model == source && r.name == relation)
            .ok_or_else(|| error(d, "reverse must name an existing forward generic relation"))?;
        if !forward.targets.contains(&d.model) { return Err(error(d, "reverse owner is not an allowed concrete target")); }
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
                {"attribute":"generic.reverse","model":"Post","field":null,"positional":["tags"],"arguments":{"source":"Tag","relation":"target"},"location":{"file":"schema.prisma","line":3,"column":3}}
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
}
