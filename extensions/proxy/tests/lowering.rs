use orm_contracts::{extension::{ProxyField, ProxyModel, ProxySelection}, ir::{ClientDefaultIr, FieldIr, SchemaIr}};
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
                {"name":"status","column":"status","type":"string","enum":"Status","default":"old"},
                {"name":"note","column":"note","type":"string","nullable":true},
                {"name":"code","column":"code","type":"string"},
                {"name":"team_id","column":"team_id","type":"int","nullable":true}],
                "relations":[{"name":"team","kind":"one","target":"Team","from":"team_id","to":"id","foreign_key":true}]},
            {"name":"Active","table":"active","fields":[]},
            {"name":"Named","table":"named","fields":[]},
            {"name":"Post","table":"posts","fields":[
                {"name":"id","column":"id","type":"int","primary_key":true},
                {"name":"user_id","column":"user_id","type":"int"}],
                "relations":[{"name":"user","kind":"one","target":"Named","from":"user_id","to":"id","foreign_key":true}]},
            {"name":"Team","table":"teams","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]}
        ]
    })).unwrap()
}
fn spec(model: &str, parent: &str) -> ProxyModel {
    ProxyModel { model: model.into(), parent: parent.into(), ..Default::default() }
}
fn select(model: &str, parent: &str, selection: ProxySelection) -> ProxyModel {
    ProxyModel { selection: Some(selection), ..spec(model, parent) }
}
fn names(list: &[&str]) -> Vec<String> { list.iter().map(|n| n.to_string()).collect() }
/// Redeclare a `User` field on a proxy, changed by `edit`.
fn redeclare(ir: &mut SchemaIr, proxy: &str, field: &str, edit: impl FnOnce(&mut FieldIr)) {
    let mut declared = ir.models[0].fields.iter().find(|f| f.name == field).cloned()
        .unwrap_or_else(|| FieldIr::plain(field, orm_contracts::ir::ColType::Text));
    edit(&mut declared);
    ir.models.iter_mut().find(|m| m.name == proxy).unwrap().fields.push(declared);
}
fn client_default(ir: &mut SchemaIr, proxy: &str, field: &str, value: serde_json::Value) {
    redeclare(ir, proxy, field, |f| f.client_default = Some(ClientDefaultIr::Value(value)));
}
fn field<'a>(ir: &'a SchemaIr, model: &str, name: &str) -> &'a FieldIr {
    ir.models.iter().find(|m| m.name == model).unwrap().fields.iter().find(|f| f.name == name).unwrap()
}
fn has_field(ir: &SchemaIr, model: &str, name: &str) -> bool {
    ir.models.iter().find(|m| m.name == model).unwrap().fields.iter().any(|f| f.name == name)
}

