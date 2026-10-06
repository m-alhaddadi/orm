use orm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use orm_engine::{params::NoParams, plan::{Output, Plan, Planner}};
use serde_json::json;

fn schema(defaults: bool) -> serde_json::Value {
    let mut schema = json!({"models":[{"name":"Selected","table":"selected","fields":[
        {"name":"value","column":"value","type":"text"},
        {"name":"id","column":"id","type":"int","primary_key":true},
        {"name":"active","column":"active","type":"bool"}
    ]}]});
    if defaults { schema["behavior"] = json!({"schema_contract":1,"query_defaults":[{"model":"Selected","filter":{"t":"col","path":[],"name":"active"},"fields":["value"]}]}); }
    schema
}

#[test]
fn explicit_shape_retains_private_identity() {
    let schema = Schema::from_ir(serde_json::from_value(schema(false)).unwrap()).unwrap();
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        let operation: Operation = serde_json::from_value(json!({"op":"select","model":"Selected","model_fields":["value"]})).unwrap();
        let Plan::Select(plan) = Planner::plan(&schema, Target::new(dialect), &operation, &NoParams).unwrap() else { panic!("select") };
        let Output::Instances { shape: Some(shape), .. } = plan.output else { panic!("partial shape") };
        assert_eq!(shape.fields.len(), 2);
        assert!(shape.fields[0].public); assert!(!shape.fields[1].public);
        assert_eq!(shape.fields[1].field.position, 1); assert_eq!(shape.fields[1].physical, Some(1));
    }
}

#[test]
fn duplicate_or_unknown_selection_rejected() {
    let schema = Schema::from_ir(serde_json::from_value(schema(false)).unwrap()).unwrap();
    for fields in [vec!["value", "value"], vec!["missing"]] {
        let operation = serde_json::from_value(json!({"op":"select","model":"Selected","model_fields":fields})).unwrap();
        assert!(Planner::plan(&schema, Target::new(Dialect::Sqlite), &operation, &NoParams).is_err());
    }
}

#[cfg(not(feature = "query-defaults"))]
#[test]
fn disabled_artifact_rejects_policies_at_definition() {
    let error = Schema::from_ir(serde_json::from_value(schema(true)).unwrap()).err().unwrap();
    assert!(error.contains("query-defaults") && error.contains("rebuild"));
}

#[cfg(feature = "query-defaults")]
#[test]
fn enabled_policy_shape_is_prepared() {
    let schema = Schema::from_ir(serde_json::from_value(schema(true)).unwrap()).unwrap();
    assert!(schema.models[0].query_defaults.filter.is_some());
    let operation = serde_json::from_value(json!({"op":"select","model":"Selected"})).unwrap();
    let Plan::Select(plan) = Planner::plan(&schema, Target::new(Dialect::Sqlite), &operation, &NoParams).unwrap() else { panic!("select") };
    assert!(matches!(plan.output, Output::Instances { shape: Some(_), .. }));
}

#[cfg(feature = "query-defaults")]
#[test]
fn policies_reject_bad_fields_and_recursive_loading() {
    let mut invalid = schema(true);
    invalid["behavior"]["query_defaults"][0]["fields"] = json!(["missing"]);
    assert!(Schema::from_ir(serde_json::from_value(invalid).unwrap()).is_err());
    let mut recursive = schema(true);
    recursive["models"][0]["relations"] = json!([{"name":"self_ref","kind":"one","target":"Selected","from":"id","to":"id"}]);
    recursive["behavior"]["query_defaults"][0]["related"] = json!([["self_ref"]]);
    let error = Schema::from_ir(serde_json::from_value(recursive).unwrap()).err().unwrap();
    assert!(error.contains("recursive default loading"));
}

#[cfg(feature = "query-defaults")]
#[test]
fn generated_types_distinguish_default_missing_fields_and_filtered_targets() {
    let mut raw = schema(true);
    raw["models"][0]["relations"] = json!([{"name":"target","kind":"one","target":"Selected","from":"id","to":"id","foreign_key":true}]);
    let ir = serde_json::from_value(raw.clone()).unwrap();
    let schema = Schema::from_ir(serde_json::from_value(raw).unwrap()).unwrap();
    let py = orm_core::codegen::python::generate(&ir, &schema, "selection-test").unwrap();
    assert!(py.stub.contains("f.BelongsTo[Selected | None"));
    let ts = orm_core::codegen::typescript::generate(&ir, &schema, "selection-test", "orm").unwrap();
    assert!(ts.contains("readonly active?: boolean"));
    assert!(ts.contains("readonly value: string"));
    assert!(ts.contains("Hop<\"target\", \"opt\""));
    // Select-out never changes insert requirements.
    assert!(ts.contains("  active: In<boolean>;"));
}
