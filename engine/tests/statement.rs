use orm_core::{dialect::{Dialect, Target}, ir::{Operation, ValueType}, schema::Schema};
use orm_engine::{exec, plan::Planner, Params, Result};
use sea_query::Value;
use serde_json::json;

struct Ints(Vec<i32>);
impl Params for Ints {
    fn len(&self) -> usize { self.0.len() }
    fn value(&self, i: usize, _: Option<ValueType>) -> Result<Value> { Ok(Value::Int(Some(self.0[i]))) }
    fn text(&self, _: usize) -> Result<String> { unreachable!() }
    fn count(&self, _: usize) -> Result<u64> { unreachable!() }
}

#[test]
fn a_statement_keeps_its_placeholders_and_is_the_same_for_each_value() {
    let schema = Schema::from_ir(serde_json::from_value(json!({"models":[{"name":"Person","table":"person","fields":[
        {"name":"id","column":"id","type":"int","primary_key":true}
    ]}]})).unwrap()).unwrap();
    let op: Operation = serde_json::from_value(json!({"op":"select","model":"Person",
        "filters":[{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"id"},"r":{"t":"param","i":0}}]})).unwrap();
    for (dialect, placeholder) in [(Dialect::Sqlite, "?"), (Dialect::Postgres, "$1")] {
        let target = Target::new(dialect);
        let shape = |value: i32| exec::statement(target, &Planner::plan(&schema, target, &op, &Ints(vec![value])).unwrap());
        assert_eq!(shape(7), shape(8));
        assert!(shape(7).contains(&format!("\"id\" = {placeholder}")) && !shape(7).contains('7'), "{}", shape(7));
        assert!(exec::sql(target, &Planner::plan(&schema, target, &op, &Ints(vec![7])).unwrap()).contains("= 7"));
    }
}
