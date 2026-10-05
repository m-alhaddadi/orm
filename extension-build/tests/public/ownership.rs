//! Minimal composition consumer: inherited reads, shared keys, owned transaction.
use orm_core::{behavior::*, schema::Schema, dialect::{Dialect, Target}};
use orm_engine::{db::{self}, exec::{self, Outcome}, ownership, plan::Planner, NoParams};
use sea_query::Value;

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
    let child = &schema.models[1];
    let shape = ResultShape { model: ModelId(1), fields: child.resolved_fields.iter().map(|f| ResultField { field:f.logical, physical:Some(f.storage.column), public:true, dependencies:vec![] }).collect() };
    for invalid in [true, false] {
        let parent_fields = [StorageId { owner:OwnerId(0), column:1 }];
        let parent_values = [Supplied::Value(WriteValue::Value(Value::from("Alice")))];
        let child_fields = [StorageId { owner:OwnerId(1), column:0 }, StorageId { owner:OwnerId(1), column:1 }];
        let child_values = [Supplied::Value(WriteValue::Returned { step:0, column:StorageId { owner:OwnerId(0),column:0 } }), Supplied::Value(WriteValue::Value(Value::from(if invalid { "invalid" } else { "employee" })))];
        let owners = [OwnerWrite { owner:OwnerId(0),fields:&parent_fields,values:&parent_values },OwnerWrite { owner:OwnerId(1),fields:&child_fields,values:&child_values }];
        let contract = WriteContract { model:ModelId(1), mode:WriteMode::Insert, owners:&owners, validation_dependencies:&[], returning:Some(&shape) };
        let plan = ownership::prepare_write(&schema,&contract).unwrap();
        let outer = db.begin().await.unwrap();
        let result = ownership::run_write(outer.as_ref(),&schema,target,plan).await;
        if invalid {
            assert!(result.is_err());
            // Failure rolled back its savepoint; the caller's transaction remains usable.
            assert_eq!(outer.query("SELECT COUNT(*) FROM extension_owner_parents".into(),vec![]).await.unwrap().get_i64(0,0).unwrap(),0);
        } else {
            let Outcome::Rows { rows,types,.. } = result.unwrap() else { panic!("missing composed result") };
            assert_eq!(rows.cell(0,2,types[2]).unwrap(),db::Cell::Text("Alice"));
            assert_eq!(rows.cell(0,1,types[1]).unwrap(),db::Cell::Text("employee"));
        }
        outer.commit().await.unwrap();
    }
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
