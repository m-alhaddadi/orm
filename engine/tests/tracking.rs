//! Column roles of the updated-at, soft-delete and optimistic-locking extensions.
use orm_core::{dialect::{Dialect, Target}, ir::{Operation, ValueType}, schema::Schema};
use orm_engine::{exec, plan::Planner, Params, Result};
use sea_query::Value;
use serde_json::{json, Value as Json};

struct Ints(Vec<i32>);
impl Params for Ints {
    fn len(&self) -> usize { self.0.len() }
    fn value(&self, i: usize, _: Option<ValueType>) -> Result<Value> { Ok(Value::Int(Some(self.0[i]))) }
    fn text(&self, _: usize) -> Result<String> { unreachable!() }
    fn count(&self, _: usize) -> Result<u64> { unreachable!() }
}

fn schema(behavior: Json) -> std::result::Result<Schema, String> {
    let mut behavior = behavior;
    behavior["schema_contract"] = json!(1);
    Schema::from_ir(serde_json::from_value(json!({"models":[{"name":"Post","table":"posts","fields":[
        {"name":"id","column":"id","type":"int","primary_key":true},
        {"name":"views","column":"views","type":"int"},
        {"name":"changed","column":"changed_at","type":"date_time"},
        {"name":"deleted","column":"deleted_at","type":"date_time","nullable":true},
        {"name":"version","column":"version","type":"int"}
    ]}],"behavior":behavior})).unwrap())
}

fn sql(schema: &Schema, dialect: Dialect, op: Json) -> String {
    let op: Operation = serde_json::from_value(op).unwrap();
    let target = Target::new(dialect);
    exec::statement(target, &Planner::plan(schema, target, &op, &Ints(vec![7])).unwrap())
}

fn update(set: &str) -> Json {
    json!({"op":"update","model":"Post","set":[{"field":set,"value":{"t":"param","i":0}}],
        "filters":[{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"param","i":0}}]})
}

#[test]
fn a_disabled_artifact_rejects_each_role_at_definition() {
    for (key, value, feature) in [
        ("updated_at", json!([{"model":"Post","field":"changed"}]), "updated-at"),
        ("soft_delete", json!([{"model":"Post","field":"deleted"}]), "soft-delete"),
        ("versions", json!([{"model":"Post","field":"version"}]), "optimistic-locking"),
    ] {
        let enabled = match feature { "updated-at" => cfg!(feature = "updated-at"), "soft-delete" => cfg!(feature = "soft-delete"), _ => cfg!(feature = "optimistic-locking") };
        let result = schema(json!({key: value}));
        if enabled { assert!(result.is_ok()); } else {
            let error = result.err().unwrap();
            assert!(error.contains(feature) && error.contains("rebuild"), "{error}");
        }
    }
}

#[cfg(feature = "updated-at")]
#[test]
fn an_update_sets_updated_at_unless_it_sets_the_field() {
    let schema = schema(json!({"updated_at":[{"model":"Post","field":"changed","mode":"database"}]})).unwrap();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let statement = sql(&schema, dialect, update("views"));
        assert!(statement.contains("\"views\" = ") && statement.contains("\"changed_at\" = "), "{statement}");
        let statement = sql(&schema, dialect, update("changed"));
        assert_eq!(statement.matches("\"changed_at\" = ").count(), 1, "{statement}");
    }
    let rows = vec![vec![Value::Int(Some(1)), Value::Int(Some(2))]];
    let (_, plan) = exec::plan_update_many(&schema, Target::new(Dialect::Postgres), "Post", &["id".into(), "views".into()], rows, &[], &Ints(vec![]), false, None, false).unwrap();
    assert!(plan.statements[0].0.contains("\"changed_at\" = "), "{}", plan.statements[0].0);
}

#[cfg(feature = "soft-delete")]
#[test]
fn a_delete_of_a_soft_delete_model_is_an_update_of_live_rows() {
    let schema = schema(json!({"soft_delete":[{"model":"Post","field":"deleted"}]})).unwrap();
    let filter = json!([{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"param","i":0}}]);
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let statement = sql(&schema, dialect, json!({"op":"delete","model":"Post","filters":filter,"returning":true}));
        assert!(statement.starts_with("UPDATE \"posts\" SET \"deleted_at\" = "), "{statement}");
        assert!(statement.contains("AND \"posts\".\"deleted_at\" IS NULL RETURNING"), "{statement}");
        let statement = sql(&schema, dialect, json!({"op":"delete","model":"Post","filters":filter,"hard":true}));
        assert!(statement.starts_with("DELETE FROM \"posts\""), "{statement}");
    }
}

#[cfg(all(feature = "soft-delete", feature = "updated-at", feature = "optimistic-locking"))]
#[test]
fn a_soft_delete_also_maintains_updated_at_and_the_version() {
    let schema = schema(json!({"soft_delete":[{"model":"Post","field":"deleted"}],"updated_at":[{"model":"Post","field":"changed"}],"versions":[{"model":"Post","field":"version"}]})).unwrap();
    let statement = sql(&schema, Dialect::Postgres, json!({"op":"delete","model":"Post"}));
    assert!(statement.contains("\"changed_at\" = ") && statement.contains("\"version\" = \"posts\".\"version\" + "), "{statement}");
}

#[cfg(feature = "optimistic-locking")]
#[test]
fn an_update_increments_the_version_unless_it_sets_it() {
    let schema = schema(json!({"versions":[{"model":"Post","field":"version"}]})).unwrap();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let statement = sql(&schema, dialect, update("views"));
        assert!(statement.contains("\"version\" = \"posts\".\"version\" + "), "{statement}");
        let statement = sql(&schema, dialect, update("version"));
        assert_eq!(statement.matches("\"version\" = ").count(), 1, "{statement}");
    }
    let rows = vec![vec![Value::Int(Some(1)), Value::Int(Some(2))]];
    let (_, plan) = exec::plan_update_many(&schema, Target::new(Dialect::Sqlite), "Post", &["id".into(), "views".into()], rows, &[], &Ints(vec![]), false, None, false).unwrap();
    assert!(plan.statements[0].0.contains("\"version\" = \"posts\".\"version\" + "), "{}", plan.statements[0].0);
}
