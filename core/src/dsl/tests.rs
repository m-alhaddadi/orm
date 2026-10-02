use serde_json::json;

use super::*;

fn ok(src: &str) -> SchemaIr {
    compile(src, None).unwrap_or_else(|e| panic!("{e}"))
}

fn fails(src: &str) -> String {
    match compile(src, None).and_then(check) {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    }
}

#[test]
fn blog_schema_compiles_to_the_ir_the_engine_expects() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/blog/schema.prisma");
    let (ir, _) = check(compile_file(&path).unwrap()).unwrap();
    let v = serde_json::to_value(&ir).unwrap();
    let user = &v["models"][0];
    assert_eq!(user["fields"][1], json!({"name": "email", "column": "email", "type": "string", "unique": true, "max_length": 254}));
    assert_eq!(user["relations"][0], json!({"name": "posts", "kind": "many", "target": "Post", "from": "id", "to": "author_id"}));
    let post = &v["models"][1];
    assert_eq!(post["table"], "posts");
    assert_eq!(
        post["fields"][0],
        json!({"name": "id", "column": "id", "type": "big_int", "primary_key": true, "auto_increment": true})
    );
    assert_eq!(post["fields"][1], json!({"name": "author_id", "column": "author_id", "type": "big_int", "index": true}));
    assert_eq!(post["fields"][3], json!({"name": "body", "column": "body", "type": "text"}));
    assert_eq!(post["fields"][4], json!({"name": "views", "column": "views", "type": "int", "default": 0}));
    assert_eq!(
        post["relations"][0],
        json!({"name": "author", "kind": "one", "target": "User", "from": "author_id", "to": "id",
               "foreign_key": true, "on_delete": "cascade"})
    );
    assert_eq!(post["relations"][1], json!({"name": "comments", "kind": "many", "target": "Comment", "from": "id", "to": "post_id"}));
    assert_eq!(
        post["indexes"][0],
        json!({"columns": [{"field": "author_id"}, {"field": "created_at", "desc": true}], "where": "published"})
    );
    assert_eq!(post["indexes"][1], json!({"columns": [{"field": "title", "opclass": "gin_trgm_ops"}], "method": "gin"}));
    assert_eq!(post["constraints"][0], json!({"kind": "check", "name": "posts_views_not_negative", "expr": "views >= 0"}));
    assert_eq!(v["models"][2]["relations"][1]["on_delete"], "set_null");
}

