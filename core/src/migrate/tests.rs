use serde_json::{json, Value};

use super::*;
use crate::ir::SchemaIr;

fn schema(ir: Value) -> Schema {
    let ir: SchemaIr = serde_json::from_value(ir).unwrap();
    Schema::from_ir(ir).unwrap()
}

fn id() -> Value {
    json!({"name": "id", "column": "id", "type": "big_int", "primary_key": true, "auto_increment": true})
}

fn field(name: &str, ty: &str) -> Value {
    json!({"name": name, "column": name, "type": ty})
}

fn blog(extra_post_fields: Vec<Value>, post: Value) -> Value {
    let mut post_fields = vec![id(), field("author_id", "big_int"), field("title", "text")];
    post_fields.extend(extra_post_fields);
    let mut p = json!({
        "name": "Post", "table": "posts", "fields": post_fields,
        "relations": [{"name": "author", "kind": "one", "target": "User", "from": "author_id", "to": "id",
                       "foreign_key": true, "on_delete": "cascade"}],
    });
    if let (Value::Object(p), Value::Object(extra)) = (&mut p, post) {
        p.extend(extra);
    }
    json!({"models": [
        {"name": "User", "table": "users", "fields": [id(), {"name": "email", "column": "email", "type": "string",
                                                             "max_length": 254, "unique": true}]},
        p,
    ]})
}

fn sql(steps: &[Step]) -> Vec<&str> {
    steps.iter().map(|s| s.sql.as_str()).collect()
}

/// Applying `up` and generating again must give an empty migration.
fn settled(s: &Schema, plan: &MigrationPlan) {
    let again = super::plan(s, &plan.snapshot).unwrap();
    assert!(again.up.is_empty(), "{:?}", sql(&again.up));
}

#[test]
fn initial_migration_creates_everything_in_dependency_order() {
    let s = schema(blog(vec![], json!({})));
    let plan = super::plan(&s, &DbSchema::default()).unwrap();
    let up = sql(&plan.up);
    assert_eq!(up.len(), 2);
    assert!(up[0].starts_with("CREATE TABLE \"users\""));
    assert!(up[1].contains("CONSTRAINT \"posts_author_id_fkey\" FOREIGN KEY (\"author_id\") REFERENCES \"users\" (\"id\") ON DELETE CASCADE"));
    assert_eq!(sql(&plan.down), vec!["ALTER TABLE \"posts\" DROP CONSTRAINT \"posts_author_id_fkey\"", "DROP TABLE \"users\"", "DROP TABLE \"posts\""]);
    settled(&s, &plan);
}

#[test]
fn indexes_constraints_and_triggers() {
    let s = schema(blog(
        vec![field("views", "int"), json!({"name": "tags", "column": "tags", "type": "json", "nullable": true})],
        json!({
            "indexes": [
                {"columns": [{"field": "author_id"}, {"field": "views", "desc": true, "nulls": "last"}],
                 "where": "views > 0", "include": ["title"]},
                {"name": "posts_title_trgm", "columns": [{"field": "title", "opclass": "gin_trgm_ops"}], "method": "gin"},
                {"columns": [{"expr": "lower(title)"}], "unique": true},
            ],
            "constraints": [
                {"kind": "check", "name": "views_positive", "expr": "views >= 0"},
                {"kind": "unique", "fields": ["author_id", "title"], "nulls_not_distinct": true},
            ],
            "triggers": [{"name": "touch", "timing": "before", "events": ["update", "insert"],
                          "update_of": ["title"], "body": "BEGIN NEW.views := 0; RETURN NEW; END;"}],
        }),
    ));
    let plan = super::plan(&s, &DbSchema::default()).unwrap();
    let up = sql(&plan.up);
    assert_eq!(up[0], "CREATE EXTENSION IF NOT EXISTS \"pg_trgm\"");
    assert!(up[1].starts_with("CREATE OR REPLACE FUNCTION \"posts_touch\"() RETURNS trigger LANGUAGE plpgsql AS $orm$"));
    let post = up.iter().find(|s| s.starts_with("CREATE TABLE \"posts\"")).unwrap();
    assert!(post.contains("CONSTRAINT \"views_positive\" CHECK (views >= 0)"));
    assert!(post.contains("CONSTRAINT \"posts_author_id_title_key\" UNIQUE NULLS NOT DISTINCT (\"author_id\", \"title\")"));
    assert!(post.contains("\"tags\" jsonb,"));
    assert!(up.contains(&"CREATE INDEX \"posts_author_id_views_idx\" ON \"posts\" (\"author_id\", \"views\" DESC NULLS LAST) INCLUDE (\"title\") WHERE views > 0"));
    assert!(up.contains(&"CREATE INDEX \"posts_title_trgm\" ON \"posts\" USING gin (\"title\" gin_trgm_ops)"));
    assert!(up.iter().any(|s| s.starts_with("CREATE UNIQUE INDEX \"posts_expr") && s.ends_with("((lower(title)))")));
    assert_eq!(
        *up.last().unwrap(),
        "CREATE TRIGGER \"touch\" BEFORE INSERT OR UPDATE OF \"title\" ON \"posts\" FOR EACH ROW EXECUTE FUNCTION \"posts_touch\"()"
    );
    // down: everything goes away, the extension last
    assert_eq!(*sql(&plan.down).last().unwrap(), "DROP EXTENSION IF EXISTS \"pg_trgm\"");
    assert!(plan.down.last().unwrap().warning.is_some());
    settled(&s, &plan);
}

