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
    let model = |name: &str| v["models"].as_array().unwrap().iter().find(|m| m["name"] == name).unwrap().clone();
    let user = &model("User");
    assert_eq!(user["fields"][1], json!({"name": "email", "column": "email", "type": "string", "unique": true, "max_length": 254}));
    assert_eq!(user["relations"][0], json!({"name": "posts", "kind": "many", "target": "Post", "from": "id", "to": "author_id"}));
    let post = &model("Post");
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
    assert_eq!(model("Comment")["relations"][1]["on_delete"], "set_null");
    assert_eq!(model("User")["relations"][2], json!({"name": "profile", "kind": "one", "target": "Profile", "from": "id", "to": "user_id"}));
    assert_eq!(post["relations"][2]["through"], json!({"model": "PostTag", "source": "post_id", "target": "tag_id"}));
    assert_eq!(v["enums"][1]["storage"], "int");
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
        json!({"name": "price", "column": "price", "type": "decimal", "db_type": "numeric(10, 2)"})
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
        ("enum Role {\n  A\n  A\n}".into(), "A is declared twice"),
        ("enum Role {\n  A @value(1)\n}".into(), "@value(n) is for @@storage(int)"),
        ("enum Role {\n  A\n  @@storage(int)\n}".into(), "need @value(n)"),
        ("enum Role {\n  A\n  @@storage(blob)\n}".into(), "native, text or int"),
        ("model A {\n  id BigInt @id\n  r Role @default(B)\n}\nenum Role {\n  A\n}".into(), "Role has no value B"),
        ("model A {\n  id BigInt @id\n  r Role @db.Text\n}\nenum Role {\n  A\n}".into(), "comes from the enum"),
        ("type T {\n  a Int\n}".into(), "`type` blocks aren't supported yet"),
        ("model A {\n  id BigInt[] @id\n}".into(), "an array can't be a primary key"),
        (
            format!("model A {{\n  id BigInt @id\n  b B?\n}}\nmodel B {{\n  id BigInt @id\n  a_id BigInt\n  a A @relation(fields: [a_id], references: [id])\n}}"),
            "B.a_id must be unique for a one-to-one relation",
        ),
        (
            format!("model A {{\n  id BigInt @id\n  b B\n}}\nmodel B {{\n  id BigInt @id\n  a_id BigInt @unique\n  a A @relation(fields: [a_id], references: [id])\n}}"),
            "a one-to-one back relation is optional: B?",
        ),
        (format!("model A {{\n  id BigInt @id\n  bs B[] @relation(through: J)\n}}\n{b}"), "through: J is not a model"),
        (
            format!("model A {{\n  id BigInt @id\n  bs B[] @relation(through: J)\n}}\n{b}\nmodel J {{\n  id BigInt @id\n  a_id BigInt\n  a A @relation(fields: [a_id], references: [id])\n}}"),
            "J has no relation with fields: to B",
        ),
        (
            format!("model A {{\n  id BigInt @id\n  as A[] @relation(through: J)\n}}\nmodel J {{\n  id BigInt @id\n  x BigInt\n  y BigInt\n  a A @relation(\"x\", fields: [x], references: [id])\n  b A @relation(\"y\", fields: [y], references: [id])\n}}"),
            "needs through_fields",
        ),
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

