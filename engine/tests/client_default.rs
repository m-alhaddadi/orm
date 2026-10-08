use orm_core::{dialect::{Dialect, Target}, dsl, ir::ValueType};
use orm_engine::{db::{self, Cell}, exec::{self, Conflict, Outcome}, params::NoParams};
use sea_query::Value;

const SOURCE: &str = r#"
enum Status {
 ACTIVE @map("active")
 OLD @map("old")
 @@storage(text)
}
model Item {
 id String @id @client_default(uuid7()) @db.Uuid
 token String @client_default(uuid())
 status Status @default(OLD) @client_default(ACTIVE)
 at DateTime @client_default(now())
 day DateTime @db.Date @client_default(now())
 meta Json @client_default("{\"a\": [1]}")
 n Int @client_default(3)
 note String? @client_default("note")
 @@map("client_default_items")
}
"#;

fn text(cell: Cell<'_>) -> String {
    match cell { Cell::Text(s) => s.to_owned(), Cell::Uuid(u) => u.to_string(), other => panic!("{other:?}") }
}

async fn exercise(url: &str, dialect: Dialect) {
    let source = if dialect == Dialect::Sqlite { format!("datasource db {{\n provider = \"sqlite\"\n}}\n{SOURCE}") } else { SOURCE.to_owned() };
    let (_, schema) = dsl::check(dsl::compile(&source, None).unwrap()).unwrap();
    let target = Target::new(dialect);
    let db = db::connect(url, 1).await.unwrap();
    let conn = db.begin().await.unwrap();
    for statement in orm_core::migrate::create_all(&schema).unwrap() { conn.batch(statement).await.unwrap(); }
    let types: Vec<ValueType> = schema.model(schema.model_idx("Item").unwrap()).fields().iter().map(|f| f.value_type()).collect();
    let insert = |fields: &[&str], rows: Vec<Vec<Option<Value>>>, conflict| {
        exec::plan_insert(&schema, target, "Item", &fields.iter().map(|f| f.to_string()).collect::<Vec<_>>(), rows, conflict, &NoParams).unwrap()
    };
    // Bulk insert: every omitted value gets its own client default, explicit values win.
    let explicit = "0192f7e2-0000-7000-8000-000000000001";
    let statement = insert(&["note", "status", "id"], vec![
        vec![None, None, None],
        vec![Some(Value::String(None)), Some("old".into()), Some(Value::Uuid(Some(explicit.parse().unwrap())))],
    ], None);
    let Outcome::Rows { rows, .. } = exec::run(conn.as_ref(), target, statement).await.unwrap() else { panic!() };
    let id = uuid::Uuid::parse_str(&text(rows.cell(0, 0, types[0]).unwrap())).unwrap();
    assert_eq!(id.get_version_num(), 7);
    assert_eq!(text(rows.cell(1, 0, types[0]).unwrap()), explicit);
    assert_eq!(uuid::Uuid::parse_str(&text(rows.cell(0, 1, types[1]).unwrap())).unwrap().get_version_num(), 4);
    assert_ne!(text(rows.cell(0, 1, types[1]).unwrap()), text(rows.cell(1, 1, types[1]).unwrap()));
    // The client default fills ORM inserts; the database default stays for other writers.
    assert_eq!(rows.cell(0, 2, types[2]).unwrap(), Cell::Text("active"));
    assert_eq!(rows.cell(1, 2, types[2]).unwrap(), Cell::Text("old"));
    let Cell::DateTime(at) = rows.cell(0, 3, types[3]).unwrap() else { panic!() };
    assert!((chrono::Utc::now() - at).num_seconds().abs() < 60);
    assert!(matches!(rows.cell(0, 4, types[4]).unwrap(), Cell::Date(_)));
    assert_eq!(rows.cell(0, 5, types[5]).unwrap(), Cell::Json(serde_json::json!({"a": [1]})));
    assert_eq!(rows.cell(0, 6, types[6]).unwrap(), Cell::Int(3));
    assert_eq!(rows.cell(0, 7, types[7]).unwrap(), Cell::Text("note"));
    assert_eq!(rows.cell(1, 7, types[7]).unwrap(), Cell::Null);
    // Upsert: the proposed row gets a new client default too.
    let first = text(rows.cell(1, 1, types[1]).unwrap());
    let statement = insert(&["id"], vec![vec![Some(Value::Uuid(Some(explicit.parse().unwrap())))]],
        Some(Conflict::Update { target: vec!["id".into()], filter: None, update: vec!["token".into()], set: vec![] }));
    let Outcome::Rows { rows, .. } = exec::run(conn.as_ref(), target, statement).await.unwrap() else { panic!() };
    assert_eq!(rows.len(), 1);
    assert_ne!(text(rows.cell(0, 1, types[1]).unwrap()), first);
    // Other writers get only the database default.
    conn.batch("INSERT INTO client_default_items (id, token, at, day, meta, n) VALUES ('0192f7e2-0000-7000-8000-000000000002', 't', '2026-01-01T00:00:00Z', '2026-01-01', '{}', 1)".into()).await.unwrap();
    let stored = conn.query_text("SELECT status, note FROM client_default_items WHERE token = 't'".into()).await.unwrap();
    assert_eq!(stored[0][0].as_deref(), Some("old"));
    assert_eq!(stored[0][1], None);
    conn.rollback().await.unwrap();
    db.close().await;
}

#[test]
fn sqlite_client_defaults_fill_omitted_insert_values() {
    tokio::runtime::Runtime::new().unwrap().block_on(exercise("sqlite://:memory:", Dialect::Sqlite));
}

#[test]
fn postgres_client_defaults_fill_omitted_insert_values() {
    let url = std::env::var("ORM_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://postgres:postgres@localhost/orm_test".into());
    tokio::runtime::Runtime::new().unwrap().block_on(exercise(&url, Dialect::Postgres));
}
