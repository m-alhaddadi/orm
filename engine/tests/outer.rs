//! `outer()` through a path of to-one relations: a correlated scalar subquery.
use orm_core::{dialect::{Dialect, Target}, ir::Operation, schema::Schema};
use orm_engine::{db, params::NoParams, plan::{Plan, Planner}};
use serde_json::{json, Value};

fn schema() -> Schema {
    let id = json!({"name":"id","column":"id","type":"big_int","primary_key":true});
    Schema::from_ir(serde_json::from_value(json!({"models":[
        {"name":"User","table":"users","fields":[id, {"name":"name","column":"name","type":"text"}],
         "relations":[{"name":"posts","kind":"many","target":"Post","from":"id","to":"author_id"}]},
        {"name":"Post","table":"posts","fields":[id, {"name":"author_id","column":"author_id","type":"big_int"}],
         "relations":[{"name":"author","kind":"one","target":"User","from":"author_id","to":"id","foreign_key":true},
                      {"name":"comments","kind":"many","target":"Comment","from":"id","to":"post_id"}]},
        {"name":"Comment","table":"comments","fields":[id, {"name":"post_id","column":"post_id","type":"big_int"}, {"name":"body","column":"body","type":"text"}],
         "relations":[{"name":"post","kind":"one","target":"Post","from":"post_id","to":"id","foreign_key":true}]}
    ]})).unwrap()).unwrap()
}

fn plan(model: &str, filter: Value) -> Result<String, String> {
    let op: Operation = serde_json::from_value(json!({"op":"select","model":model,"filters":[filter]})).unwrap();
    match Planner::plan(&schema(), Target::new(Dialect::Postgres), &op, &NoParams).map_err(|e| e.to_string())? {
        Plan::Select(p) => Ok(db::build(Dialect::Postgres, &p.stmt).0),
        _ => panic!("select"),
    }
}

fn exists(model: &str, l: Value, r: Value) -> Value {
    json!({"t":"exists","select":{"model":model,"filters":[{"t":"cmp","op":"eq","l":l,"r":r}]}})
}

#[test]
fn outer_reads_a_related_row_through_to_one_relations() {
    let sql = plan("Comment", exists("User", json!({"t":"col","path":[],"name":"name"}), json!({"t":"outer","depth":1,"path":["post","author"],"name":"name"}))).unwrap();
    assert!(sql.ends_with(r#"WHERE EXISTS(SELECT $1 FROM "users" WHERE "users"."name" = (SELECT "o2"."name" FROM "posts" AS "o1" INNER JOIN "users" AS "o2" ON "o2"."id" = "o1"."author_id" WHERE "o1"."id" = "comments"."post_id"))"#), "{sql}");
    let many = plan("User", exists("Post", json!({"t":"col","path":[],"name":"id"}), json!({"t":"outer","depth":1,"path":["posts"],"name":"id"}))).unwrap_err();
    assert!(many.contains("needs a to-one relation"), "{many}");
}
