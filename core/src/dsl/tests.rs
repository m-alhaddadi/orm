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
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/blog/schema.orm");
    let (ir, _) = check(compile_file(&path).unwrap()).unwrap();
    let v = serde_json::to_value(&ir).unwrap();
    let post = &v["models"][1];
    assert_eq!(post["table"], "posts");
    assert_eq!(post["fields"][1], json!({"name": "author_id", "column": "author_id", "type": "big_int", "index": true}));
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
}

#[test]
fn every_feature() {
    let ir = ok(r#"
        extension postgis(schema: "ext", version: "3.4")

        function audit_row {
            returns: trigger
            body: """
                BEGIN
                    RETURN NULL;
                END;
            """
        }

        model Room @table("rooms") @comment("bookable") {
            id:        Uuid           @primary @default(sql("gen_random_uuid()"))
            name:      citext         @unique
            floor:     Int            @check("floor >= 0") @comment("0 = ground")
            features:  Json?          @default({"wifi": true})
            embedding: vector(3)?
            spot:      geography(Point)?
            title:     String(80)     @column("label") @renamed_from("caption")

            @@index([embedding(ops: vector_cosine_ops)], type: hnsw, with: {m: 16, ef_construction: 64})
            @@index([sql("lower(label)", collate: "C"), floor(sort: desc, nulls: last)], unique: true, include: [name], name: "rooms_x")
            @@unique([floor, title], nulls_not_distinct: true, deferrable: deferred)
            @@trigger(audit, after: [insert, delete], for_each: statement, function: audit_row, args: ["rooms"])
            @@trigger(touch, before: [update], update_of: [title], when: "OLD.label <> NEW.label",
                      body: """BEGIN RETURN NEW; END;""")
        }

        model Booking {
            id:       BigInt   @primary @auto
            room_id:  Uuid
            starts:   DateTime
            ends:     DateTime
            room:     Room     @relation(via: room_id, on_delete: restrict, on_update: cascade, deferrable: immediate)

            @@exclude([room_id(op: "="), sql("tstzrange(starts, ends)", op: "&&")], where: "starts < ends")
        }
    "#);
    let (ir, schema) = check(ir).unwrap();
    let v = serde_json::to_value(&ir).unwrap();
    let room = &v["models"][0];
    assert_eq!(room["comment"], "bookable");
    assert_eq!(room["fields"][0]["default_sql"], "gen_random_uuid()");
    assert_eq!(room["fields"][1]["db_type"], "citext");
    assert_eq!(room["fields"][1]["write_sql"], "CAST({} AS citext)");
    assert_eq!(room["fields"][3]["default"], json!({"wifi": true}));
    assert_eq!(room["fields"][4]["db_type"], "vector(3)");
    assert_eq!(room["fields"][4]["hints"]["python"], "list[float]");
    assert_eq!(room["fields"][5]["db_type"], "geography(Point, 4326)");
    assert_eq!(room["fields"][6]["column"], "label");
    assert_eq!(room["indexes"][0]["with"], json!([["m", "16"], ["ef_construction", "64"]]));
    assert_eq!(room["triggers"][1]["update_of"], json!(["title"]));
    let booking = &v["models"][1];
    assert_eq!(booking["table"], "booking");
    assert_eq!(booking["constraints"][0]["requires"], json!(["btree_gist"]));
    assert_eq!(v["extensions"][0], json!({"name": "postgis", "schema": "ext", "version": "3.4"}));

    let snap = crate::migrate::snapshot(&schema).unwrap();
    let exts: Vec<_> = snap.extensions.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(exts, ["btree_gist", "citext", "postgis", "vector"]);
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
    let path = dir.join("schema.orm");
    std::fs::write(
        &path,
        "import \"ext/acme.toml\"\nmodel Item { id: BigInt @primary\n price: money\n slug: Text @default(sql(\"acme_slug()\")) }",
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
    let cases = [
        ("model A { id: BigInt @primary\n x: Strng }", "<schema>:2:5: A.x: unknown type Strng"),
        ("model A { id: BigInt @primary @prim }", "unknown attribute @prim"),
        ("model A { x: Int }", "model A has no @primary field"),
        ("model A { id: BigInt @primary\n b: B @relation(via: b_id) }\nmodel B { id: BigInt @primary }", "no field b_id"),
        ("model A { id: BigInt @primary\n b_id: BigInt?\n b: B @relation(via: b_id) }\nmodel B { id: BigInt @primary }", "so the relation type is `B?`"),
        ("model A { id: BigInt @primary\n bs: B[] @relation(via: B.a_id) }\nmodel B { id: BigInt @primary }", "B has no field a_id"),
        ("model A { id: BigInt @primary\n @@index([nope]) }", "nope"),
        ("model A { id: BigInt @primary\n @@index([id], wher: \"x\") }", "unknown argument `wher`"),
        ("model A { id: BigInt @primary\n @@trigger(t, body: \"x\") }", "say when it fires"),
        ("model A { id: BigInt @primary\n e: vector }", "needs argument \"dims\""),
        ("model A { id: BigInt @primary }\nmodel A { id: BigInt @primary }", "declared twice"),
        ("import \"missing.toml\"", "import \"missing.toml\""),
    ];
    for (src, expected) in cases {
        let e = fails(src);
        assert!(e.contains(expected), "{src:?}\n  got: {e}\n  expected: {expected}");
    }
}
