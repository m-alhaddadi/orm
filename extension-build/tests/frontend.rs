use std::{fs, path::PathBuf, process::Command};

#[test]
fn local_crate_builds_without_host_id_changes_and_lowers_once() {
    let host = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let root = host.join("target/extension-proof-tests").join(format!("orm-extension-build-test-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let output = root.join("build");
    assert!(!output.exists());
    let config = root.join("config.json");
    fs::write(&config, serde_json::to_vec(&serde_json::json!({
        "host": host, "output": output, "offline": true, "prepare_only": true,
        "bindings": ["python"], "dependencies": {"rename": {"package": "example-rename", "path": host.join("extension-build/tests/fixtures/rename")}}
    })).unwrap()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_orm-extension-build")).arg(config).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    fs::create_dir_all(output.join("core/examples")).unwrap();
    fs::write(output.join("core/examples/proof.rs"), r###"
use orm_core::{dsl, schema::Schema, migrate};
fn main() {
    let ir = dsl::compile("model User {\n id Int @id\n name String @example.rename(to: \"public_name\")\n secret String? @example.hide\n}\nmodel UserView {\n @@example.proxy(parent: \"User\")\n}", None).unwrap();
    let missing_identity = dsl::compile("model User {\n id Int @id\n}\nmodel UserView {\n id Int @example.hide\n @@example.proxy(parent: \"User\")\n}", None).err().unwrap();
    assert!(missing_identity.ends_with(":4:1: model UserView has no @id field after extension lowering"), "{missing_identity}");
    assert_eq!(ir.models[0].fields[1].name, "public_name");
    assert_eq!(ir.models[0].fields[1].column, "name");
    let json = serde_json::to_string(&ir).unwrap();
    let normalized = serde_json::from_str(&json).unwrap();
    let schema = Schema::from_ir(normalized).unwrap();
    let direct = Schema::from_ir(dsl::compile("model User {\n id Int @id\n public_name String @map(\"name\")\n secret String?\n}", None).unwrap()).unwrap();
    assert_eq!(migrate::create_all(&schema).unwrap(), migrate::create_all(&direct).unwrap());
    assert_eq!(schema.models[0].fields()[1].name, "public_name");
    assert_eq!(schema.models[0].fields().len(), 2);
    assert_eq!(schema.models[1].table(), schema.models[0].table());
    assert_eq!(schema.models[1].resolved_fields[1].storage.owner, orm_core::behavior::OwnerId(0));
    assert_eq!(migrate::snapshot(&schema).unwrap().tables.len(), 1);
    assert_eq!(migrate::plan(&schema, &migrate::snapshot(&direct).unwrap()).unwrap().up.len(), 0);
    // Recompilation would fail the extension's collision check if applied twice.
    assert_eq!(ir.behavior.completed_passes, vec!["example.rename.lower"]);
    let incoming = serde_json::from_str(r##"{"models":[{"name":"AutoLowered","table":"auto_lowered","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]}]}"##).unwrap();
    let mut batch = orm_core::behavior::merge_definition(ir, incoming).unwrap();
    orm_core::behavior::prepare(&mut batch, None).unwrap();
    assert_eq!(batch.models.last().unwrap().comment.as_deref(), Some("prepared by example"));
    let normalized = serde_json::to_string(&batch).unwrap();
    let mut batch = serde_json::from_str(&normalized).unwrap();
    orm_core::behavior::prepare(&mut batch, None).unwrap();
    assert_eq!(serde_json::to_string(&batch).unwrap(), normalized);
}
"###).unwrap();
    let result = Command::new("cargo").args(["run", "--quiet", "--offline", "--locked", "--manifest-path"])
        .arg(output.join("Cargo.toml")).args(["-p", "orm-core", "--features", "composition", "--example", "proof"])
        .env("ORM_CORE_COMPOSITION", output.join("composition.rs"))
        .env("CARGO_TARGET_DIR", host.join("target/extension-proof"))
        .output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    fs::create_dir_all(output.join("engine/examples")).unwrap();
    fs::copy(host.join("extension-build/tests/public/ownership.rs"), output.join("engine/examples/ownership.rs")).unwrap();
    let result = Command::new("cargo").args(["run", "--quiet", "--offline", "--locked", "--manifest-path"])
        .arg(output.join("Cargo.toml")).args(["-p", "orm-engine", "--features", "composition", "--example", "ownership"])
        .env("ORM_CORE_COMPOSITION", output.join("composition.rs"))
        .env("ORM_ENGINE_COMPOSITION", output.join("engine-composition.rs"))
        .env("CARGO_TARGET_DIR", host.join("target/extension-proof"))
        .output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn core_only_build_adds_no_composition() {
    let host = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let root = host.join("target/extension-proof-tests").join(format!("orm-core-only-test-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let output = root.join("build");
    let config = root.join("config.json");
    fs::write(&config, serde_json::to_vec(&serde_json::json!({
        "host": host, "output": output, "offline": true, "prepare_only": true, "bindings": ["python"]
    })).unwrap()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_orm-extension-build")).arg(config).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let artifact: serde_json::Value = serde_json::from_slice(&fs::read(output.join("artifact.json")).unwrap()).unwrap();
    assert_eq!(artifact["extensions"], serde_json::json!({}));
    assert_eq!(fs::read_to_string(output.join("manifests.json")).unwrap().trim(), "[]");
    for generated in ["engine-composition.rs", "python-methods.rs", "core/build.rs", "core/composition-inputs.json"] {
        assert!(!output.join(generated).exists(), "{generated}");
    }
    for manifest in ["core/Cargo.toml", "engine/Cargo.toml", "bindings/python/Cargo.toml"] {
        // The build rewrites relative paths, so compare only the dependency names and features.
        let table = |path: PathBuf| fs::read_to_string(path).unwrap().parse::<toml::Table>().unwrap();
        let (built, source) = (table(output.join(manifest)), table(host.join(manifest)));
        let names = |t: &toml::Table| t.get("dependencies").and_then(|d| d.as_table()).map(|d| d.keys().cloned().collect::<Vec<_>>());
        assert_eq!(names(&built), names(&source), "{manifest}");
        assert_eq!(built.get("features"), source.get("features"), "{manifest}");
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn manifest_capability_selects_the_host_feature() {
    let host = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let root = host.join("target/extension-proof-tests").join(format!("orm-extension-feature-test-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    for (feature, alias, package, path) in [
        ("file-storage", "file_storage", "orm-file-storage-extension", "storage/orm-extension"),
        ("query-defaults", "query_defaults", "orm-query-defaults", "query-defaults"),
        ("model-composition", "composition", "orm-model-composition", "model-composition"),
        ("proxy-models", "proxy", "orm-proxy", "extensions/proxy"),
        ("generic-relations", "generic", "orm-generic", "extensions/generic"),
        ("updated-at", "timestamps", "orm-timestamps", "extensions/timestamps"),
        ("soft-delete", "soft_delete", "orm-soft-delete", "extensions/soft-delete"),
        ("optimistic-locking", "locking", "orm-locking", "extensions/locking"),
    ] {
        let output = root.join(feature);
        let config = root.join(format!("{feature}.json"));
        fs::write(&config, serde_json::to_vec(&serde_json::json!({
            "host": host, "output": output, "offline": true, "prepare_only": true,
            "bindings": ["python", "node"], "dependencies": {alias: {"package": package, "path": host.join(path)}}
        })).unwrap()).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_orm-extension-build")).arg(config).output().unwrap();
        assert!(result.status.success(), "{feature}: {}", String::from_utf8_lossy(&result.stderr));
        let selected = toml::Value::String(feature.into());
        for binding in ["python", "node"] {
            let manifest: toml::Value = toml::from_str(&fs::read_to_string(output.join("bindings").join(binding).join("Cargo.toml")).unwrap()).unwrap();
            let defaults = manifest["features"]["default"].as_array().unwrap();
            assert!(defaults.contains(&selected), "{feature}: {binding} default features");
            // Extension artifacts keep the binding defaults, reference loading included.
            assert!(defaults.contains(&toml::Value::String("reference-loading".into())), "{feature}: {binding} default features");
            assert!(manifest["dependencies"]["orm-core"]["features"].as_array().unwrap().contains(&selected), "{feature}: {binding} orm-core features");
        }
        let artifact: serde_json::Value = serde_json::from_slice(&fs::read(output.join("artifact.json")).unwrap()).unwrap();
        assert!(artifact["capabilities"].as_array().unwrap().contains(&serde_json::json!(feature)), "{feature}: {artifact}");
        // The orm-core and orm-engine tests of the snapshot read these host files.
        for file in ["examples/blog/schema.prisma", "examples/sqlite/schema.prisma", "docs/prisma-syntax.md", "docs/schema.md", "PLAN.md"] {
            assert_eq!(fs::read(output.join(file)).unwrap(), fs::read(host.join(file)).unwrap(), "{feature}: {file}");
        }
    }
    fs::remove_dir_all(root).unwrap();
}
