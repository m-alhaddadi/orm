//! Same-table views are lowered once, before policy/result preparation.
//! The compiler preserves parent enum identities and server defaults. Neither
//! narrowing nor client defaults change physical storage or imply predicates.
use std::collections::{BTreeMap, BTreeSet};
use orm_contracts::{extension::{capture_storage, ProxyField, ProxyModel}, ir::{FieldIr, ModelIr, SchemaIr}};

/// Namespaced declarations contribute logical metadata only. Required argument
/// kinds/targets are checked by the composition manifest before this pass runs.
pub fn lower(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations: Vec<_> = ir.behavior.declarations.iter().filter(|d| d.attribute.starts_with("proxy.") && !d.lowered).cloned().collect();
    let mut specs = BTreeMap::new();
    for d in declarations.iter().filter(|d| d.attribute == "proxy.of") {
        let parent = d.positional.first().and_then(|v| v.as_str()).ok_or("proxy.of requires a source model name")?;
        let spec = ProxyModel { model: d.model.clone(), parent: parent.into(), storage_owner: String::new(), fields: vec![], defaults: BTreeMap::new() };
        if specs.insert(d.model.clone(), spec).is_some() { return Err(format!("{}: duplicate proxy source", d.model)); }
    }
    let mut overrides = BTreeSet::new();
    for d in declarations.iter().filter(|d| d.attribute != "proxy.of") {
        let error = |message: &str| format!("{}:{}:{}: @@{}: {message}", d.location.file, d.location.line, d.location.column, d.attribute);
        let spec = specs.get_mut(&d.model).ok_or_else(|| error("override requires @@proxy.of on the same model"))?;
        let field = d.positional.first().and_then(|v| v.as_str()).ok_or_else(|| error("requires an inherited field name"))?;
        if !overrides.insert((&d.model, field, &d.attribute)) { return Err(error("duplicate proxy override")); }
        if d.attribute == "proxy.default" {
            let value = d.positional.get(1).ok_or_else(|| error("requires a literal client default"))?;
            if spec.defaults.insert(field.into(), value.clone()).is_some() { return Err(error("duplicate client default")); }
            continue;
        }
        let pos = spec.fields.iter().position(|f| f.field == field);
        let contract = match pos {
            Some(pos) => &mut spec.fields[pos],
            None => {
                // Inherit nullability from the nearest declared or normalized
                // source field, even when that source is another pending proxy.
                let mut parent = spec.parent.as_str();
                let mut visited = BTreeSet::new();
                let nullable = loop {
                    if !visited.insert(parent) { return Err(error("cyclic proxy source chain")); }
                    if declarations.iter().any(|p| p.model == parent && p.attribute == "proxy.nonNull" && p.positional.first().and_then(|v| v.as_str()) == Some(field)) { break false; }
                    if let Some(f) = ir.models.iter().find(|m| m.name == parent).and_then(|m| m.fields.iter().find(|f| f.name == field)) { break f.nullable; }
                    parent = declarations.iter().find(|p| p.model == parent && p.attribute == "proxy.of")
                        .and_then(|p| p.positional.first()).and_then(|v| v.as_str()).ok_or_else(|| error("unknown inherited field"))?;
                };
                spec.fields.push(ProxyField { field: field.into(), non_null: !nullable, subset: None });
                spec.fields.last_mut().expect("added contract")
            }
        };
        match d.attribute.as_str() {
            "proxy.nonNull" => contract.non_null = true,
            "proxy.subset" => {
                if contract.subset.is_some() { return Err(error("duplicate enum subset")); }
                contract.subset = Some(d.positional.get(1).and_then(|v| v.as_array()).ok_or_else(|| error("requires enum member list"))?
                    .iter().map(|v| v.as_str().map(str::to_owned).ok_or_else(|| error("subset members must be names"))).collect::<Result<_, _>>()?);
            }
            _ => return Err(error("unknown proxy declaration")),
        }
    }
    lower_specs(ir, &specs.into_values().collect::<Vec<_>>())
}