#[test]
fn changes_are_altered_in_place() {
    let v1 = schema(blog(vec![field("views", "int")], json!({})));
    let p1 = super::plan(&v1, &DbSchema::default()).unwrap();
    let v2 = schema(blog(
        vec![
            json!({"name": "views", "column": "views", "type": "big_int", "default": 0, "check": "views >= 0"}),
            json!({"name": "slug", "column": "slug", "type": "string", "nullable": true, "index": true}),
        ],
        json!({"triggers": [{"name": "t", "timing": "after", "events": ["delete"], "for_each": "statement",
                             "function": "audit", "args": ["posts"]}]}),
    ));
    let p2 = super::plan(&v2, &p1.snapshot).unwrap();
    assert_eq!(
        sql(&p2.up),
        vec![
            "ALTER TABLE \"posts\" ADD COLUMN \"slug\" varchar",
            "ALTER TABLE \"posts\" ALTER COLUMN \"views\" TYPE bigint USING \"views\"::bigint",
            "ALTER TABLE \"posts\" ALTER COLUMN \"views\" SET DEFAULT 0",
            "ALTER TABLE \"posts\" ADD CONSTRAINT \"posts_views_check\" CHECK (views >= 0)",
            "CREATE INDEX \"posts_slug_idx\" ON \"posts\" (\"slug\")",
            "CREATE TRIGGER \"t\" AFTER DELETE ON \"posts\" FOR EACH STATEMENT EXECUTE FUNCTION \"audit\"('posts')",
        ]
    );
    assert!(p2.up[1].warning.is_some());
    assert_eq!(
        sql(&p2.down),
        vec![
            "DROP TRIGGER \"t\" ON \"posts\"",
            "ALTER TABLE \"posts\" DROP CONSTRAINT \"posts_views_check\"",
            "DROP INDEX \"posts_slug_idx\"",
            "ALTER TABLE \"posts\" ALTER COLUMN \"views\" DROP DEFAULT",
            "ALTER TABLE \"posts\" ALTER COLUMN \"views\" TYPE integer USING \"views\"::integer",
            "ALTER TABLE \"posts\" DROP COLUMN \"slug\"",
        ]
    );
    settled(&v2, &p2);
}

