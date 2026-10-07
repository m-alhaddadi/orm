//! A proxy writes its storage owner's table, so its rows get the same file check,
//! also for a value that a proxy client default supplies.
#![cfg(all(feature = "proxy-models", feature = "file-storage"))]
use orm_core::{dialect::{Dialect, Target}, dsl};
use orm_engine::{exec, params::NoParams};

const SOURCE: &str = r#"
datasource db {
 provider = "sqlite"
}
model Report {
 id Int @id
 file Json? @storage.file(storage: "reports")
 @@map("proxy_file_reports")
}
model Draft {
 @@proxy.of(Report)
 @@proxy.default("file", "not a file reference")
}
"#;

#[test]
fn proxy_defaults_are_applied_before_the_file_check() {
    let (_, schema) = dsl::check(dsl::compile(SOURCE, None).unwrap()).unwrap();
    let target = Target::new(Dialect::Sqlite);
    let insert = |model: &str, fields: &[&str], row: Vec<sea_query::Value>| {
        let fields: Vec<String> = fields.iter().map(|f| (*f).into()).collect();
        exec::plan_insert(&schema, target, model, &fields, vec![row.into_iter().map(Some).collect()], None, &NoParams).err().map(|e| e.to_string())
    };
    let invalid = sea_query::Value::Json(Some(Box::new(serde_json::json!("not a file reference"))));
    assert_eq!(insert("Report", &["id"], vec![1.into()]), None);
    let supplied = insert("Draft", &["id", "file"], vec![1.into(), invalid]).expect("a supplied value is checked");
    let defaulted = insert("Draft", &["id"], vec![1.into()]).expect("a client default is checked");
    assert!(supplied.contains("reference") && defaulted.contains("reference"), "{supplied} / {defaulted}");
}
