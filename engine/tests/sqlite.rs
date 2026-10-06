#![cfg(feature = "sqlite")]
use std::sync::Arc;
use orm_core::{dsl, migrate as ddl};
use orm_engine::{db::{self, Executor, ErrorKind}, exec};

fn runtime() -> tokio::runtime::Runtime { tokio::runtime::Runtime::new().unwrap() }

#[test]
fn worker_transactions_and_integrity() {
    runtime().block_on(async {
        let db = db::connect("sqlite://:memory:", 4).await.unwrap();
        db.batch("CREATE TABLE probe (id INTEGER PRIMARY KEY, name TEXT UNIQUE)".into()).await.unwrap();
        let tx = db.begin().await.unwrap();
        tx.execute("INSERT INTO probe VALUES (?, ?)".into(), vec![1i64.into(), "outer".into()]).await.unwrap();
        let nested = tx.begin().await.unwrap();
        nested.execute("INSERT INTO probe VALUES (?, ?)".into(), vec![2i64.into(), "inner".into()]).await.unwrap();
        nested.rollback().await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(db.query("SELECT count(*) FROM probe".into(), vec![]).await.unwrap().get_i64(0,0).unwrap(), 1);
        assert!(tx.batch("SELECT 1".into()).await.is_err());
        let tx = db.begin().await.unwrap();
        tx.batch("INSERT INTO probe VALUES (3, 'abandoned')".into()).await.unwrap();
        drop(tx);
        assert_eq!(db.query("SELECT count(*) FROM probe".into(), vec![]).await.unwrap().get_i64(0,0).unwrap(), 1);
        let error = db.batch("INSERT INTO probe VALUES (4, 'outer')".into()).await.err().unwrap();
        assert_eq!(error.kind, ErrorKind::Integrity);
        db.close().await;
        assert!(db.batch("SELECT 1".into()).await.is_err());
    });
}

#[test]
fn rebuilds_preserve_cascading_relations_and_autoincrement() {
    runtime().block_on(async {
        let source = include_str!("../../examples/sqlite/schema.prisma");
        let schema = |source: &str| dsl::check(dsl::compile(source, None).unwrap()).unwrap().1;
        let first = schema(source);
        let second = schema(&source.replace("  title     String", "  name      String @renamed_from(\"title\")").replace("@@index([title]", "@@index([name]"));
        let db = db::connect("sqlite://:memory:", 1).await.unwrap();
        let conn: Arc<dyn Executor> = db.clone();
        exec::run_schema_script(conn, ddl::create_all(&first).unwrap()).await.unwrap();
        db.batch("INSERT INTO author (email, name) VALUES ('a', 'A'); INSERT INTO book (author_id, title) VALUES (1, 'kept'); INSERT INTO author (id, email, name) VALUES (100, 'gone', 'gone'); DELETE FROM author WHERE id=100".into()).await.unwrap();
        let plan = ddl::plan(&second, &ddl::snapshot(&first).unwrap()).unwrap();
        let tx = db.begin_migration().await.unwrap();
        for step in &plan.up { tx.batch(step.sql.clone()).await.unwrap(); }
        tx.commit().await.unwrap();
        assert_eq!(db.query_text("SELECT name FROM book".into()).await.unwrap()[0][0].as_deref(), Some("kept"));
        assert!(db.query_text("PRAGMA foreign_key_check".into()).await.unwrap().is_empty());
        db.batch("INSERT INTO author (email, name) VALUES ('next', 'Next')".into()).await.unwrap();
        assert_eq!(db.query("SELECT max(id) FROM author".into(), vec![]).await.unwrap().get_i64(0,0).unwrap(), 101);
        let tx = db.begin_migration().await.unwrap();
        tx.batch("DELETE FROM author WHERE id=1".into()).await.unwrap();
        assert_eq!(tx.commit().await.err().unwrap().kind, ErrorKind::Integrity);
        assert_eq!(db.query("SELECT count(*) FROM author".into(), vec![]).await.unwrap().get_i64(0,0).unwrap(), 2);
        let error = db.batch("INSERT INTO book (author_id, name) VALUES (999, 'orphan')".into()).await.err().unwrap();
        assert_eq!(error.kind, ErrorKind::Integrity);
        let conn: Arc<dyn Executor> = db.clone();
        exec::run_schema_script(conn, ddl::drop_all(&second).unwrap()).await.unwrap();
        db.close().await;
    });
}

#[test]
fn finished_nested_transaction_releases_abandoned_parent() {
    runtime().block_on(async {
        let db = db::connect("sqlite://:memory:", 1).await.unwrap();
        db.batch("CREATE TABLE probe (id INTEGER)".into()).await.unwrap();
        let tx = db.begin().await.unwrap();
        let nested = tx.begin().await.unwrap();
        nested.commit().await.unwrap();
        tx.batch("INSERT INTO probe VALUES (1)".into()).await.unwrap();
        drop(tx);
        let rows = tokio::time::timeout(std::time::Duration::from_secs(3), db.query("SELECT count(*) FROM probe".into(), vec![])).await.unwrap().unwrap();
        assert_eq!(rows.get_i64(0, 0).unwrap(), 0);
        assert!(nested.batch("SELECT 1".into()).await.is_err());
        db.close().await;
    });
}
