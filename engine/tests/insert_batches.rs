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
    let plans = exec::plan_inserts(&schema, target, "Item", &["name".into()], rows, None, &NoParams, batch_size, true).unwrap();
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
    let err = exec::plan_inserts(&schema, Target::new(Dialect::Postgres), "Item", &[], vec![], None, &NoParams, Some(0), true);
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

#[test]
fn a_count_insert_has_no_returning_clause() {
    let (_, schema) = dsl::check(dsl::compile(SOURCE, None).unwrap()).unwrap();
    let target = Target::new(Dialect::Postgres);
    let plan = |returning| {
        let rows = vec![vec![Some(Value::String(Some("a".into())))]];
        let plans = exec::plan_inserts(&schema, target, "Item", &["name".into()], rows, None, &NoParams, None, returning).unwrap();
        exec::statement(target, &plans[0])
    };
    assert!(!plan(false).contains("RETURNING"), "{}", plan(false));
    assert!(plan(true).contains("RETURNING"), "{}", plan(true));
    // SQLite before 3.35 has no RETURNING; a count insert does not need it.
    let old = Target::new(Dialect::Sqlite).without(&["returning".into()]).unwrap();
    let rows = vec![vec![Some(Value::String(Some("a".into())))]];
    assert!(exec::plan_inserts(&schema, old, "Item", &["name".into()], rows.clone(), None, &NoParams, None, false).is_ok());
    assert!(exec::plan_inserts(&schema, old, "Item", &["name".into()], rows, None, &NoParams, None, true).is_err());
}

/// One parameter that the statement does not use: it still counts toward the limit.
struct OneParam;

impl orm_engine::params::Params for OneParam {
    fn len(&self) -> usize {
        1
    }
    fn value(&self, _: usize, _: Option<orm_core::ir::ValueType>) -> orm_engine::error::Result<Value> {
        Ok(Value::String(Some("p".into())))
    }
    fn text(&self, _: usize) -> orm_engine::error::Result<String> {
        Ok("p".into())
    }
    fn count(&self, _: usize) -> orm_engine::error::Result<u64> {
        Ok(1)
    }
}

fn statements(schema: &orm_core::schema::Schema, target: Target, model: &str, fields: &[String], rows: Vec<Vec<Option<Value>>>, params: &dyn orm_engine::params::Params) -> Vec<usize> {
    exec::plan_inserts(schema, target, model, fields, rows, None, params, None, true)
        .unwrap()
        .iter()
        .map(|p| match p {
            Plan::Insert(s, _) => orm_engine::db::build(target.dialect, s).1.len(),
            _ => panic!("an insert plan"),
        })
        .collect()
}

#[test]
fn the_statement_parameters_count_toward_the_limit() {
    let (_, schema) = dsl::check(dsl::compile(SOURCE, None).unwrap()).unwrap();
    let target = Target::new(Dialect::Postgres).without(&["max_params=6".into()]).unwrap();
    let rows = (0..5).map(|i| vec![Some(Value::String(Some(format!("n{i}"))))]).collect();
    // (6 - 1) / 2 = 2 rows per statement.
    assert_eq!(statements(&schema, target, "Item", &["name".into()], rows, &OneParam), vec![4, 4, 2]);
}

#[test]
fn rows_without_values_go_one_per_statement() {
    let (_, schema) = dsl::check(dsl::compile("model Blank {\n id BigInt @id @default(autoincrement())\n}\n", None).unwrap()).unwrap();
    let target = Target::new(Dialect::Postgres);
    assert_eq!(statements(&schema, target, "Blank", &[], vec![vec![], vec![], vec![]], &NoParams), vec![0, 0, 0]);
}

#[test]
fn a_write_template_counts_each_placeholder() {
    let mut ir = dsl::compile(SOURCE, None).unwrap();
    let name = ir.models[0].fields.iter_mut().find(|f| f.name == "name").unwrap();
    name.write_sql = Some("concat({}, {})".into());
    let (_, schema) = dsl::check(ir).unwrap();
    let target = Target::new(Dialect::Postgres).without(&["max_params=7".into()]).unwrap();
    let rows = (0..5).map(|i| vec![Some(Value::String(Some(format!("n{i}"))))]).collect();
    // `name` binds twice and `token` once: 3 parameters per row, 2 rows per statement.
    assert_eq!(statements(&schema, target, "Item", &["name".into()], rows, &NoParams), vec![6, 6, 3]);
}