#[test]
fn chains_preserve_physical_constraints_enum_representation_and_relation_identity() {
    let mut ir = schema();
    redeclare(&mut ir, "Active", "status", |f| {
        f.enum_subset = Some(names(&["ACTIVE"]));
        f.client_default = Some(ClientDefaultIr::Value(json!("active")));
    });
    redeclare(&mut ir, "Named", "name", |f| f.nullable = false);
    lower_specs(&mut ir, &[spec("Named", "Active"), spec("Active", "User")]).unwrap();
    assert_eq!(ir.models[2].table, "users");
    assert_eq!(field(&ir, "Named", "status").enum_name.as_deref(), Some("Status"));
    assert_eq!(ir.enums.len(), 1);
    assert_eq!(field(&ir, "Named", "status").default, Some(json!("old")));
    assert!(!field(&ir, "Named", "name").nullable);
    assert_eq!(field(&ir, "Active", "status").client_default, Some(ClientDefaultIr::Value(json!("active"))));
    assert_eq!(field(&ir, "Named", "status").client_default, Some(ClientDefaultIr::Value(json!("active"))));
    assert!(field(&ir, "Active", "status").enum_subset.is_none());
    // Generated TypeScript enums are const objects, so a member type needs `typeof`.
    assert_eq!(field(&ir, "Active", "status").hints["typescript"], "typeof Status.ACTIVE");
    assert_eq!(field(&ir, "Active", "status").hints["python"], "Literal[Status.ACTIVE]");
    assert_eq!(ir.behavior.proxy_models[1].fields, [
        ProxyField { field: "status".into(), non_null: false, subset: Some(names(&["ACTIVE"])) },
        ProxyField { field: "name".into(), non_null: true, subset: None },
    ]);
    assert_eq!(ir.models[3].relations[0].target, "Named");
    let storage = ir.behavior.storage.as_ref().unwrap();
    assert_eq!(storage.models.len(), 3);
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
        redeclare(&mut b, "Active", "name", |f| { f.nullable = false; f.client_default = Some(ClientDefaultIr::Value(json!("new default"))); });
        redeclare(&mut b, "Active", "status", |f| f.enum_subset = Some(names(&["OLD"])));
        lower_specs(&mut b, &[select("Active", "User", ProxySelection::Exclude(names(&["note"]))),
            select("Named", "Active", ProxySelection::Include(names(&["id", "name", "code"])))]).unwrap();
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
fn include_and_exclude_select_inherited_fields_and_relations() {
    let mut all = schema();
    lower_specs(&mut all, &[spec("Active", "User")]).unwrap();
    assert_eq!(all.models[1].fields.len(), 6);
    assert!(all.behavior.proxy_models[0].omitted.is_empty());
    let mut ir = schema();
    lower_specs(&mut ir, &[select("Active", "User", ProxySelection::Exclude(names(&["note", "team", "team_id"])))]).unwrap();
    assert!(!has_field(&ir, "Active", "note") && !has_field(&ir, "Active", "team_id") && has_field(&ir, "Active", "name"));
    assert!(ir.models[1].relations.is_empty());
    assert_eq!(ir.behavior.proxy_models[0].omitted, names(&["note", "team", "team_id"]));
    // The physical owner keeps every column.
    assert_eq!(ir.behavior.storage.as_ref().unwrap().models[0].fields.len(), 6);
    let mut ir = schema();
    lower_specs(&mut ir, &[select("Active", "User", ProxySelection::Include(names(&["id", "name", "status", "code"])))]).unwrap();
    assert_eq!(ir.models[1].fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["id", "name", "status", "code"]);
    assert!(ir.models[1].relations.is_empty());
    // A child proxy selects from the fields of its source proxy.
    let mut ir = schema();
    lower_specs(&mut ir, &[select("Active", "User", ProxySelection::Exclude(names(&["note"]))),
        select("Named", "Active", ProxySelection::Exclude(names(&["name"])))]).unwrap();
    assert!(!has_field(&ir, "Named", "note") && !has_field(&ir, "Named", "name") && has_field(&ir, "Named", "code"));
    let error = lower_specs(&mut schema(), &[select("Active", "User", ProxySelection::Exclude(names(&["note"]))),
        select("Named", "Active", ProxySelection::Include(names(&["id", "note"])))]).unwrap_err();
    assert!(error.contains("Named: @@proxy.fields names note, which is not a field or relation of Active"), "{error}");
}

#[test]
fn omitted_fields_follow_the_key_null_and_relation_rules() {
    let cases = [
        (ProxySelection::Exclude(names(&["id"])), "Active.id: the primary key cannot be omitted"),
        (ProxySelection::Include(names(&["name"])), "Active.id: the primary key cannot be omitted"),
        (ProxySelection::Exclude(names(&["code"])), "Active.code: a NOT NULL field without a database default cannot be omitted"),
        (ProxySelection::Exclude(names(&["team_id"])), "Active.team_id: relation team uses it as its key; omit the relation too"),
        (ProxySelection::Exclude(names(&["missing"])), "Active: @@proxy.fields names missing, which is not a field or relation of User"),
        (ProxySelection::Exclude(names(&["note", "note"])), "names note twice"),
    ];
    for (selection, expected) in cases {
        let error = lower_specs(&mut schema(), &[select("Active", "User", selection)]).unwrap_err();
        assert!(error.contains(expected), "{error}\n  expected: {expected}");
    }
    // A database default makes a NOT NULL field safe to omit.
    lower_specs(&mut schema(), &[select("Active", "User", ProxySelection::Exclude(names(&["status"])))]).unwrap();
    // A relation from another model into the proxy keeps its referenced field.
    let mut ir = schema();
    ir.models[3].relations[0].to = "code".into();
    ir.models[3].relations[0].target = "Active".into();
    ir.models[0].fields[4].default = Some(json!("c"));
    let error = lower_specs(&mut ir, &[select("Active", "User", ProxySelection::Exclude(names(&["code"])))]).unwrap_err();
    assert!(error.contains("Active.code: relation Post.user references it"), "{error}");
    // A redeclared field must be in the inherited set.
    let mut ir = schema();
    redeclare(&mut ir, "Active", "note", |f| f.nullable = false);
    let error = lower_specs(&mut ir, &[select("Active", "User", ProxySelection::Exclude(names(&["note"])))]).unwrap_err();
    assert!(error.contains("Active.note: a redeclared field must be in the inherited set"), "{error}");
}

#[test]
fn invalid_chains_overrides_defaults_and_subsets_fail_at_preparation() {
    let cases = [
        (vec![spec("Active", "Named"), spec("Named", "Active")], "cyclic"),
        (vec![spec("Active", "Missing")], "unknown proxy source"),
        (vec![spec("Active", "User"), spec("Active", "User")], "duplicate"),
    ];
    for (specs, expected) in cases { assert!(lower_specs(&mut schema(), &specs).unwrap_err().contains(expected)); }
    for (name, members) in [("name", vec!["ACTIVE"]), ("status", vec!["MISSING"]), ("status", vec![]), ("status", vec!["ACTIVE", "ACTIVE"]), ("id", vec!["ACTIVE"])] {
        let mut ir = schema();
        redeclare(&mut ir, "Active", name, |f| f.enum_subset = Some(names(&members)));
        assert!(lower_specs(&mut ir, &[spec("Active", "User")]).is_err(), "{name} {members:?}");
    }
    for (name, value) in [("missing", json!("x")), ("id", json!("wrong")), ("status", json!("missing"))] {
        let mut ir = schema();
        client_default(&mut ir, "Active", name, value);
        assert!(lower_specs(&mut ir, &[spec("Active", "User")]).is_err());
    }
    let mut ir = schema();
    redeclare(&mut ir, "Active", "name", |f| f.default = Some(json!("x")));
    assert!(lower_specs(&mut ir, &[spec("Active", "User")]).unwrap_err().contains("physical"));
    let mut ir = schema();
    ir.models[1].fields = ir.models[0].fields.clone();
    ir.models[1].fields[0].primary_key = false;
    assert!(lower_specs(&mut ir, &[spec("Active", "User")]).unwrap_err().contains("physical"));
}

#[test]
fn explicit_defaults_replace_inherited_and_shape_violations_remain_allowed() {
    let mut ir = schema();
    client_default(&mut ir, "Active", "name", json!("parent"));
    redeclare(&mut ir, "Named", "name", |f| { f.nullable = false; f.client_default = Some(ClientDefaultIr::Value(json!(null))); });
    lower_specs(&mut ir, &[spec("Named", "Active"), spec("Active", "User")]).unwrap();
    assert_eq!(field(&ir, "Active", "name").client_default, Some(ClientDefaultIr::Value(json!("parent"))));
    assert_eq!(field(&ir, "Named", "name").client_default, Some(ClientDefaultIr::Value(json!(null))));
    let serialized = serde_json::to_value(&ir).unwrap();
    let roundtrip: SchemaIr = serde_json::from_value(serialized).unwrap();
    assert_eq!(roundtrip.models[2].fields[1].client_default, Some(ClientDefaultIr::Value(json!(null))));
    assert!(roundtrip.behavior.storage.unwrap().models[0].fields[1].client_default.is_none());
}
#[test]
fn logical_nullable_broadening_preserves_physical_not_null() {
    let mut ir = schema();
    ir.models[0].fields[1].nullable = false;
    redeclare(&mut ir, "Active", "name", |f| f.nullable = true);
    lower_specs(&mut ir, &[spec("Active", "User")]).unwrap();
    assert!(ir.models[1].fields[1].nullable);
    assert!(!ir.behavior.storage.as_ref().unwrap().models[0].fields[1].nullable);
}

#[test]
fn proxy_fields_declarations_reject_include_and_exclude_together() {
    let declare = |arguments: serde_json::Value| {
        let mut ir = schema();
        ir.behavior.declarations = serde_json::from_value(json!([
            {"attribute":"proxy.of","model":"Active","field":null,"arguments":{},"positional":["User"],"location":{"file":"s.prisma","line":1,"column":1}},
            {"attribute":"proxy.fields","model":"Active","field":null,"arguments":arguments,"positional":[],"location":{"file":"s.prisma","line":2,"column":3}}
        ])).unwrap();
        orm_proxy::lower(&mut ir).map(|_| ir)
    };
    let error = declare(json!({"include": ["id"], "exclude": ["note"]})).unwrap_err();
    assert_eq!(error, "s.prisma:2:3: @@proxy.fields: include and exclude together; use one");
    assert!(declare(json!({})).unwrap_err().contains("requires include: or exclude:"));
    let ir = declare(json!({"exclude": ["note"]})).unwrap();
    assert!(!has_field(&ir, "Active", "note"));
}

#[test]
fn a_proxy_cannot_protect_writes_and_a_client_default_stays_in_the_subset() {
    let mut ir = schema();
    ir.models[1].protected_write = true;
    let error = lower_specs(&mut ir, &[spec("Active", "User")]).unwrap_err();
    assert!(error.contains("Active: a proxy cannot declare @@protected_write; protect the root model User"), "{error}");
    let mut ir = schema();
    redeclare(&mut ir, "Active", "status", |f| { f.enum_subset = Some(names(&["ACTIVE"])); f.client_default = Some(ClientDefaultIr::Value(json!("old"))); });
    let error = lower_specs(&mut ir, &[spec("Active", "User")]).unwrap_err();
    assert!(error.contains("Active.status: client default is outside the enum subset Status(ACTIVE)"), "{error}");
    // A child subset applies to the client default it inherits.
    let mut ir = schema();
    client_default(&mut ir, "Active", "status", json!("active"));
    redeclare(&mut ir, "Named", "status", |f| f.enum_subset = Some(names(&["OLD"])));
    let error = lower_specs(&mut ir, &[spec("Named", "Active"), spec("Active", "User")]).unwrap_err();
    assert!(error.contains("Named.status: client default is outside the enum subset Status(OLD)"), "{error}");
}

#[test]
fn redeclared_fields_keep_inherited_shape_and_defaults() {
    // A child may leave out a field its parent proxy redeclared.
    let mut ir = schema();
    redeclare(&mut ir, "Active", "name", |f| f.nullable = false);
    lower_specs(&mut ir, &[select("Named", "Active", ProxySelection::Exclude(names(&["name"]))), spec("Active", "User")]).unwrap();
    assert!(ir.behavior.proxy_models[1].fields.iter().all(|c| c.field != "name"));
    // An enum primary key cannot take a subset.
    let mut ir = schema();
    (ir.models[0].fields[0].primary_key, ir.models[0].fields[2].primary_key) = (false, true);
    redeclare(&mut ir, "Active", "status", |f| f.enum_subset = Some(names(&["ACTIVE"])));
    assert!(lower_specs(&mut ir, &[spec("Active", "User")]).unwrap_err().contains("proxy cannot change primary key shape"));
    // A redeclaration without @client_default keeps the inherited one.
    let mut ir = schema();
    client_default(&mut ir, "Active", "name", json!("x"));
    redeclare(&mut ir, "Named", "name", |f| f.nullable = false);
    lower_specs(&mut ir, &[spec("Named", "Active"), spec("Active", "User")]).unwrap();
    assert_eq!(field(&ir, "Named", "name").client_default, Some(ClientDefaultIr::Value(json!("x"))));
    // Leaving out @default keeps the database default.
    let mut ir = schema();
    redeclare(&mut ir, "Active", "status", |f| { f.default = None; f.enum_subset = Some(names(&["OLD"])); });
    lower_specs(&mut ir, &[spec("Active", "User")]).unwrap();
    assert_eq!(field(&ir, "Active", "status").default, Some(json!("old")));
}

#[test]
fn any_database_default_lets_a_not_null_field_be_omitted() {
    let edits: [fn(&mut FieldIr); 3] = [|f| f.default_now = true, |f| f.default_sql = Some("'c'".into()), |f| f.auto_increment = true];
    for edit in edits {
        let mut ir = schema();
        edit(&mut ir.models[0].fields[4]);
        lower_specs(&mut ir, &[select("Active", "User", ProxySelection::Exclude(names(&["code"])))]).unwrap();
        assert!(!has_field(&ir, "Active", "code"));
    }
    let mut ir = schema();
    ir.behavior.declarations = serde_json::from_value(json!([
        {"attribute":"proxy.of","model":"Active","field":null,"arguments":{},"positional":["User"],"location":{"file":"s.prisma","line":1,"column":1}},
        {"attribute":"proxy.fields","model":"Active","field":null,"arguments":{"exclude":["note"]},"positional":[],"location":{"file":"s.prisma","line":2,"column":3}},
        {"attribute":"proxy.fields","model":"Active","field":null,"arguments":{"exclude":["name"]},"positional":[],"location":{"file":"s.prisma","line":3,"column":3}}
    ])).unwrap();
    assert_eq!(orm_proxy::lower(&mut ir).unwrap_err(), "s.prisma:3:3: @@proxy.fields: duplicate @@proxy.fields");
}
