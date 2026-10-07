#![cfg(feature = "proxy-models")]
use orm_core::{behavior::{FieldId, ModelId, ResultField, ResultShape}, dialect::{Dialect, Target}, dsl, ir::{ColType, Operation, ValueType}, proxy::{Category, Warnings}};
use orm_engine::{db::{self, Cell}, exec::{self, Outcome}, params::NoParams, plan, proxy};
use serde_json::json;

const SOURCE: &str = r#"
enum Status {
 ACTIVE @map("active")
 OLD @map("old")
 @@storage(text)
}
model User {
 id Int @id
 name String?
 status Status @default(OLD)
 @@map("proxy_05_users")
}
model Active {
 name String @client_default("client")
 status Status(ACTIVE) @client_default(ACTIVE)
 @@proxy.of(User)
}
"#;

async fn exercise(url: &str, dialect: Dialect) {
    let source = if dialect == Dialect::Sqlite { format!("datasource db {{\n provider = \"sqlite\"\n}}\n{SOURCE}") } else { SOURCE.to_owned() };
    let (_, schema) = dsl::check(dsl::compile(&source, None).unwrap()).unwrap();
    let target = Target::new(dialect);
    let db = db::connect(url, 1).await.unwrap();
    let conn = db.begin().await.unwrap();
    for statement in orm_core::migrate::create_all(&schema).unwrap() { conn.batch(statement).await.unwrap(); }
    conn.batch("INSERT INTO proxy_05_users VALUES (1,NULL,'old'),(2,NULL,'old'),(3,'ok','active')".into()).await.unwrap();
    let compile = |op| {
        let operation: Operation = serde_json::from_value(json!({"op":op,"model":"Active","order":[{"expr":{"t":"col","path":[],"name":"id"}}]})).unwrap();
        plan::Planner::plan(&schema, target, &operation, &NoParams).unwrap()
    };
    let selected = exec::run(conn.as_ref(), target, compile("select")).await.unwrap();
    let warnings = proxy::diagnostics(&schema.proxy_models, &selected).unwrap();
    assert_eq!(warnings.len(), 2);
    assert!(warnings.iter().all(|w| w.occurrence_count == 2));
    let Outcome::Select(rows) = &selected else { panic!() };
    assert_eq!(rows.rows.len(), 3);
    assert_eq!(rows.rows.cell(0,1,ValueType::scalar(ColType::String)).unwrap(), Cell::Null);
    assert_eq!(rows.rows.cell(0,2,ValueType::scalar(ColType::String)).unwrap(), Cell::Text("old"));
    let count = exec::run(conn.as_ref(), target, compile("count")).await.unwrap();
    assert!(matches!(count, Outcome::Count(3)));
    assert!(proxy::diagnostics(&schema.proxy_models, &count).unwrap().is_empty());
    // Public subset checks must not inspect name, which is not in this output.
    let subset = conn.query("SELECT id,status FROM proxy_05_users ORDER BY id".into(), vec![]).await.unwrap();
    let active = schema.model_idx("Active").unwrap();
    let shape = ResultShape { model: ModelId(active), fields: vec![
        ResultField { field:FieldId {model:ModelId(active),position:0},physical:Some(0),public:true,dependencies:vec![] },
        ResultField { field:FieldId {model:ModelId(active),position:2},physical:Some(1),public:true,dependencies:vec![] },
    ]};
    let mut warnings = Warnings::default();
    proxy::inspect(&schema.proxy_models[active], Some(&shape), 0, None, subset.as_ref(), &mut warnings).unwrap();
    let warnings = warnings.finish();
    assert_eq!(warnings.len(), 1); assert_eq!(warnings[0].category, Category::EnumSubset);
    // Omission uses proxy client defaults, explicit values (including NULL) win.
    let insert = exec::plan_insert(&schema,target,"Active",&["id".into()],vec![vec![Some(4.into())]],None,&NoParams).unwrap();
    let inserted = exec::run(conn.as_ref(),target,insert).await.unwrap();
    let Outcome::Rows { rows, .. } = &inserted else { panic!() };
    assert_eq!(rows.cell(0,1,ValueType::scalar(ColType::String)).unwrap(),Cell::Text("client"));
    assert_eq!(rows.cell(0,2,ValueType::scalar(ColType::String)).unwrap(),Cell::Text("active"));
    assert!(proxy::diagnostics(&schema.proxy_models,&inserted).unwrap().is_empty());
    let insert = exec::plan_insert(&schema,target,"Active",&["id".into(),"name".into(),"status".into()],vec![vec![Some(5.into()),Some(sea_query::Value::String(None)),Some("old".into())]],None,&NoParams).unwrap();
    let inserted = exec::run(conn.as_ref(),target,insert).await.unwrap();
    assert_eq!(proxy::diagnostics(&schema.proxy_models,&inserted).unwrap().len(),2);
    // The root still uses the physical server default and accepts nullable name.
    let insert = exec::plan_insert(&schema,target,"User",&["id".into()],vec![vec![Some(6.into())]],None,&NoParams).unwrap();
    let inserted = exec::run(conn.as_ref(),target,insert).await.unwrap();
    let Outcome::Rows { rows, .. } = &inserted else { panic!() };
    assert_eq!(rows.cell(0,1,ValueType::scalar(ColType::String)).unwrap(),Cell::Null);
    assert_eq!(rows.cell(0,2,ValueType::scalar(ColType::String)).unwrap(),Cell::Text("old"));
    // Roll back isolated physical objects even on a shared PostgreSQL test server.
    conn.rollback().await.unwrap();
    db.close().await;
}

#[test]
fn sqlite_proxy_rows_writes_and_public_diagnostics() {
    tokio::runtime::Runtime::new().unwrap().block_on(exercise("sqlite://:memory:",Dialect::Sqlite));
}
#[test]
fn postgres_proxy_rows_writes_and_public_diagnostics() {
    let url = std::env::var("ORM_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://postgres:postgres@localhost/orm_test".into());
    tokio::runtime::Runtime::new().unwrap().block_on(exercise(&url,Dialect::Postgres));
}
