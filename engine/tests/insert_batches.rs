use orm_core::{dialect::{Dialect, Target}, dsl};
use orm_engine::{exec, params::NoParams, plan::Plan};
use sea_query::Value;

const SOURCE: &str = r#"
model Item {
 id BigInt @id @default(autoincrement())
 name String
 token String @client_default(uuid())
}
"#;

fn batches(target: Target, rows: usize, batch_size: Option<usize>) -> Vec<usize> {
    let (_, schema) = dsl::check(dsl::compile(SOURCE, None).unwrap()).unwrap();
    let rows = (0..rows).map(|i| vec![Some(Value::String(Some(format!("n{i}"))))]).collect();
    let plans = exec::plan_inserts(&schema, target, "Item", &["name".into()], rows, None, &NoParams, batch_size).unwrap();
    plans
        .iter()
        .map(|p| match p {
            Plan::Insert(s, _) => orm_engine::db::build(target.dialect, s).1.len() / 2,
            _ => panic!("an insert plan"),
        })
        .collect()
}

#[test]
fn rows_split_by_the_parameter_limit_after_client_defaults() {
    // `token` is filled in by its client default: two parameters per row.
    let target = Target::new(Dialect::Postgres).without(&["max_params=5".into()]).unwrap();
    assert_eq!(batches(target, 5, None), vec![2, 2, 1]);
    assert_eq!(batches(Target::new(Dialect::Postgres), 40_000, None), vec![32_767, 7_233]);
}

#[test]
fn batch_size_caps_the_rows_per_statement() {
    assert_eq!(batches(Target::new(Dialect::Postgres), 5, Some(3)), vec![3, 2]);
    assert_eq!(batches(Target::new(Dialect::Postgres), 2, Some(3)), vec![2]);
    let (_, schema) = dsl::check(dsl::compile(SOURCE, None).unwrap()).unwrap();
    let err = exec::plan_inserts(&schema, Target::new(Dialect::Postgres), "Item", &[], vec![], None, &NoParams, Some(0));
    assert!(err.is_err());
}

#[test]
fn conflict_target_takes_a_partial_index_predicate() {
    const PARTIAL: &str = "model Doc {\n id BigInt @id @default(autoincrement())\n slug String\n deleted_at DateTime?\n}\n";
    let (_, schema) = dsl::check(dsl::compile(PARTIAL, None).unwrap()).unwrap();
    let target = Target::new(Dialect::Postgres);
    let filter = serde_json::from_value(serde_json::json!({"t": "is_null", "item": {"t": "col", "path": [], "name": "deleted_at"}, "neg": false})).unwrap();
    let conflict = exec::Conflict::Nothing { target: vec!["slug".into()], filter: Some(filter) };
    let rows = vec![vec![Some(Value::String(Some("a".into())))]];
    let plan = exec::plan_insert(&schema, target, "Doc", &["slug".into()], rows, Some(conflict), &NoParams).unwrap();
    let sql = exec::statement(target, &plan);
    assert!(sql.contains(r#"ON CONFLICT ("slug") WHERE "doc"."deleted_at" IS NULL DO NOTHING"#), "{sql}");
}
