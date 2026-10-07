//! Minimal composition consumer: inherited reads through shared keys; ordinary writes fail.
use orm_core::{schema::Schema, dialect::{Dialect, Target}};
use orm_engine::{db::{self}, exec::{self, Outcome}, plan::Planner, NoParams};

async fn check(url: &str) {
    let dialect = if url.starts_with("sqlite:") { Dialect::Sqlite } else { Dialect::Postgres };
    let target = Target::new(dialect);
    let physical = serde_json::json!([
        {"name":"OwnerParent","table":"extension_owner_parents","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true,"auto_increment":true},
            {"name":"name","column":"name","type":"string"}]},
        {"name":"OwnerChild","table":"extension_owner_children","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"role","column":"role","type":"string"}],
            "constraints":[{"kind":"check","name":"valid_role","expr":"role <> 'invalid'"}],
            "relations":[{"name":"parent","kind":"one","target":"OwnerParent","from":"id","to":"id","foreign_key":true,"on_delete":"cascade"}]}
    ]);
    let mut logical = physical.clone();
    logical[1]["fields"].as_array_mut().unwrap().push(serde_json::json!({"name":"name","column":"name","type":"string"}));
    let definition = serde_json::json!({"dialect":dialect,"models":logical,
        "behavior":{"schema_contract":1,"storage":{"models":physical},
            "field_storage":[{"model":"OwnerChild","field":"name","owner":"OwnerParent","column":"name"}],
            "owner_links":[{"child":"OwnerChild","parent":"OwnerParent","child_key":"id","parent_key":"id"}]}
    });
    let mut invalid = definition.clone();
    invalid["behavior"].as_object_mut().unwrap().remove("storage");
    assert!(Schema::from_ir(serde_json::from_value(invalid).unwrap()).err().unwrap().contains("explicit physical schema"));
    let schema = Schema::from_ir(serde_json::from_value(definition).unwrap()).unwrap();
    let db = db::connect(url, 2).await.unwrap();
    db.batch("DROP TABLE IF EXISTS extension_owner_children; DROP TABLE IF EXISTS extension_owner_parents;".into()).await.unwrap();
    for statement in orm_core::migrate::create_all(&schema).unwrap() { db.batch(statement).await.unwrap(); }
    db.batch("INSERT INTO extension_owner_parents (id, name) VALUES (42, 'Alice'); INSERT INTO extension_owner_children (id, role) VALUES (42, 'employee');".into()).await.unwrap();
    let select = orm_engine::parse_op(r#"{"op":"select","model":"OwnerChild"}"#).unwrap();
    let plan = Planner::plan(&schema,target,&select,&NoParams).unwrap();
    let sql = exec::sql(target,&plan);
    assert!(sql.contains("extension_owner_parents"));
    let Outcome::Select(result) = exec::run(db.as_ref(),target,plan).await.unwrap() else { panic!("missing composed read") };
    assert_eq!(result.rows.len(),1);
    assert_eq!(result.rows.cell(0,2,result.plan.types[2]).unwrap(),db::Cell::Text("Alice"));
    let cte = orm_engine::parse_op(r#"{"op":"select","model":"OwnerChild","from":"owned","with":[{"name":"owned","query":{"model":"OwnerChild"}}]}"#).unwrap();
    let plan = Planner::plan(&schema,target,&cte,&NoParams).unwrap();
    let Outcome::Select(result) = exec::run(db.as_ref(),target,plan).await.unwrap() else { panic!("missing CTE read") };
    assert_eq!(result.rows.cell(0,2,result.plan.types[2]).unwrap(),db::Cell::Text("Alice"));
    let filter = orm_engine::parse_op(r#"{"op":"count","model":"OwnerChild","filters":[{"t":"is_null","neg":true,"item":{"t":"col","path":[],"name":"name"}}]}"#).unwrap();
    let plan = Planner::plan(&schema,target,&filter,&NoParams).unwrap();
    let Outcome::Count(count) = exec::run(db.as_ref(),target,plan).await.unwrap() else { panic!("missing count") };
    assert_eq!(count,1);
    // Ordinary writes must fail rather than targeting a nonexistent local column.
    let update = orm_engine::parse_op(r#"{"op":"update","model":"OwnerChild","set":[]}"#).unwrap();
    assert!(Planner::plan(&schema,target,&update,&NoParams).is_err());
    db.batch("DROP TABLE extension_owner_children; DROP TABLE extension_owner_parents;".into()).await.unwrap();
    db.close().await;
}
fn main() {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        check("sqlite://:memory:").await;
        if let Ok(url) = std::env::var("ORM_TEST_DATABASE_URL") { check(&url).await; }
    });
}
