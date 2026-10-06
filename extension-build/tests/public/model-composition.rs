use orm_core::{
    dialect::{Dialect, Target},
    dsl, migrate,
    schema::Schema,
};
use orm_engine::{
    composed,
    db::{self, Cell},
    exec::{self, Outcome},
    NoParams,
};
async fn check(url: &str) {
    let dialect = if url.starts_with("sqlite:") {
        Dialect::Sqlite
    } else {
        Dialect::Postgres
    };
    let source = "datasource db { provider = \"sqlite\" }\nmodel Person {\n id Int @id @default(autoincrement())\n name String\n}\nmodel Employee {\n salary Int @check(\"salary > 0\")\n @@composition.model(parent: \"Person\", parentRef: \"person\", childRef: \"employee\")\n}\nmodel Manager {\n level Int @check(\"level > 0\")\n @@composition.model(parent: \"Employee\", parentRef: \"employee\", childRef: \"manager\")\n}";
    let source = if dialect == Dialect::Postgres {
        source.replace("\"sqlite\"", "\"postgresql\"")
    } else {
        source.into()
    };
    let source=format!("{source}\nmodel Customer {{\n points Int\n @@composition.model(parent: \"Person\", parentRef: \"person\", childRef: \"customer\")\n}}");
    let ir = dsl::compile(&source, None).unwrap();
    let encoded=serde_json::to_string(&ir).unwrap();
    let schema = Schema::from_ir(ir).unwrap();
    let ir=serde_json::from_str(&encoded).unwrap();
    let py=orm_core::codegen::python::generate(&ir,&schema,"composition.prisma").unwrap();
    let ts=orm_core::codegen::typescript::generate(&ir,&schema,"composition.prisma","orm").unwrap();
    assert!(py.stub.contains("class ManagerAttach(TypedDict):\n    level: int"));
    assert!(py.stub.contains("async def attach(self, parent_id: int, values: ManagerAttach) -> Manager"));
    assert!(ts.contains("export interface ManagerAttach {\n  level: In<number>;\n}"));
    assert!(ts.contains("readonly attach: ManagerAttach;"));
    let target = Target::new(dialect);
    let db = db::connect(url, 1).await.unwrap();
    for statement in migrate::create_all(&schema).unwrap() {
        db.batch(statement).await.unwrap();
    }
    let plan = exec::plan_insert(
        &schema,
        target,
        "Manager",
        &["name".into(), "salary".into(), "level".into()],
        vec![vec![
            Some("Alice".into()),
            Some(20i32.into()),
            Some(3i32.into()),
        ]],
        None,
        &NoParams,
    )
    .unwrap();
    let Outcome::Rows { rows, types, .. } = exec::run(db.as_ref(), target, plan).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows.cell(0, 0, types[0]).unwrap(), Cell::Int(1));
    assert_eq!(rows.cell(0, 3, types[3]).unwrap(), Cell::Text("Alice"));
    let update = orm_engine::parse_op(r#"{"op":"update","model":"Manager","filters":[{"t":"cmp","op":"eq","l":{"t":"col","path":[],"name":"salary"},"r":{"t":"int","value":20}}],"set":[{"field":"name","value":{"t":"func","name":"upper","args":[{"t":"col","path":[],"name":"name"}]}},{"field":"salary","value":{"t":"arith","op":"add","l":{"t":"col","path":[],"name":"salary"},"r":{"t":"int","value":1}}}],"returning":true}"#).unwrap();
    let plan = orm_engine::plan::Planner::plan(&schema, target, &update, &NoParams).unwrap();
    let Outcome::Rows { rows, types, .. } = exec::run(db.as_ref(), target, plan).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(rows.cell(0, 2, types[2]).unwrap(), Cell::Int(21));
    assert_eq!(rows.cell(0, 3, types[3]).unwrap(), Cell::Text("ALICE"));
    let customer = composed::prepare_attach(
        &schema,
        target,
        "Customer",
        1i32.into(),
        &["points".into()],
        vec![vec![Some(10i32.into())]],
    )
    .unwrap();
    let Outcome::Rows { rows, types, .. } = composed::run_insert(db.as_ref(), target, customer)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(rows.cell(0, 2, types[2]).unwrap(), Cell::Text("ALICE"));
    let related = orm_engine::parse_op(
        r#"{"op":"select","model":"Person","select_related":[["employee"],["customer"]]}"#,
    )
    .unwrap();
    let plan = orm_engine::plan::Planner::plan(&schema, target, &related, &NoParams).unwrap();
    let Outcome::Select(result) = exec::run(db.as_ref(), target, plan).await.unwrap() else {
        panic!()
    };
    assert_eq!(result.rows.len(), 1);
    // Attach touches no parent values and is protected by the ordinary FK/PK.
    let attach = composed::prepare_attach(
        &schema,
        target,
        "Employee",
        1i32.into(),
        &["salary".into()],
        vec![vec![Some(99i32.into())]],
    )
    .unwrap();
    assert!(composed::run_insert(db.as_ref(), target, attach)
        .await
        .is_err());
    assert!(composed::prepare_attach(
        &schema,
        target,
        "Employee",
        1i32.into(),
        &["name".into()],
        vec![vec![Some("overwrite".into())]]
    )
    .is_err());
    let bad = exec::plan_insert(
        &schema,
        target,
        "Manager",
        &["name".into(), "salary".into(), "level".into()],
        vec![vec![
            Some("rollback".into()),
            Some(22i32.into()),
            Some(0i32.into()),
        ]],
        None,
        &NoParams,
    )
    .unwrap();
    let outer = db.begin().await.unwrap();
    assert!(exec::run(outer.as_ref(), target, bad).await.is_err());
    assert_eq!(
        outer
            .query("SELECT COUNT(*) FROM person".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        1
    );
    outer.rollback().await.unwrap();
    let bad=orm_engine::parse_op(r#"{"op":"update","model":"Manager","set":[{"field":"salary","value":{"t":"int","value":30}},{"field":"level","value":{"t":"int","value":0}}]}"#).unwrap();
    let plan = orm_engine::plan::Planner::plan(&schema, target, &bad, &NoParams).unwrap();
    assert!(exec::run(db.as_ref(), target, plan).await.is_err());
    assert_eq!(
        db.query("SELECT CAST(level AS BIGINT) FROM manager".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        3
    );
    assert_eq!(
        db.query("SELECT CAST(salary AS BIGINT) FROM employee".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        21
    );
    let explicit = exec::plan_insert(
        &schema,
        target,
        "Manager",
        &["id".into(), "name".into(), "salary".into(), "level".into()],
        vec![vec![
            Some(42i32.into()),
            Some("Explicit".into()),
            Some(22i32.into()),
            Some(1i32.into()),
        ]],
        None,
        &NoParams,
    )
    .unwrap();
    let Outcome::Rows { rows, types, .. } = exec::run(db.as_ref(), target, explicit).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(rows.cell(0, 0, types[0]).unwrap(), Cell::Int(42));
    let delete =
        orm_engine::parse_op(r#"{"op":"delete","model":"Employee","returning":true}"#).unwrap();
    let plan = orm_engine::plan::Planner::plan(&schema, target, &delete, &NoParams).unwrap();
    let Outcome::Rows { rows, .. } = exec::run(db.as_ref(), target, plan).await.unwrap() else {
        panic!()
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(
        db.query("SELECT COUNT(*) FROM person".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        2
    );
    assert_eq!(
        db.query("SELECT COUNT(*) FROM manager".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        0
    );
    assert_eq!(
        db.query("SELECT COUNT(*) FROM customer".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        1
    );
    let parent_delete = orm_engine::parse_op(r#"{"op":"delete","model":"Person"}"#).unwrap();
    let plan = orm_engine::plan::Planner::plan(&schema, target, &parent_delete, &NoParams).unwrap();
    let Outcome::Affected(count) = exec::run(db.as_ref(), target, plan).await.unwrap() else {
        panic!()
    };
    assert_eq!(count, 2);
    assert_eq!(
        db.query("SELECT COUNT(*) FROM customer".into(), vec![])
            .await
            .unwrap()
            .get_i64(0, 0)
            .unwrap(),
        0
    );
    for statement in migrate::drop_all(&schema).unwrap() {
        db.batch(statement).await.unwrap();
    }
    db.close().await;
}
fn main() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        check("sqlite://:memory:").await;
        if let Ok(url) = std::env::var("ORM_TEST_DATABASE_URL") {
            check(&url).await;
        }
    });
}