#[test]
fn enums_arrays_decimals() {
    let ir = ok(r#"
enum Role {
  member
  admin  @map("ADMIN")
  @@map("user_role")
}

enum Priority {
  low  @value(1)
  high @value(10)
  @@storage(int)
}

enum Color {
  red
  green
  @@storage(text)
}

model Item {
  id       BigInt     @id
  role     Role       @default(member)
  roles    Role[]     @default([member, admin])
  priority Priority?
  color    Color      @default(green)
  tags     String[]   @default([])
  scores   Int[]
  price    Decimal    @default(0) @db.Decimal(12, 2)
  ratio    Decimal?
}
"#);
    let v = serde_json::to_value(&ir).unwrap();
    assert_eq!(
        v["enums"][0],
        json!({"name": "Role", "db_name": "user_role", "storage": "native",
               "values": [{"name": "member", "value": "member"}, {"name": "admin", "value": "ADMIN"}]})
    );
    assert_eq!(v["enums"][1]["values"][1], json!({"name": "high", "value": 10}));
    let f = &v["models"][0]["fields"];
    assert_eq!(
        f[1],
        json!({"name": "role", "column": "role", "type": "string", "enum": "Role", "db_type": "\"user_role\"",
               "write_sql": "CAST({} AS \"user_role\")", "default": "member"})
    );
    assert_eq!(f[2]["array"], true);
    assert_eq!(f[2]["write_sql"], "CAST({} AS \"user_role\"[])");
    assert_eq!(f[2]["default"], json!(["member", "ADMIN"]));
    assert_eq!(f[3]["type"], "int");
    assert_eq!(f[4]["type"], "text");
    assert_eq!(f[5], json!({"name": "tags", "column": "tags", "type": "string", "array": true, "default": []}));
    assert_eq!(f[7]["type"], "decimal");

    let (_, schema) = check(ir).unwrap();
    let snap = crate::migrate::snapshot(&schema).unwrap();
    assert_eq!(snap.enums.len(), 1);
    assert_eq!(snap.enums[0].values, ["member", "ADMIN"]);
    let t = &snap.tables[0];
    let ty = |c: &str| t.column(c).unwrap().ty.clone();
    assert_eq!(ty("role"), "\"user_role\"");
    assert_eq!(ty("roles"), "\"user_role\"[]");
    assert_eq!(ty("priority"), "integer");
    assert_eq!(ty("tags"), "text[]");
    assert_eq!(ty("scores"), "integer[]");
    assert_eq!(ty("price"), "numeric(12, 2)");
    assert_eq!(ty("ratio"), "numeric");
    assert_eq!(t.column("roles").unwrap().default.as_deref(), Some("ARRAY['member', 'ADMIN']::\"user_role\"[]"));
    assert_eq!(t.column("tags").unwrap().default.as_deref(), Some("'{}'::text[]"));
    let checks: Vec<(&str, &str)> = t.checks.iter().map(|c| (c.name.as_str(), c.expr.as_str())).collect();
    assert_eq!(
        checks,
        [("item_priority_enum_check", "\"priority\" IN (1, 10)"), ("item_color_enum_check", "\"color\" IN ('red', 'green')")]
    );
}

#[test]
fn has_one_and_many_to_many() {
    let ir = ok(r#"
model User {
  id      BigInt   @id
  profile Profile?
}

model Profile {
  id      BigInt @id
  user_id BigInt @unique
  user    User   @relation(fields: [user_id], references: [id])
}

model Post {
  id        BigInt    @id
  tags      Tag[]     @relation(through: PostTag)
  post_tags PostTag[]
}

model Tag {
  id    BigInt @id
  posts Post[] @relation(through: PostTag)
}

model PostTag {
  id      BigInt @id
  post_id BigInt
  tag_id  BigInt
  post    Post   @relation(fields: [post_id], references: [id])
  tag     Tag    @relation(fields: [tag_id], references: [id])

  @@unique([post_id, tag_id])
}

model Person {
  id        BigInt   @id
  following Person[] @relation(through: Follow, through_fields: [follower, followee])
  followers Person[] @relation(through: Follow, through_fields: [followee, follower])
}

model Follow {
  id          BigInt @id
  follower_id BigInt
  followee_id BigInt
  follower    Person @relation("a", fields: [follower_id], references: [id])
  followee    Person @relation("b", fields: [followee_id], references: [id])
}
"#);
    let v = serde_json::to_value(&ir).unwrap();
    assert_eq!(
        v["models"][0]["relations"][0],
        json!({"name": "profile", "kind": "one", "target": "Profile", "from": "id", "to": "user_id"})
    );
    assert_eq!(
        v["models"][2]["relations"][0],
        json!({"name": "tags", "kind": "many", "target": "Tag", "from": "id", "to": "id",
               "through": {"model": "PostTag", "source": "post_id", "target": "tag_id"}})
    );
    assert_eq!(v["models"][3]["relations"][0]["through"], json!({"model": "PostTag", "source": "tag_id", "target": "post_id"}));
    assert_eq!(v["models"][5]["relations"][1]["through"], json!({"model": "Follow", "source": "followee_id", "target": "follower_id"}));
    let (_, schema) = check(ir).unwrap();
    // neither side of a has-one or many-to-many adds a foreign key of its own
    let snap = crate::migrate::snapshot(&schema).unwrap();
    assert_eq!(snap.tables.iter().map(|t| t.foreign_keys.len()).sum::<usize>(), 5);
}
