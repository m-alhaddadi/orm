use orm_core::{dialect::{Dialect, Target}, ir::{Operation, ValueType}, schema::Schema};
use orm_engine::{plan::unique_row_update, Params, Result};
use sea_query::Value;
use serde_json::json;

struct Ints(Vec<Option<i32>>);
impl Params for Ints {
    fn len(&self) -> usize { self.0.len() }
    fn value(&self, i: usize, _: Option<ValueType>) -> Result<Value> { Ok(Value::Int(self.0[i])) }
    fn text(&self, _: usize) -> Result<String> { unreachable!() }
    fn count(&self, _: usize) -> Result<u64> { unreachable!() }
}

#[test]
fn only_a_non_null_unique_equality_pins_one_row() {
    let schema = Schema::from_ir(serde_json::from_value(json!({"models":[{"name":"Report","table":"reports","fields":[
        {"name":"id","column":"id","type":"int","primary_key":true},
        {"name":"code","column":"code","type":"int","unique":true},
        {"name":"size","column":"size","type":"int"}
    ]}]})).unwrap()).unwrap();
    let eq = |name: &str, i: usize| json!({"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":name},"r":{"t":"param","i":i}});
    let check = |filters: serde_json::Value, params: Vec<Option<i32>>| {
        let op: Operation = serde_json::from_value(json!({"op":"update","model":"Report","filters":filters,"set":[{"field":"size","value":{"t":"param","i":0}}]})).unwrap();
        unique_row_update(&schema, Target::new(Dialect::Sqlite), &op, &Ints(params))
    };
    assert!(check(json!([eq("id", 1)]), vec![Some(1), Some(2)]).unwrap());
    assert!(check(json!([{"t":"and","items":[eq("size", 1), eq("code", 2)]}]), vec![Some(1), Some(2), Some(3)]).unwrap());
    assert!(!check(json!([eq("id", 1)]), vec![Some(1), None]).unwrap());
    assert!(!check(json!([eq("size", 1)]), vec![Some(1), Some(2)]).unwrap());
    assert!(!check(json!([]), vec![Some(1)]).unwrap());
    assert!(check(json!([eq("missing", 1)]), vec![Some(1), Some(2)]).is_err());
}