#[test]
fn every_feature() {
    let ir = ok(r#"
datasource db {
  provider   = "postgresql"
  url        = env("DATABASE_URL")
  extensions = [postgis(schema: "ext", version: "3.4"), uuid_ossp(map: "uuid-ossp")]
}

generator client {
  provider = "prisma-client-js"
}

function audit_row {
returns  = trigger
language = plpgsql
body     = """
BEGIN
    RETURN NULL;
END;
"""
}

model Room {
  id         String                       @id @default(dbgenerated("gen_random_uuid()")) @db.Uuid
  name       String                       @unique @db.Citext
  floor      Int                          @check("floor >= 0") @comment("0 = ground")
  features   Json?                        @default("{\"wifi\": true}")
  embedding  Unsupported("vector(3)")?
  spot       Unsupported("geography(Point)")?
  title      String                       @map("label") @renamed_from("caption") @db.VarChar(80)
  price      Decimal                      @db.Decimal(10, 2)
  born_on    DateTime?                    @db.Date
  small      Int                          @db.SmallInt
  updated_at DateTime                     @default(now())
  bookings   Booking[]

  @@index([embedding(ops: vector_cosine_ops)], type: hnsw, with: { m: 16, ef_construction: 64 })
  @@index([sql("lower(label)", collate: "C"), floor(sort: Desc, nulls: last)], unique: true, include: [name], name: "rooms_x")
  @@unique([floor, title], nulls_not_distinct: true, deferrable: deferred)
  @@map("rooms")
  @@trigger(audit, after: [insert, delete], for_each: statement, function: audit_row, args: ["rooms"])
  @@trigger(touch, before: [update], update_of: [title], when: "OLD.label <> NEW.label", body: "BEGIN RETURN NEW; END;")
  @@comment("bookable")
}

model Booking {
  id      BigInt   @id @default(autoincrement())
  room_id String   @db.Uuid
  starts  DateTime
  ends    DateTime
  room    Room     @relation(fields: [room_id], references: [id], onDelete: Restrict, onUpdate: Cascade, deferrable: immediate)

  @@exclude([room_id(op: "="), sql("tstzrange(starts, ends)", op: "&&")], where: "starts < ends")
}
"#);
    let (ir, schema) = check(ir).unwrap();
    let v = serde_json::to_value(&ir).unwrap();
    let room = &v["models"][0];
    assert_eq!(room["table"], "rooms");
    assert_eq!(room["comment"], "bookable");
    assert_eq!(room["fields"][0]["type"], "uuid");
    assert_eq!(room["fields"][0]["default_sql"], "gen_random_uuid()");
    assert_eq!(room["fields"][1]["db_type"], "citext");
    assert_eq!(room["fields"][1]["write_sql"], "CAST({} AS citext)");
    assert_eq!(room["fields"][3]["default"], json!({"wifi": true}));
    assert_eq!(room["fields"][4]["db_type"], "vector(3)");
    assert_eq!(room["fields"][4]["hints"]["python"], "list[float]");
    assert_eq!(room["fields"][5]["db_type"], "geography(Point, 4326)");
    assert_eq!(room["fields"][6]["column"], "label");
    assert_eq!(room["fields"][6]["max_length"], 80);
    assert_eq!(room["fields"][6]["renamed_from"], "caption");
    assert_eq!(
        room["fields"][7],
        json!({"name": "price", "column": "price", "type": "string", "db_type": "numeric(10, 2)",
               "read_sql": "CAST({} AS text)", "write_sql": "CAST({} AS numeric(10, 2))"})
    );
    assert_eq!(room["fields"][8]["type"], "date");
    assert_eq!(room["fields"][9]["db_type"], "smallint");
    assert_eq!(room["relations"][0], json!({"name": "bookings", "kind": "many", "target": "Booking", "from": "id", "to": "room_id"}));
    assert_eq!(room["indexes"][0]["method"], "hnsw");
    assert_eq!(room["indexes"][0]["with"], json!([["m", "16"], ["ef_construction", "64"]]));
    assert_eq!(room["indexes"][1]["name"], "rooms_x");
    assert_eq!(room["triggers"][1]["update_of"], json!(["title"]));
    assert_eq!(room["triggers"][1]["body"], "BEGIN RETURN NEW; END;");
    let booking = &v["models"][1];
    assert_eq!(booking["table"], "booking");
    assert_eq!(booking["relations"][0]["on_update"], "cascade");
    assert_eq!(booking["relations"][0]["deferrable"], "immediate");
    assert_eq!(booking["constraints"][0]["requires"], json!(["btree_gist"]));
    assert_eq!(v["extensions"][0], json!({"name": "postgis", "schema": "ext", "version": "3.4"}));
    assert_eq!(v["extensions"][1], json!({"name": "uuid-ossp"}));
    assert_eq!(v["functions"][0]["body"], "BEGIN\n    RETURN NULL;\nEND;");

    let snap = crate::migrate::snapshot(&schema).unwrap();
    let exts: Vec<_> = snap.extensions.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(exts, ["btree_gist", "citext", "postgis", "uuid-ossp", "vector"]);
}

#[test]
fn named_relations_pair_up() {
    let ir = ok(r#"
model User {
  id      BigInt    @id
  written Message[] @relation("author")
  read    Message[] @relation("reader")
}

model Message {
  id        BigInt @id
  author_id BigInt
  reader_id BigInt
  author    User   @relation("author", fields: [author_id], references: [id])
  reader    User   @relation("reader", fields: [reader_id], references: [id])
}
"#);
    let user = &ir.models[0];
    assert_eq!((user.relations[0].to.as_str(), user.relations[1].to.as_str()), ("author_id", "reader_id"));
    let e = fails(r#"
model User {
  id      BigInt    @id
  written Message[]
}

model Message {
  id        BigInt @id
  author_id BigInt
  reader_id BigInt
  author    User   @relation("author", fields: [author_id], references: [id])
  reader    User   @relation("reader", fields: [reader_id], references: [id])
}
"#);
    assert!(e.contains("relation User.written: Message has no relation to User with fields:"), "{e}");
}

#[test]
fn imported_extension_files_add_types() {
    let dir = std::env::temp_dir().join(format!("orm-dsl-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("ext")).unwrap();
    std::fs::write(
        dir.join("ext/acme.toml"),
        "name = \"acme\"\nfunctions = [\"acme_slug\"]\n[types.money]\nsql = \"numeric({p}, 2)\"\nargs = [\"p\"]\ndefaults = { p = \"12\" }\nvalue = \"text\"\npython = \"str\"\n",
    )
    .unwrap();
    let path = dir.join("schema.prisma");
    std::fs::write(
        &path,
        "import \"ext/acme.toml\"\nmodel Item {\n  id BigInt @id\n  price Unsupported(\"money\")\n  slug String @db.Text @default(dbgenerated(\"acme_slug()\"))\n}",
    )
    .unwrap();
    let (ir, schema) = check(compile_file(&path).unwrap()).unwrap();
    assert_eq!(ir.models[0].fields[1].db_type.as_deref(), Some("numeric(12, 2)"));
    assert_eq!(ir.models[0].fields[1].requires, ["acme"]);
    let snap = crate::migrate::snapshot(&schema).unwrap();
    assert_eq!(snap.extensions.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["acme"]);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn errors_point_at_the_problem() {
    let b = "model B {\n  id BigInt @id\n}";
    let cases = [
        ("model A {\n  id BigInt @id\n  x Strng\n}".to_owned(), "<schema>:3:5: A.x: unknown type Strng"),
        ("model A {\n  id BigInt @id @prim\n}".into(), "unknown attribute @prim"),
        ("model A {\n  x Int\n}".into(), "model A has no @id field"),
        (format!("model A {{\n  id BigInt @id\n  b B @relation(fields: [b_id], references: [id])\n}}\n{b}"), "no field b_id"),
        (
            format!("model A {{\n  id BigInt @id\n  b_id BigInt?\n  b B @relation(fields: [b_id], references: [id])\n}}\n{b}"),
            "so the relation type is `B?`",
        ),
        (format!("model A {{\n  id BigInt @id\n  b_id BigInt\n  b B @relation(fields: [b_id])\n}}\n{b}"), "needs references:"),
        (format!("model A {{\n  id BigInt @id\n  bs B[]\n}}\n{b}"), "B has no relation to A"),
        (
            format!("model A {{\n  id BigInt @id\n  b_id BigInt\n  b B @relation(fields: [b_id], references: [id], onDelete: cascade)\n}}\n{b}"),
            "unknown referential action cascade",
        ),
        ("model A {\n  id BigInt @id\n  @@index([nope])\n}".into(), "nope"),
        ("model A {\n  id BigInt @id\n  @@index([id], wher: \"x\")\n}".into(), "unknown argument `wher`"),
        ("model A {\n  id BigInt @id\n  @@index([id(sort: desc)])\n}".into(), "sort must be Asc or Desc"),
        ("model A {\n  id BigInt @id\n  @@trigger(t, body: \"x\")\n}".into(), "say when it fires"),
        ("model A {\n  id BigInt @id\n  e Unsupported(\"vector\")\n}".into(), "needs argument \"dims\""),
        ("model A {\n  id BigInt @id\n  e Unsupported(\"nope(1)\")\n}".into(), "unknown type nope"),
        ("model A {\n  id BigInt @id\n  e String @db.Nope\n}".into(), "unknown native type @db.Nope"),
        ("model A {\n  id BigInt @id @default(uuid())\n}".into(), "not uuid"),
        ("model A {\n  id BigInt @id\n  j Json @default(\"{oops\")\n}".into(), "JSON text"),
        (format!("{b}\n{b}"), "declared twice"),
        ("datasource db {\n  provider = \"mysql\"\n}".into(), "provider must be \"postgresql\""),
        ("enum Role {\n  A\n}".into(), "`enum` blocks aren't supported yet"),
        ("import \"missing.toml\"".into(), "import \"missing.toml\""),
        ("model A {\n  id BigInt @id\n  @@trigger(t, after: [insert],\n    function: f)\n}".into(), "<schema>:3:3: model A: an attribute must be on one line"),
        (
            "model A {\n  id BigInt @id\n  @@trigger(t, before: [update], body: \"\"\"BEGIN RETURN NEW; END;\"\"\")\n}".into(),
            "only go in top-level blocks",
        ),
    ];
    for (src, expected) in cases {
        let e = fails(&src);
        assert!(e.contains(expected), "{src:?}\n  got: {e}\n  expected: {expected}");
    }
}

/// The `prisma` code blocks in the docs parse, and the complete ones compile (the
/// "every feature" one imports a file that isn't there). The blog example in
/// prisma-syntax.md is the blog schema.
#[test]
fn documented_schemas_parse() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let blog = std::fs::read_to_string(root.join("examples/blog/schema.prisma")).unwrap();
    let blog = &blog[blog.find("datasource").unwrap()..];
    let mut blog_documented = false;
    for doc in ["docs/prisma-syntax.md", "docs/schema.md", "PLAN.md"] {
        let text = std::fs::read_to_string(root.join(doc)).unwrap();
        for block in text.split("```prisma\n").skip(1) {
            let src = &block[..block.find("```").unwrap()];
            if let Err(e) = syntax::parse(src) {
                panic!("{doc}: {}: {}\n{src}", e.pos, e.msg);
            }
            blog_documented |= src == blog;
            if !src.contains("import ") && !src.contains("(excerpt)") && src.contains("model ") {
                if let Err(e) = compile(src, None).and_then(check) {
                    panic!("{doc}: {e}\n{src}");
                }
            }
        }
    }
    assert!(blog_documented, "docs/prisma-syntax.md: update the blog example");
}