#[test]
fn renames_keep_data_and_rename_generated_names() {
    let v1 = schema(blog(vec![], json!({})));
    let p1 = super::plan(&v1, &DbSchema::default()).unwrap();
    let mut ir = blog(vec![], json!({}));
    ir["models"][0]["table"] = json!("members");
    ir["models"][0]["renamed_from"] = json!("users");
    ir["models"][0]["fields"][1]["column"] = json!("mail");
    ir["models"][0]["fields"][1]["renamed_from"] = json!("email");
    let v2 = schema(ir);
    let p2 = super::plan(&v2, &p1.snapshot).unwrap();
    assert_eq!(
        sql(&p2.up),
        vec![
            "ALTER TABLE \"users\" RENAME TO \"members\"",
            "ALTER TABLE \"members\" RENAME COLUMN \"email\" TO \"mail\"",
            "ALTER TABLE \"members\" RENAME CONSTRAINT \"users_pkey\" TO \"members_pkey\"",
            "ALTER TABLE \"members\" RENAME CONSTRAINT \"users_email_key\" TO \"members_mail_key\"",
        ]
    );
    assert_eq!(
        sql(&p2.down),
        vec![
            "ALTER TABLE \"members\" RENAME TO \"users\"",
            "ALTER TABLE \"users\" RENAME COLUMN \"mail\" TO \"email\"",
            "ALTER TABLE \"users\" RENAME CONSTRAINT \"members_pkey\" TO \"users_pkey\"",
            "ALTER TABLE \"users\" RENAME CONSTRAINT \"members_mail_key\" TO \"users_email_key\"",
        ]
    );
    settled(&v2, &p2);
}

/// The rename of `renames_keep_data_and_rename_generated_names`, then a later column with both hints kept.
fn kept_rename_hints(dialect: &str) -> (MigrationPlan, MigrationPlan) {
    let renamed = |extra: Vec<Value>| {
        let mut ir = blog(extra, json!({}));
        ir["dialect"] = json!(dialect);
        ir["models"][0]["table"] = json!("members");
        ir["models"][0]["renamed_from"] = json!("users");
        ir["models"][0]["fields"][1]["column"] = json!("mail");
        ir["models"][0]["fields"][1]["renamed_from"] = json!("email");
        schema(ir)
    };
    let mut v1 = blog(vec![], json!({}));
    v1["dialect"] = json!(dialect);
    let p1 = super::plan(&schema(v1), &DbSchema::default()).unwrap();
    let p2 = super::plan(&renamed(vec![]), &p1.snapshot).unwrap();
    let v3 = renamed(vec![json!({"name": "views", "column": "views", "type": "int", "default": 42})]);
    let p3 = super::plan(&v3, &p2.snapshot).unwrap();
    settled(&v3, &p3);
    (p2, p3)
}

#[test]
fn kept_rename_hints_do_nothing_after_the_rename() {
    let (_, p3) = kept_rename_hints("postgres");
    assert_eq!(sql(&p3.up), vec!["ALTER TABLE \"posts\" ADD COLUMN \"views\" integer DEFAULT 42 NOT NULL"]);
    assert_eq!(sql(&p3.down), vec!["ALTER TABLE \"posts\" DROP COLUMN \"views\""]);
}

#[test]
fn sqlite_rebuild_copies_from_the_current_name_when_the_hint_is_kept() {
    let (p2, p3) = kept_rename_hints("sqlite");
    let rename = &p2.up[0].sql;
    assert!(rename.contains("INSERT INTO \"__orm_new_members\" (\"id\", \"mail\") SELECT \"id\", \"email\" FROM \"users\""), "{rename}");
    assert!(rename.contains("WHERE name = 'users'), 0)) WHERE name = 'members'"), "{rename}");
    for later in [&p3.up[0].sql, &p3.down[0].sql] {
        assert!(later.contains("INSERT INTO \"__orm_new_members\" (\"id\", \"mail\") SELECT \"id\", \"mail\" FROM \"members\""), "{later}");
        assert!(later.contains("WHERE name = 'members'), 0)) WHERE name = 'members'"), "{later}");
    }
    let undo = &p2.down[0].sql;
    assert!(undo.contains("INSERT INTO \"__orm_new_users\" (\"id\", \"email\") SELECT \"id\", \"mail\" FROM \"members\""), "{undo}");
}