/// Resolve a complete declaration batch. Existing normalized parents can be
/// referenced; only declarations in this batch are transformed.
pub fn lower_specs(ir: &mut SchemaIr, specs: &[ProxyModel]) -> Result<(), String> {
    let mut by_model = BTreeMap::new();
    for spec in specs {
        if by_model.insert(spec.model.as_str(), spec).is_some()
            || ir.behavior.proxy_models.iter().any(|p| p.model == spec.model) {
            return Err(format!("{}: duplicate proxy source", spec.model));
        }
        if !ir.models.iter().any(|m| m.name == spec.model) {
            return Err(format!("{}: unknown proxy model", spec.model));
        }
        if !ir.models.iter().any(|m| m.name == spec.parent) {
            return Err(format!("{}: unknown proxy source {}; define dependent schemas together", spec.model, spec.parent));
        }
    }
    // Topological order is independent of model/declaration order.
    let mut order = Vec::new();
    let mut done = BTreeSet::new();
    fn visit<'a>(name: &'a str, specs: &BTreeMap<&'a str, &'a ProxyModel>,
        stack: &mut BTreeSet<&'a str>, done: &mut BTreeSet<&'a str>, order: &mut Vec<&'a ProxyModel>) -> Result<(), String> {
        if done.contains(name) { return Ok(()); }
        if !stack.insert(name) { return Err(format!("{name}: cyclic proxy source chain")); }
        let spec = specs[name];
        if specs.contains_key(spec.parent.as_str()) { visit(&spec.parent, specs, stack, done, order)?; }
        stack.remove(name);
        done.insert(name);
        order.push(spec);
        Ok(())
    }
    for name in by_model.keys() { visit(name, &by_model, &mut BTreeSet::new(), &mut done, &mut order)?; }
    capture_storage(ir)?;
    for spec in order {
        let source = ir.models.iter().find(|m| m.name == spec.parent).expect("checked source");
        let mut model: ModelIr = copy(source)?;
        let placeholder = ir.models.iter().find(|m| m.name == spec.model).expect("checked model");
        if !placeholder.relations.is_empty() {
            return Err(format!("{}: proxy relation overrides are unsupported; inherited relations retain their logical targets", spec.model));
        }
        if !placeholder.indexes.is_empty() || !placeholder.constraints.is_empty()
            || !placeholder.triggers.is_empty() || placeholder.renamed_from.is_some() {
            return Err(format!("{}: proxies cannot declare physical objects", spec.model));
        }
        // Repeated field declarations cannot quietly become physical overrides.
        for declared in &placeholder.fields {
            let inherited = source.fields.iter().find(|f| f.name == declared.name)
                .ok_or_else(|| format!("{}.{}: a proxy cannot add a stored field", spec.model, declared.name))?;
            let mut comparable = declared.clone();
            if declared.primary_key && declared.nullable != inherited.nullable {
                return Err(format!("{}.{}: proxy cannot change primary key shape", spec.model, declared.name));
            }
            comparable.nullable = inherited.nullable;
            comparable.comment = inherited.comment.clone();
            comparable.hints = inherited.hints.clone();
            if serde_json::to_value(&comparable).map_err(|e| e.to_string())? != serde_json::to_value(inherited).map_err(|e| e.to_string())? {
                return Err(format!("{}.{}: proxy override changes physical type, encoding, identity or constraint; use a logical proxy declaration", spec.model, declared.name));
            }
            model.fields.iter_mut().find(|f| f.name == declared.name).expect("inherited field").nullable = declared.nullable;
        }
        model.name = spec.model.clone();
        model.comment = placeholder.comment.clone();
        model.indexes.clear(); model.constraints.clear(); model.triggers.clear(); model.renamed_from = None;
        // Relations retain logical targets; the physical schema resolves FKs to
        // owners below, avoiding references to nonexistent proxy tables.
        for rel in &mut model.relations { rel.foreign_key = false; }
        let inherited = ir.behavior.proxy_models.iter().find(|p| p.model == spec.parent);
        let mut prepared = ProxyModel { model: spec.model.clone(), parent: spec.parent.clone(),
            storage_owner: inherited.map(|p| p.storage_owner.clone()).unwrap_or_else(|| source.name.clone()),
            fields: inherited.map(|p| p.fields.clone()).unwrap_or_default(),
            defaults: inherited.map(|p| p.defaults.clone()).unwrap_or_default() };
        for declared in &placeholder.fields {
            if let Some(contract) = prepared.fields.iter_mut().find(|p| p.field == declared.name) {
                contract.non_null = !declared.nullable;
            } else if !declared.nullable && source.fields.iter().any(|f| f.name == declared.name && f.nullable) {
                prepared.fields.push(ProxyField { field: declared.name.clone(), non_null: true, subset: None });
            }
        }
        let mut seen = BTreeSet::new();
        for contract in &spec.fields {
            if !seen.insert(&contract.field) { return Err(format!("{}.{}: duplicate proxy shape", spec.model, contract.field)); }
            let f = model.fields.iter_mut().find(|f| f.name == contract.field)
                .ok_or_else(|| format!("{}.{}: unknown proxy field", spec.model, contract.field))?;
            if f.primary_key && (contract.subset.is_some() || (contract.non_null && f.nullable)) {
                return Err(format!("{}.{}: proxy cannot change primary key shape", spec.model, f.name));
            }
            if let Some(members) = &contract.subset {
                let e = f.enum_name.as_ref().and_then(|n| ir.enums.iter().find(|e| e.name == *n))
                    .ok_or_else(|| format!("{}.{}: subset requires a parent enum", spec.model, f.name))?;
                if members.is_empty() || members.iter().collect::<BTreeSet<_>>().len() != members.len()
                    || members.iter().any(|n| !e.values.iter().any(|v| v.name == *n)) {
                    return Err(format!("{}.{}: enum subset must contain distinct parent members", spec.model, f.name));
                }
                // Runtime continues using e.name and all its members. Hints alone
                // describe the intended subset, including enum arrays.
                let python: Vec<_> = members.iter().map(|n| format!("{}.{n}", e.name)).collect();
                let typescript: Vec<_> = members.iter().map(|n| format!("typeof {}.{n}", e.name)).collect();
                f.hints.insert("python".into(), format!("Literal[{}]", python.join(", ")));
                f.hints.insert("typescript".into(), typescript.join(" | "));
            }
            f.nullable &= !contract.non_null;
            let subset = contract.subset.clone().or_else(|| prepared.fields.iter().find(|p| p.field == f.name).and_then(|p| p.subset.clone()));
            prepared.fields.retain(|p| p.field != contract.field);
            prepared.fields.push(ProxyField { field: f.name.clone(), non_null: !f.nullable, subset });
        }
        for (field, value) in &spec.defaults {
            let f = model.fields.iter().find(|f| f.name == *field)
                .ok_or_else(|| format!("{}.{}: unknown client-default field", spec.model, field))?;
            validate_default(f, value).map_err(|e| format!("{}.{}: {e}", spec.model, field))?;
            if let Some(name) = &f.enum_name {
                let e = ir.enums.iter().find(|e| e.name == *name).ok_or("unknown enum")?;
                let values: Vec<&serde_json::Value> = if f.array { value.as_array().map(|a| a.iter().collect()).unwrap_or_default() } else { vec![value] };
                if values.iter().any(|v| !v.is_null() && !e.values.iter().any(|m| m.value == **v)) {
                    return Err(format!("{}.{}: client default must use physical parent enum values", spec.model, field));
                }
            }
            prepared.defaults.insert(field.clone(), value.clone());
        }
        let pos = ir.models.iter().position(|m| m.name == spec.model).expect("checked proxy");
        ir.models[pos] = model;
        ir.behavior.storage.as_mut().expect("captured storage").models.retain(|m| m.name != spec.model);
        ir.behavior.proxy_models.push(prepared);
    }
    // Physical relations may name a proxy. Resolve only the physical FK target;
    // logical relation targets preserve their view and policies.
    let storage = ir.behavior.storage.as_mut().expect("captured storage");
    for model in &mut storage.models {
        for relation in &mut model.relations {
            let mut target = relation.target.clone();
            let mut visited = BTreeSet::new();
            while let Some(proxy) = ir.behavior.proxy_models.iter().find(|p| p.model == target) {
                if !visited.insert(target.clone()) { return Err("cyclic normalized proxy metadata".into()); }
                target = proxy.parent.clone();
            }
            relation.target = target;
        }
    }
    Ok(())
}

