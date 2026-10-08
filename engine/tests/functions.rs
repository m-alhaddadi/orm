//! String functions, concatenation, array element access and `unnest` in the planner.
use orm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use orm_engine::{db, params::NoParams, plan::{Plan, Planner}};
use serde_json::{json, Value};

fn schema(dialect: &str) -> Schema {
    let mut fields = vec![
        json!({"name":"id","column":"id","type":"big_int","primary_key":true}),
        json!({"name":"title","column":"title","type":"text"}),
        json!({"name":"views","column":"views","type":"int"}),
    ];
    if dialect == "postgres" {
        fields.push(json!({"name":"links","column":"links","type":"text","array":true}));
        fields.push(json!({"name":"code","column":"code","type":"string","db_type":"char(4)"}));
        fields.push(json!({"name":"codes","column":"codes","type":"string","array":true,"db_type":"char(4)"}));
    }
    Schema::from_ir(serde_json::from_value(json!({"dialect": dialect, "models":[{"name":"Note","table":"notes","fields":fields}]})).unwrap()).unwrap()
}

fn col(name: &str) -> Value { json!({"t":"col","path":[],"name":name}) }
fn text(value: &str) -> Value { json!({"t":"text","value":value}) }
fn func(name: &str, args: Vec<Value>) -> Value { json!({"t":"func","name":name,"args":args}) }

fn plan(dialect: Dialect, columns: Vec<Value>, filters: Vec<Value>) -> Result<String, String> {
    let schema = schema(if dialect == Dialect::Sqlite { "sqlite" } else { "postgres" });
    let columns = columns.into_iter().map(|e| json!({"t":"expr","expr":e})).collect::<Vec<_>>();
    let op: Operation = serde_json::from_value(json!({"op":"select","model":"Note","columns":columns,"filters":filters})).unwrap();
    match Planner::plan(&schema, Target::new(dialect), &op, &NoParams).map_err(|e| e.to_string())? {
        Plan::Select(p) => Ok(db::build(dialect, &p.stmt).0),
        _ => panic!("select"),
    }
}

#[test]
fn string_functions_and_concatenation() {
    let columns = || vec![
        func("concat", vec![col("title"), text(" by "), col("views")]),
        json!({"t":"arith","op":"concat","l":col("title"),"r":text("!")}),
        func("trim", vec![col("title")]), func("ltrim", vec![col("title")]), func("rtrim", vec![col("title")]),
        func("replace", vec![col("title"), text("a"), text("b")]),
        func("substr", vec![col("title"), json!({"t":"int","value":2}), json!({"t":"int","value":3})]),
        func("strpos", vec![col("title"), text("x")]),
    ];
    assert_eq!(plan(Dialect::Postgres, columns(), vec![]).unwrap(),
        r#"SELECT CONCAT("notes"."title", $1, "notes"."views"), "notes"."title" || $2, TRIM("notes"."title"), LTRIM("notes"."title"), RTRIM("notes"."title"), REPLACE("notes"."title", $3, $4), SUBSTR("notes"."title", 2, 3), STRPOS("notes"."title", $5) FROM "notes""#);
    let sqlite = plan(Dialect::Sqlite, columns(), vec![]).unwrap();
    assert!(sqlite.contains(r#"CONCAT("notes"."title", ?, "notes"."views"), "notes"."title" || ?"#), "{sqlite}");
    assert!(sqlite.contains(r#"INSTR("notes"."title", ?)"#), "{sqlite}");
    assert!(plan(Dialect::Postgres, vec![func("concat", vec![])], vec![]).unwrap_err().contains("wrong arguments"));
}

#[test]
fn array_element_and_unnest() {
    let element = func("element", vec![col("links"), json!({"t":"int","value":1})]);
    let unnest = func("unnest", vec![col("links")]);
    assert_eq!(plan(Dialect::Postgres, vec![element.clone(), unnest.clone()], vec![]).unwrap(),
        r#"SELECT ("notes"."links")[1], UNNEST("notes"."links") FROM "notes""#);
    let filter = json!({"t":"cmp","op":"eq","l":unnest,"r":text("x")});
    assert!(plan(Dialect::Postgres, vec![col("id")], vec![filter]).unwrap_err().contains("only be a select() column"));
    let scalar = func("element", vec![col("title"), json!({"t":"int","value":1})]);
    assert!(plan(Dialect::Postgres, vec![scalar], vec![]).unwrap_err().contains("an index needs an array"));
    for (name, args) in [("element", vec![col("title"), json!({"t":"int","value":1})]), ("unnest", vec![col("title")])] {
        let error = plan(Dialect::Sqlite, vec![func(name, args)], vec![]).unwrap_err();
        assert!(error.contains("sqlite does not support"), "{error}");
    }
}

#[test]
fn unnest_is_not_planned_inside_an_aggregate_coalesce_or_window() {
    let unnest = func("unnest", vec![col("links")]);
    let window = json!({"t":"window","func":func("lag",vec![col("title")]),"partition_by":[unnest.clone()]});
    for e in [func("count", vec![unnest.clone()]), func("coalesce", vec![unnest.clone(), text("z")]), window] {
        let error = plan(Dialect::Postgres, vec![e], vec![]).unwrap_err();
        assert!(error.contains("only be a select() column"), "{error}");
    }
    assert_eq!(plan(Dialect::Postgres, vec![func("lower", vec![unnest.clone()])], vec![]).unwrap(), r#"SELECT LOWER(UNNEST("notes"."links")) FROM "notes""#);
    // The ban ends with the aggregate, `COALESCE` or window: a later argument may unnest.
    let lag = json!({"t":"window","func":func("lag",vec![col("title")])});
    for first in [func("coalesce", vec![col("title"), text("z")]), lag] {
        assert!(plan(Dialect::Postgres, vec![func("concat", vec![first, unnest.clone()])], vec![]).is_ok());
    }
}

#[test]
fn a_char_n_parameter_compares_as_bpchar() {
    let eq = |name: &str| json!({"t":"cmp","op":"eq","l":col(name),"r":text("ab")});
    let sql = plan(Dialect::Postgres, vec![col("id")], vec![eq("code")]).unwrap();
    assert!(sql.ends_with(r#"WHERE "notes"."code" = (CAST($1 AS bpchar))"#), "{sql}");
    let sql = plan(Dialect::Postgres, vec![col("id")], vec![eq("codes")]).unwrap();
    assert!(!sql.contains("bpchar"), "{sql}");
}

#[test]
fn substr_takes_a_start_from_one_and_a_length_from_zero() {
    let substr = |args: &[i64]| func("substr", std::iter::once(col("title")).chain(args.iter().map(|v| json!({"t":"int","value":v}))).collect());
    for dialect in [Dialect::Postgres, Dialect::Sqlite] {
        for args in [&[0][..], &[-2], &[2, -1]] {
            let error = plan(dialect, vec![substr(args)], vec![]).unwrap_err();
            assert!(error.contains("substr() takes a start of at least 1 and a length of at least 0"), "{error}");
        }
        assert!(plan(dialect, vec![substr(&[1, 0])], vec![]).is_ok());
    }
}