#[test]
fn sqlite_rebuild_creates_triggers_after_every_table_has_its_name() {
    let s = schema(json!({"dialect": "sqlite", "models": [
        {"name": "A", "table": "a", "fields": [id()], "triggers": [{"name": "touch", "timing": "after", "events": ["update"], "body": "UPDATE b SET id = id"}]},
        {"name": "B", "table": "b", "fields": [id()]},
    ]}));
    let rebuild = &super::plan(&s, &DbSchema::default()).unwrap().up[0].sql;
    let (renamed, trigger) = (rebuild.rfind("ALTER TABLE \"__orm_new_b\" RENAME TO \"b\"").unwrap(), rebuild.find("CREATE TRIGGER").unwrap());
    assert!(renamed < trigger, "{rebuild}");
}

#[test]
fn foreign_key_cycles_are_added_after_both_tables() {
    let s = schema(json!({"models": [
        {"name": "A", "table": "a", "fields": [id(), field("b_id", "big_int")],
         "relations": [{"name": "b", "kind": "one", "target": "B", "from": "b_id", "to": "id", "foreign_key": true}]},
        {"name": "B", "table": "b", "fields": [id(), field("a_id", "big_int")],
         "relations": [{"name": "a", "kind": "one", "target": "A", "from": "a_id", "to": "id", "foreign_key": true}]},
    ]}));
    let plan = super::plan(&s, &DbSchema::default()).unwrap();
    let up = sql(&plan.up);
    assert_eq!(up.len(), 3);
    assert!(up[2].starts_with("ALTER TABLE") && up[2].contains("FOREIGN KEY"));
    settled(&s, &plan);
}

#[test]
fn extension_types_and_functions_pull_in_their_extension() {
    let s = schema(json!({
        "models": [{"name": "Doc", "table": "docs", "fields": [
            {"name": "id", "column": "id", "type": "uuid", "primary_key": true, "default_sql": "uuid_generate_v4()"},
            {"name": "email", "column": "email", "type": "text", "db_type": "citext"},
            {"name": "embedding", "column": "embedding", "type": "json", "db_type": "vector(3)"},
        ], "indexes": [{"columns": [{"field": "embedding", "opclass": "vector_cosine_ops"}], "method": "hnsw",
                        "with": [["m", "16"]]}],
           "constraints": [{"kind": "exclude", "elements": [{"field": "email", "operator": "="}], "requires": ["btree_gist"]}]}],
        "extensions": [{"name": "citext", "schema": "public"}],
    }));
    let plan = super::plan(&s, &DbSchema::default()).unwrap();
    let up = sql(&plan.up);
    assert_eq!(
        &up[..4],
        &[
            "CREATE EXTENSION IF NOT EXISTS \"btree_gist\"",
            "CREATE EXTENSION IF NOT EXISTS \"citext\" SCHEMA \"public\"",
            "CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\"",
            "CREATE EXTENSION IF NOT EXISTS \"vector\"",
        ]
    );
    assert!(up[4].contains("\"id\" uuid DEFAULT uuid_generate_v4() NOT NULL"));
    assert!(up[4].contains("CONSTRAINT \"docs_email_excl\" EXCLUDE USING gist (\"email\" WITH =)"));
    assert_eq!(up[5], "CREATE INDEX \"docs_embedding_idx\" ON \"docs\" USING hnsw (\"embedding\" vector_cosine_ops) WITH (m = 16)");
}

#[test]
fn trigger_body_change_replaces_the_function_only() {
    let trig = |body: &str| {
        json!({"triggers": [{"name": "touch", "timing": "before", "events": ["update"], "body": body}]})
    };
    let v1 = schema(blog(vec![], trig("BEGIN RETURN NEW; END;")));
    let p1 = super::plan(&v1, &DbSchema::default()).unwrap();
    let v2 = schema(blog(vec![], trig("BEGIN NEW.title := upper(NEW.title); RETURN NEW; END;")));
    let p2 = super::plan(&v2, &p1.snapshot).unwrap();
    assert_eq!(p2.up.len(), 1);
    assert!(p2.up[0].sql.starts_with("CREATE OR REPLACE FUNCTION \"posts_touch\"()"));
    assert!(p2.down[0].sql.contains("BEGIN RETURN NEW; END;"));
}

