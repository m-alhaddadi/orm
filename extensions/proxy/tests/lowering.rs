use orm_contracts::{extension::{ProxyField, ProxyModel}, ir::SchemaIr};
use orm_proxy::lower_specs;
use serde_json::json;

fn schema() -> SchemaIr {
    serde_json::from_value(json!({
        "enums": [{"name":"Status","db_name":"status","storage":"text","values":[
            {"name":"ACTIVE","value":"active"},{"name":"OLD","value":"old"}]}],
        "models": [
            {"name":"User","table":"users","fields":[
                {"name":"id","column":"id","type":"int","primary_key":true},
                {"name":"name","column":"name","type":"string","nullable":true},
                {"name":"status","column":"status","type":"string","enum":"Status","default":"old"}]},
            {"name":"Active","table":"active","fields":[]},
            {"name":"Named","table":"named","fields":[]},
            {"name":"Post","table":"posts","fields":[
                {"name":"id","column":"id","type":"int","primary_key":true},
                {"name":"user_id","column":"user_id","type":"int"}],
                "relations":[{"name":"user","kind":"one","target":"Named","from":"user_id","to":"id","foreign_key":true}]}
        ]
    })).unwrap()
}
fn spec(model: &str, parent: &str) -> ProxyModel {
    ProxyModel { model: model.into(), parent: parent.into(), storage_owner: String::new(), fields: vec![], defaults: Default::default() }
}

#[test]
fn chains_preserve_physical_constraints_enum_representation_and_relation_identity() {
    let mut ir = schema();
    let mut active = spec("Active", "User");
    active.fields.push(ProxyField { field: "status".into(), non_null: true, subset: Some(vec!["ACTIVE".into()]) });
    active.defaults.insert("status".into(), json!("active"));
    let mut named = spec("Named", "Active");
    named.fields.push(ProxyField { field: "name".into(), non_null: true, subset: None });
    lower_specs(&mut ir, &[named, active]).unwrap();
    assert_eq!(ir.models[2].table, "users");
    assert_eq!(ir.models[2].fields[2].enum_name.as_deref(), Some("Status"));
    assert_eq!(ir.enums.len(), 1);
    assert_eq!(ir.models[2].fields[2].default, Some(json!("old")));
    assert!(!ir.models[2].fields[1].nullable);
    assert_eq!(ir.behavior.proxy_models[1].defaults["status"], json!("active"));
    assert_eq!(ir.behavior.proxy_models[1].fields.len(), 2);
    assert_eq!(ir.models[3].relations[0].target, "Named");
    let storage = ir.behavior.storage.as_ref().unwrap();
    assert_eq!(storage.models.len(), 2);
    assert!(storage.models[0].fields[1].nullable);
    assert_eq!(storage.models[1].relations[0].target, "User");
    // Parent enum remains complete; there is no inferred filter contribution.
    assert_eq!(ir.enums[0].values.len(), 2);
    assert_eq!(ir.behavior.declarations.len(), 0);
}

#[test]
fn proxy_only_edits_have_identical_postgres_and_sqlite_migrations() {
    for dialect in ["postgres", "sqlite"] {
        let mut a = schema();
        a.dialect = if dialect == "sqlite" { orm_core::dialect::Dialect::Sqlite } else { orm_core::dialect::Dialect::Postgres };
        lower_specs(&mut a, &[spec("Active", "User"), spec("Named", "Active")]).unwrap();
        let mut b = schema();
        b.dialect = a.dialect;
        let mut active = spec("Active", "User");
        active.defaults.insert("name".into(), json!("new default"));
        active.fields.push(ProxyField { field: "name".into(), non_null: true, subset: None });
        lower_specs(&mut b, &[active, spec("Named", "Active")]).unwrap();
        let physical = |ir: SchemaIr| {
            let storage = ir.behavior.storage.unwrap();
            orm_core::schema::Schema::from_ir(serde_json::from_value(json!({
                "models":storage.models,"enums":ir.enums,"dialect":ir.dialect
            })).unwrap()).unwrap()
        };
        let a = physical(a); let b = physical(b);
        assert_eq!(orm_core::migrate::create_all(&a).unwrap(), orm_core::migrate::create_all(&b).unwrap());
        assert!(orm_core::migrate::plan(&b, &orm_core::migrate::snapshot(&a).unwrap()).unwrap().up.is_empty());
    }
}

#[test]
fn invalid_chains_overrides_defaults_and_subsets_fail_at_preparation() {
    let cases = [
        (vec![spec("Active", "Named"), spec("Named", "Active")], "cyclic"),
        (vec![spec("Active", "Missing")], "unknown proxy source"),
        (vec![spec("Active", "User"), spec("Active", "User")], "duplicate"),
    ];
    for (specs, expected) in cases { assert!(lower_specs(&mut schema(), &specs).unwrap_err().contains(expected)); }
    for (field, members) in [("name", vec!["ACTIVE"]), ("status", vec!["MISSING"]), ("status", vec![]), ("status", vec!["ACTIVE", "ACTIVE"])] {
        let mut p = spec("Active", "User");
        p.fields.push(ProxyField { field: field.into(), non_null: true, subset: Some(members.into_iter().map(str::to_owned).collect()) });
        assert!(lower_specs(&mut schema(), &[p]).is_err());
    }
    for (field, value) in [("missing", json!("x")), ("id", json!("wrong")), ("status", json!("missing"))] {
        let mut p = spec("Active", "User"); p.defaults.insert(field.into(), value);
        assert!(lower_specs(&mut schema(), &[p]).is_err());
    }
    let mut ir = schema();
    ir.models[1].fields = ir.models[0].fields.clone();
    ir.models[1].fields[0].primary_key = false;
    assert!(lower_specs(&mut ir, &[spec("Active", "User")]).unwrap_err().contains("physical"));
}

#[test]
fn explicit_defaults_replace_inherited_and_shape_violations_remain_allowed() {
    let mut ir = schema();
    let mut active = spec("Active", "User"); active.defaults.insert("name".into(), json!("parent"));
    let mut named = spec("Named", "Active");
    named.fields.push(ProxyField { field:"name".into(),non_null:true,subset:None });
    named.defaults.insert("name".into(), json!(null));
    lower_specs(&mut ir, &[named, active]).unwrap();
    assert_eq!(ir.behavior.proxy_models[1].defaults["name"], json!(null));
    let serialized = serde_json::to_value(&ir).unwrap();
    let roundtrip: SchemaIr = serde_json::from_value(serialized).unwrap();
    assert_eq!(roundtrip.behavior.proxy_models[1].defaults["name"], json!(null));
}
#[test]
fn logical_nullable_broadening_preserves_physical_not_null() {
    let mut ir = schema();
    ir.models[0].fields[1].nullable = false;
    let mut declaration = ir.models[0].fields[1].clone();
    declaration.nullable = true;
    ir.models[1].fields.push(declaration);
    lower_specs(&mut ir, &[spec("Active", "User")]).unwrap();
    assert!(ir.models[1].fields[1].nullable);
    assert!(!ir.behavior.storage.as_ref().unwrap().models[0].fields[1].nullable);
}
