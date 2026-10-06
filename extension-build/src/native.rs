//! Model specializations emitted as Rust source; execution never reads these specs.
use orm_contracts::{extension::{ExportKind, Manifest, Specialization}, ir::{ColType, SchemaIr}};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub use orm_contracts::native::{FieldRule, Computed, RecordRule, Method, NativeSpec};

pub struct Sources {
    pub core: String,
    pub engine: String,
    pub specializations: Vec<Specialization>,
    pub python: String,
    pub node: String,
}

/// Resolve names and types at build time. The generated hot path uses field positions.
pub fn generate(schema: &mut SchemaIr, specs: &[NativeSpec], manifests: &[Manifest]) -> Result<Sources, String> {
    generate_with_sources(schema, specs, manifests, &BTreeMap::new())
}

pub fn generate_with_sources(schema: &mut SchemaIr, specs: &[NativeSpec], manifests: &[Manifest], sources: &BTreeMap<String, String>) -> Result<Sources, String> {
    let mut specs = specs.to_vec();
    specs.sort_by(|a, b| a.model.cmp(&b.model));
    for spec in &mut specs { spec.methods.sort_by(|a, b| a.name.cmp(&b.name)); }
    let exports: BTreeMap<_, _> = manifests.iter().flat_map(|m| m.exports.iter().map(|e| (e.id.as_str(), e))).collect();
    let mut core = String::from("#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub enum NativeModel { None,\n");
    for i in 0..specs.len() { core.push_str(&format!("S{i},\n")); }
    core.push_str("}\nimpl NativeModel { pub fn computed(self) -> &'static [usize] { match self { Self::None => &[],\n");
    let mut engine = String::from("use orm_core::behavior::NativeModel;\nuse sea_query::Value;\nuse crate::error::{Error, Result};\n");
    let mut field_arms = String::new();
    let mut field_helpers = String::new();
    let mut record_helpers = String::new();
    let mut insert_bulk = String::new();
    let mut update_bulk = String::new();
    let mut validated_arms = String::new();
    let mut record_models = String::new();
    let mut dependency_arms = String::new();
    let mut computation_arms = String::new();
    let mut bind_arms = String::new();
    let mut restricted = String::new();
    let mut insert_requirements = String::new();
    let mut native_models = String::new();
    let mut record_arms = String::new();
    let mut specializations = vec![];
    let mut python = String::from("#[allow(unused_imports)] use pyo3::prelude::*;\n");
    let mut node = String::from("#[allow(unused_imports)] use napi_derive::napi;\n");
    let mut registration = String::new();
    let mut seen_models = std::collections::BTreeSet::new();
    for (i, spec) in specs.iter().enumerate() {
        if !seen_models.insert(&spec.model) { return Err(format!("duplicate native model {}", spec.model)); }
        let model = schema.models.iter_mut().find(|m| m.name == spec.model).ok_or_else(|| format!("unknown native model {}", spec.model))?;
        if !spec.records.is_empty() { record_models.push_str(&format!("Self::S{i} => true,\n")); }
        let mut ids = vec![];
        let mut validated_positions = std::collections::BTreeSet::new();
        if !spec.fields.is_empty() || !spec.records.is_empty() { native_models.push_str(&format!("NativeModel::S{i} => Err(Error::query(\"native validation does not support upsert conflict outcomes; use an explicit transaction or database constraint\")),\n")); }
        insert_requirements.push_str(&format!("NativeModel::S{i} => {{\n"));
        let mut seen_fields = std::collections::BTreeSet::new();
        let mut rule_positions = vec![];
        for rule in &spec.fields {
            if !seen_fields.insert(&rule.field) { return Err(format!("{}.{}: duplicate native field rule", spec.model, rule.field)); }
            let pos = model.fields.iter().position(|f| f.name == rule.field).ok_or_else(|| format!("{}.{}: unknown field", spec.model, rule.field))?;
            restricted.push_str(&format!("(NativeModel::S{i}, {pos}) => Err(Error::query(\"native validated fields require supplied values; database defaults and expressions need a constraint or explicit transaction strategy\")),\n"));
            insert_requirements.push_str(&format!("if map.get({pos}).copied().flatten().is_none() {{ return Err(Error::query({:?})); }}\n", format!("{}.{}: native validation requires an explicitly supplied insert value", spec.model, rule.field)));
            validated_positions.insert(pos);
            let field = &model.fields[pos];
            if !matches!(field.ty, ColType::String | ColType::Text) || field.array || field.enum_name.is_some() {
                return Err(format!("{}.{}: borrowed string exports require a scalar string", spec.model, rule.field));
            }
            rule_positions.push(pos);
            let mut body = format!("fn field_s{i}_{pos}(value: &mut Value) -> Result<()> {{\n");
            body.push_str(&format!("let v = match value {{ Value::String(Some(v)) => v, Value::String(None) if {} => return Ok(()), _ => return Err(Error::query({:?})) }};\n", rule.allow_null, format!("{}.{}: validator requires a non-null string", spec.model, rule.field)));
            for (export, kind) in rule.transforms.iter().map(|s| (s, ExportKind::StringTransform)).chain(rule.validators.iter().map(|s| (s, ExportKind::StringValidator))) {
                let e = exports.get(export.as_str()).ok_or_else(|| format!("missing export {export}; rebuild"))?;
                if e.kind != kind || !matches!(e.input, ColType::String | ColType::Text) || (kind == ExportKind::StringValidator && e.output.is_some()) { return Err(format!("wrong export type for {export}")); }
                ids.push(export.clone());
                if kind == ExportKind::StringTransform {
                    if !matches!(e.output, Some(ColType::String | ColType::Text)) { return Err(format!("wrong transform output for {export}")); }
                    body.push_str(&format!("*v = {}(v.as_str()).map_err(Error::query)?;\n", e.rust));
                } else {
                    body.push_str(&format!("let () = {}(v.as_str()).map_err(Error::query)?;\n", e.rust));
                }
            }
            body.push_str("Ok(()) }\n");
            field_helpers.push_str(&body);
            field_arms.push_str(&format!("(NativeModel::S{i}, {pos}) => field_s{i}_{pos}(value),\n"));
        }
        let mut record_body = format!("fn record_s{i}<R: crate::behavior::Values + ?Sized>(map: &[Option<usize>], values: &R) -> Result<()> {{\n");
        for rule in &spec.records {
            if rule.dependencies.is_empty() { return Err(format!("{}: record validator must declare dependencies", spec.model)); }
            let export = exports.get(rule.export.as_str()).ok_or_else(|| format!("missing record export {}", rule.export))?;
            if export.kind != ExportKind::StringRecordValidator || !matches!(export.input, ColType::String | ColType::Text) || export.output.is_some() {
                return Err(format!("wrong record export type for {}", rule.export));
            }
            ids.push(rule.export.clone());
            let mut arguments = vec![];
            let dependencies: Vec<_> = rule.dependencies.iter().map(|name| model.fields.iter().position(|f| f.name == *name).ok_or_else(|| format!("missing record dependency {name}"))).collect::<Result<_, _>>()?;
            let dependencies = format!("{dependencies:?}");
            record_body.push_str(&format!("if {dependencies}.iter().any(|d| map.get(*d).copied().flatten().is_some()) {{\n"));
            for dependency in &rule.dependencies {
                let position = model.fields.iter().position(|f| f.name == *dependency).ok_or_else(|| format!("missing record dependency {dependency}"))?;
                validated_positions.insert(position);
                let field = model.fields.iter().find(|f| f.name == *dependency).ok_or_else(|| format!("missing record dependency {dependency}"))?;
                if !matches!(field.ty, ColType::String | ColType::Text) || field.array || field.enum_name.is_some() {
                    return Err(format!("record dependency {dependency} requires a scalar string"));
                }
                insert_requirements.push_str(&format!("if map.get({position}).copied().flatten().is_none() {{ return Err(Error::query(\"record validation requires all insert dependencies\")); }}\n"));
                arguments.push(format!("match map.get({position}).copied().flatten().and_then(|p| values.value(p)) {{ Some(Value::String(Some(v))) => v.as_str(), _ => return Err(Error::query(\"record validation requires all non-null supplied dependencies; use an explicit transaction for partial records\")) }}"));
            }
            record_body.push_str(&format!("let args = [{}];\nlet () = {}(&args).map_err(Error::query)?;\n}}\n", arguments.join(", "), export.rust));
        }
        record_body.push_str("Ok(()) }\n");
        record_helpers.push_str(&record_body);
        record_arms.push_str(&format!("NativeModel::S{i} => record_s{i}(map, values),\n"));
        for (bulk, insert) in [(&mut insert_bulk, true), (&mut update_bulk, false)] {
            bulk.push_str(&format!("NativeModel::S{i} => {{\n"));
            if insert { bulk.push_str(&format!("insert_fields(NativeModel::S{i}, map)?;\n")); }
            for pos in &rule_positions { bulk.push_str(&format!("let slot_{pos} = map.get({pos}).copied().flatten();\n")); }
            if !rule_positions.is_empty() || !spec.records.is_empty() {
                bulk.push_str("for row in rows { if row.len() != width { return Err(Error::query(\"write row length does not match fields\")); }\n");
                for pos in &rule_positions {
                    if insert {
                        bulk.push_str(&format!("if let Some(slot) = slot_{pos} {{ match &mut row[slot] {{ Some(value) => field_s{i}_{pos}(value)?, None => omitted(NativeModel::S{i}, {pos})? }} }}\n"));
                    } else {
                        bulk.push_str(&format!("if let Some(slot) = slot_{pos}.filter(|slot| *slot > 0) {{ field_s{i}_{pos}(&mut row[slot])?; }}\n"));
                    }
                }
                if !spec.records.is_empty() { bulk.push_str(&format!("record_s{i}(map, row.as_slice())?;\n")); }
                bulk.push_str("}\n");
            }
            bulk.push_str("Ok(()) },\n");
        }
        insert_requirements.push_str("Ok(()) },\n");
        let mut positions = vec![];
        let mut results = vec![];
        for computed in &spec.computed {
            if model.fields.iter().any(|f| f.name == computed.field) { return Err(format!("{}.{}: computed field collides with a field", spec.model, computed.field)); }
            if model.relations.iter().any(|r| r.name == computed.field) { return Err(format!("{}.{}: computed field collides with a relation", spec.model, computed.field)); }
            if spec.computed.iter().any(|other| other.field == computed.dependency) {
                return Err(format!("{}.{}: nested/cyclic native computed dependencies require an expanded specialization; rebuild with direct stored dependencies", spec.model, computed.field));
            }
            let source = model.fields.iter().find(|f| f.name == computed.dependency).ok_or_else(|| format!("missing computed dependency {}", computed.dependency))?;
            if !matches!(source.ty, ColType::String | ColType::Text) || source.array || source.enum_name.is_some() {
                return Err(format!("{}.{}: computed proof requires a scalar string dependency", spec.model, computed.field));
            }
            let e = exports.get(computed.export.as_str()).ok_or_else(|| format!("missing computed export {}", computed.export))?;
            if e.kind != ExportKind::StringComputed || !matches!(e.input, ColType::String | ColType::Text) || !matches!(e.output, Some(ColType::String | ColType::Text)) {
                return Err(format!("wrong computed export type for {}", computed.export));
            }
            ids.push(computed.export.clone());
            results.push(orm_contracts::extension::ResultDeclaration { model: spec.model.clone(), field: computed.field.clone(), dependencies: vec![computed.dependency.clone()] });
            let pos = model.fields.len();
            let dependency_position = model.fields.iter().position(|f| f.name == computed.dependency).expect("resolved dependency");
            dependency_arms.push_str(&format!("(Self::S{i}, {pos}) => Some({dependency_position}),\n"));
            positions.push(pos);
            let mut field = source.clone();
            field.name = computed.field.clone();
            field.ty = e.output.expect("validated computed output");
            field.max_length = None; field.hints.clear();
            field.primary_key = false; field.auto_increment = false; field.unique = false; field.index = false;
            field.default = None; field.default_sql = None; field.default_now = false; field.renamed_from = None;
            model.fields.push(field);
            computation_arms.push_str(&format!(r#"(NativeModel::S{i}, {pos}) => {{
    let mut values = Vec::with_capacity(rows.len());
    for row in 0..rows.len() {{
        values.push(match rows.cell(row, column, ty)? {{
            crate::db::Cell::Text(value) => Some({path}(value).map_err(crate::db::DbError::other)?),
            crate::db::Cell::Null => None,
            _ => return Err(crate::db::DbError::other("computed dependency is not a string")),
        }});
    }}
    Ok(values)
}},
"#, path=e.rust));
        }
        core.push_str(&format!("Self::S{i} => &{positions:?},\n"));
        validated_arms.push_str(&format!("Self::S{i} => &{:?},\n", validated_positions.into_iter().collect::<Vec<_>>()));
        let mut methods = vec![];
        let mut seen_methods = std::collections::BTreeSet::new();
        for (j, method) in spec.methods.iter().enumerate() {
            crate::rust_path(&method.name)?;
            if ["__debug__", "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield"].contains(&method.name.as_str()) {
                return Err(format!("{}.{}: model method is a Python keyword", spec.model, method.name));
            }
            if method.name.contains("::") || !seen_methods.insert(&method.name) { return Err(format!("invalid or duplicate model method {}", method.name)); }
            if ["pk", "objects", "_meta", "DoesNotExist", "MultipleObjectsReturned", "update", "delete", "refresh", "toJSON", "constructor", "then", "toString"].contains(&method.name.as_str()) || model.fields.iter().any(|f| f.name == method.name) || model.relations.iter().any(|r| r.name == method.name) {
                return Err(format!("{}.{}: model method collision", spec.model, method.name));
            }
            let e = exports.get(method.export.as_str()).ok_or_else(|| format!("missing method export {}", method.export))?;
            if !matches!(e.input, ColType::String | ColType::Text) || !matches!(e.kind, ExportKind::StringValidator | ExportKind::StringTransform | ExportKind::StringComputed) {
                return Err(format!("{}.{}: model methods require a scalar string export", spec.model, method.name));
            }
            if (e.kind == ExportKind::StringValidator && e.output.is_some()) || (e.kind != ExportKind::StringValidator && !matches!(e.output, Some(ColType::String | ColType::Text))) { return Err(format!("wrong method output type for {}", method.export)); }
            let rust_type = if e.output.is_some() { "String" } else { "()" };
            let symbol = format!("orm_native_method_{i}_{j}");
            python.push_str(&format!("#[pyfunction] fn {symbol}(value: &str) -> PyResult<{rust_type}> {{ {}(value).map_err(crate::errors::query_err) }}\n", e.rust));
            registration.push_str(&format!("m.add_function(pyo3::wrap_pyfunction!({symbol}, m)?)?;\n"));
            node.push_str(&format!("#[napi(js_name = {symbol:?})] pub fn {symbol}(value: String) -> napi::Result<{rust_type}> {{ {}(&value).map_err(crate::query_err) }}\n", e.rust));
            methods.push(orm_contracts::extension::MethodDeclaration { model: spec.model.clone(), name: method.name.clone(), native_function: symbol, input: e.input, output: e.output });
            ids.push(method.export.clone());
        }
        let methods_json = serde_json::to_string(&methods).map_err(|e| e.to_string())?;
        let model_json = serde_json::to_string(model).map_err(|e| e.to_string())?;
        let enum_names: std::collections::BTreeSet<_> = model.fields.iter().filter_map(|f| f.enum_name.as_deref()).collect();
        let relevant_enums: Vec<_> = schema.enums.iter().filter(|e| enum_names.contains(e.name.as_str())).collect();
        let enums = serde_json::to_string(&relevant_enums).map_err(|e| e.to_string())?;
        let enum_names_json = serde_json::to_string(&enum_names).map_err(|e| e.to_string())?;
        let computed_names: Vec<_> = spec.computed.iter().map(|f| f.field.clone()).collect();
        let computed_names_json = serde_json::to_string(&computed_names).map_err(|e| e.to_string())?;
        let physical = orm_contracts::extension::specialization_storage(model, &schema.behavior, &computed_names)?;
        let physical_json = serde_json::to_string(&physical).map_err(|e| e.to_string())?;
        let ownership: Vec<_> = schema.behavior.field_storage.iter().filter(|f| f.model == model.name).collect();
        let owner_names: std::collections::BTreeSet<_> = physical.iter().map(|m| m.name.as_str()).collect();
        let links: Vec<_> = schema.behavior.owner_links.iter().filter(|l| owner_names.contains(l.child.as_str())).collect();
        let ownership_json = serde_json::to_string(&ownership).map_err(|e| e.to_string())?;
        let links_json = serde_json::to_string(&links).map_err(|e| e.to_string())?;
        let field_adapters: Vec<_> = schema.behavior.field_adapters.iter().filter(|f| f.model == model.name).collect();
        let file_fields: Vec<_> = schema.behavior.file_fields.iter().filter(|f| f.model == model.name).collect();
        let field_adapters_json = serde_json::to_string(&field_adapters).map_err(|e| e.to_string())?;
        let file_fields_json = serde_json::to_string(&file_fields).map_err(|e| e.to_string())?;
        let identities_json = serde_json::to_string(&schema.identities).map_err(|e| e.to_string())?;
        let generic_json = serde_json::to_string(&(&schema.behavior.generic_relations, &schema.behavior.generic_reverse)).map_err(|e| e.to_string())?;
        let versions: BTreeMap<_, _> = manifests.iter().map(|m| (&m.id, &m.version)).collect();
        let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&serde_json::json!({"model": model, "identities": schema.identities, "generic_relations": schema.behavior.generic_relations, "generic_reverse": schema.behavior.generic_reverse, "enums": relevant_enums, "physical_schema": physical, "field_storage": ownership, "owner_links": links, "field_adapters": field_adapters, "file_fields": file_fields, "dialect": schema.dialect, "configuration": spec, "extensions": versions, "composition": manifests, "sources": sources, "host_contract": orm_contracts::extension::HOST_CONTRACT})).map_err(|e| e.to_string())?));
        ids.sort(); ids.dedup();
        let specialization = Specialization { model: spec.model.clone(), fingerprint, exports: ids };
        let specialization_json = serde_json::to_string(&specialization).map_err(|e| e.to_string())?;
        let results_json = serde_json::to_string(&results).map_err(|e| e.to_string())?;
        let added_fields = serde_json::to_string(&model.fields[model.fields.len()-spec.computed.len()..]).map_err(|e| e.to_string())?;
        bind_arms.push_str(&format!(r#"{name:?} => {{
    let additions: Vec<crate::ir::FieldIr> = serde_json::from_str({added_fields:?}).expect("generated result fields");
    for field in additions {{
        if !m.fields.iter().any(|f| f.name == field.name) {{ m.fields.push(field); }}
    }}
    if serde_json::to_string(&ir.identities).map_err(|e| e.to_string())? != {identities_json:?} || serde_json::to_string(&(&ir.behavior.generic_relations, &ir.behavior.generic_reverse)).map_err(|e| e.to_string())? != {generic_json:?} {{
        return Err(format!("{{}}: stale native identity/generic routing specialization; rebuild", m.name));
    }}
    let enum_names: std::collections::BTreeSet<String> = serde_json::from_str({enum_names_json:?}).expect("generated enum dependencies");
    let relevant_enums: Vec<_> = ir.enums.iter().filter(|e| enum_names.contains(&e.name)).collect();
    if serde_json::to_string(m).map_err(|e| e.to_string())? != {model_json:?} || serde_json::to_string(&relevant_enums).map_err(|e| e.to_string())? != {enums:?} || ir.dialect.name() != {dialect:?} {{
        return Err(format!("{{}}: stale native specialization; rebuild with the normalized schema", m.name));
    }}
    let computed_names: Vec<String> = serde_json::from_str({computed_names_json:?}).expect("generated computed names");
    let physical = crate::behavior::specialization_storage(m, &ir.behavior, &computed_names)?;
    let ownership: Vec<_> = ir.behavior.field_storage.iter().filter(|f| f.model == m.name).collect();
    let owner_names: std::collections::BTreeSet<_> = physical.iter().map(|m| m.name.as_str()).collect();
    let links: Vec<_> = ir.behavior.owner_links.iter().filter(|l| owner_names.contains(l.child.as_str())).collect();
    if serde_json::to_string(&physical).map_err(|e| e.to_string())? != {physical_json:?} || serde_json::to_string(&ownership).map_err(|e| e.to_string())? != {ownership_json:?} || serde_json::to_string(&links).map_err(|e| e.to_string())? != {links_json:?} {{
        return Err(format!("{{}}: stale native storage specialization; rebuild", m.name));
    }}
    let adapters: Vec<_> = ir.behavior.field_adapters.iter().filter(|f| f.model == m.name).collect();
    let files: Vec<_> = ir.behavior.file_fields.iter().filter(|f| f.model == m.name).collect();
    if serde_json::to_string(&adapters).map_err(|e| e.to_string())? != {field_adapters_json:?} || serde_json::to_string(&files).map_err(|e| e.to_string())? != {file_fields_json:?} {{
        return Err(format!("{{}}: stale native file adapter specialization; rebuild", m.name));
    }}
    let requirement = serde_json::from_str({specialization_json:?}).expect("generated specialization");
    if !ir.behavior.specializations.contains(&requirement) {{ ir.behavior.specializations.push(requirement); }}
    let methods: Vec<crate::behavior::MethodDeclaration> = serde_json::from_str({methods_json:?}).expect("generated model methods");
    if ir.behavior.methods.iter().any(|method| method.model == m.name && !methods.contains(method)) {{ return Err(format!("{{}}: stale native method adapter; rebuild", m.name)); }}
    for method in methods {{ if !ir.behavior.methods.contains(&method) {{ ir.behavior.methods.push(method); }} }}
    let results: Vec<crate::behavior::ResultDeclaration> = serde_json::from_str({results_json:?}).expect("generated result dependencies");
    for result in results {{ if !ir.behavior.result_fields.contains(&result) {{ ir.behavior.result_fields.push(result); }} }}
    out.push(NativeModel::S{i});
}},
"#, name=spec.model, dialect=schema.dialect.name()));
        specializations.push(specialization);
    }
    core.push_str("} } }\npub fn bind(ir: &mut SchemaIr) -> Result<Vec<NativeModel>, String> { let mut out = Vec::with_capacity(ir.models.len()); for m in &mut ir.models { match m.name.as_str() {\n");
    core.push_str(&bind_arms);
    core.push_str("_ => out.push(NativeModel::None),\n} } Ok(out) }\n");
    core.push_str(&format!("impl NativeModel {{ pub fn validated(self) -> &'static [usize] {{ match self {{ Self::None => &[], {validated_arms} }} }} }}\n"));
    core.push_str(&format!("impl NativeModel {{ pub fn has_records(self) -> bool {{ match self {{ {record_models} _ => false }} }} }}\n"));
    core.push_str(&format!("impl NativeModel {{ pub fn dependency(self, field: usize) -> Option<usize> {{ match (self, field) {{ {dependency_arms} _ => None }} }} }}\n"));
    engine.push_str(&field_helpers);
    engine.push_str(&record_helpers);
    engine.push_str(&format!("pub fn insert_values(kind: NativeModel, map: &[Option<usize>], rows: &mut [Vec<Option<Value>>], width: usize) -> Result<()> {{ match kind {{ {insert_bulk} _ => Ok(()) }} }}\npub fn update_values(kind: NativeModel, map: &[Option<usize>], rows: &mut [Vec<Value>], width: usize) -> Result<()> {{ match kind {{ {update_bulk} _ => Ok(()) }} }}\n"));
    engine.push_str("pub fn field(kind: NativeModel, position: usize, value: &mut Value) -> Result<()> { match (kind, position) {\n");
    engine.push_str(&field_arms);
    engine.push_str("_ => Ok(()),\n} }\n");
    engine.push_str(&format!("pub fn expression(kind: NativeModel, position: usize) -> Result<()> {{ match (kind, position) {{ {restricted} _ => Ok(()) }} }}\npub fn omitted(kind: NativeModel, position: usize) -> Result<()> {{ expression(kind, position) }}\npub fn upsert(kind: NativeModel) -> Result<()> {{ match kind {{ {native_models} _ => Ok(()) }} }}\npub fn insert_fields(kind: NativeModel, map: &[Option<usize>]) -> Result<()> {{ match kind {{ {insert_requirements} _ => Ok(()) }} }}\n"));
    engine.push_str(&format!("pub fn record<R: crate::behavior::Values + ?Sized>(kind: NativeModel, map: &[Option<usize>], values: &R) -> Result<()> {{ match kind {{ {record_arms} _ => Ok(()) }} }}\n"));
    engine.push_str("pub fn compute(kind: NativeModel, position: usize, rows: &dyn crate::db::RowSet, column: usize, ty: orm_core::ir::ValueType) -> crate::db::DbResult<Vec<Option<String>>> { match (kind, position) {\n");
    engine.push_str(&computation_arms);
    engine.push_str("_ => Err(crate::db::DbError::other(\"missing compiled computation\")),\n} }\n");
    python.push_str(&format!("pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {{ {registration} Ok(()) }}\n"));
    Ok(Sources { core, engine, specializations, python, node })
}