fn copy<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) -> Result<T, String> {
    serde_json::from_value(serde_json::to_value(value).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn validate_default(field: &FieldIr, value: &serde_json::Value) -> Result<(), String> {
    use orm_contracts::ir::ColType;
    // Shape violations, including null for a non-null view, are warning-only.
    // Validate encoding here; physical constraints remain database-enforced.
    if value.is_null() { return Ok(()); }
    if field.array {
        let values = value.as_array().ok_or("array client default requires an array")?;
        let scalar = FieldIr { array: false, ..field.clone() };
        for v in values { validate_default(&scalar, v)?; }
        return Ok(());
    }
    let valid = match field.ty {
        ColType::Int => value.as_i64().is_some_and(|v| i32::try_from(v).is_ok()),
        ColType::BigInt => value.is_i64(),
        ColType::Float => value.is_number(),
        ColType::Bool => value.is_boolean(),
        ColType::Json => true,
        ColType::String | ColType::Text | ColType::DateTime | ColType::Date | ColType::Uuid | ColType::Decimal => value.is_string(),
    };
    if valid { Ok(()) } else { Err(format!("client default does not encode as {:?}", field.ty)) }
}

#[cfg(test)]
mod effects {
    use orm_contracts::{extension::pass_effects, ir::SchemaIr};

    /// Declared effect `orm.proxy.logical-models`: the logical models, their contracts, and
    /// the storage snapshot, where a proxy shares its parent table instead of its own.
    #[test]
    fn lowering_changes_only_logical_proxy_models_once() {
        let ir: SchemaIr = serde_json::from_value(serde_json::json!({"models":[
            {"name":"User","table":"users","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"name","column":"name","type":"string","nullable":true}]},
            {"name":"Active","table":"active","fields":[]}
        ],"behavior":{"declarations":[
            {"attribute":"proxy.of","model":"Active","field":null,"arguments":{},"positional":["User"],"location":{"file":"schema.prisma","line":1,"column":1}},
            {"attribute":"proxy.nonNull","model":"Active","field":null,"arguments":{},"positional":["name"],"location":{"file":"schema.prisma","line":2,"column":1}},
            {"attribute":"proxy.default","model":"Active","field":null,"arguments":{},"positional":["name","client"],"location":{"file":"schema.prisma","line":3,"column":1}}
        ]}})).unwrap();
        assert_eq!(pass_effects(&ir, super::lower).unwrap().into_iter().collect::<Vec<_>>(), ["behavior.proxy_models", "behavior.storage", "models"]);
    }
}