#[test]
fn invalid_references_are_reported() {
    let ir: SchemaIr = serde_json::from_value(blog(vec![], json!({"indexes": [{"columns": [{"field": "nope"}]}]}))).unwrap();
    let s = Schema::from_ir(ir).unwrap();
    let err = super::plan(&s, &DbSchema::default()).unwrap_err();
    assert!(err.contains("nope"), "{err}");
}

#[test]
fn snapshots_round_trip() {
    let s = schema(blog(vec![], json!({"constraints": [{"kind": "check", "expr": "id > 0"}]})));
    let snap = snapshot(&s).unwrap();
    let json = serde_json::to_string(&snap).unwrap();
    assert_eq!(parse_snapshot(&json).unwrap(), snap);
    assert!(parse_snapshot(r#"{"version": 99}"#).is_err());
}

fn with_enum(values: &[&str]) -> Schema {
    let vals: Vec<Value> = values.iter().map(|v| json!({"name": v, "value": v})).collect();
    schema(json!({
        "enums": [{"name": "Status", "db_name": "status", "values": vals}],
        "models": [{"name": "Post", "table": "posts", "fields": [
            id(),
            {"name": "status", "column": "status", "type": "string", "enum": "Status", "db_type": "\"status\"",
             "default": values[0]},
            {"name": "history", "column": "history", "type": "string", "enum": "Status", "db_type": "\"status\"",
             "array": true, "nullable": true},
        ]}],
    }))
}

#[test]
fn enum_types_are_created_altered_and_dropped() {
    let v1 = with_enum(&["draft", "published"]);
    let p1 = super::plan(&v1, &DbSchema::default()).unwrap();
    let up = sql(&p1.up);
    assert_eq!(up[0], "CREATE TYPE \"status\" AS ENUM ('draft', 'published')");
    assert!(up[1].contains("\"status\" \"status\" DEFAULT 'draft' NOT NULL"), "{}", up[1]);
    assert!(up[1].contains("\"history\" \"status\"[]"), "{}", up[1]);
    assert_eq!(sql(&p1.down), vec!["DROP TABLE \"posts\"", "DROP TYPE \"status\""]);
    settled(&v1, &p1);

    // added values keep their place
    let v2 = with_enum(&["draft", "review", "published", "archived"]);
    let p2 = super::plan(&v2, &p1.snapshot).unwrap();
    assert_eq!(
        sql(&p2.up),
        vec![
            "ALTER TYPE \"status\" ADD VALUE 'review' BEFORE 'published'",
            "ALTER TYPE \"status\" ADD VALUE 'archived'",
        ]
    );
    assert!(p2.up[0].warning.as_deref().unwrap().contains("same transaction"));
    settled(&v2, &p2);

    // a removed value recreates the type and converts its columns
    let p3 = super::plan(&v1, &p2.snapshot).unwrap();
    assert_eq!(
        sql(&p3.up),
        vec![
            "ALTER TYPE \"status\" RENAME TO \"status_old\";\n\
             CREATE TYPE \"status\" AS ENUM ('draft', 'published');\n\
             ALTER TABLE \"posts\" ALTER COLUMN \"status\" DROP DEFAULT;\n\
             ALTER TABLE \"posts\" ALTER COLUMN \"status\" TYPE \"status\" USING \"status\"::text::\"status\";\n\
             ALTER TABLE \"posts\" ALTER COLUMN \"status\" SET DEFAULT 'draft';\n\
             ALTER TABLE \"posts\" ALTER COLUMN \"history\" TYPE \"status\"[] USING \"history\"::text[]::\"status\"[];\n\
             DROP TYPE \"status_old\""
        ]
    );
    assert!(p3.up[0].warning.as_deref().unwrap().contains("removes review, archived"));
    settled(&v1, &p3);

    // idempotent DDL wraps CREATE TYPE, drop_all drops it
    let all = create_all(&v1).unwrap();
    assert!(all[0].starts_with("DO $orm$\nBEGIN\n    CREATE TYPE \"status\""), "{}", all[0]);
    assert_eq!(drop_all(&v1).unwrap().last().unwrap(), "DROP TYPE IF EXISTS \"status\" CASCADE");
}
