//! Real optional compiler crate and enabled engine integration.
use std::{fs, path::PathBuf, process::Command};
#[test]
fn model_composition_compiles_and_executes() {
    let host = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = host.join(format!(
        "target/model-composition-proof-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    let output = root.join("build");
    let config = root.join("config.json");
    fs::write(&config,serde_json::to_vec(&serde_json::json!({"host":host,"output":output,"offline":true,"prepare_only":true,"bindings":["python","node"],"dependencies":{"composition":{"package":"orm-model-composition","path":host.join("model-composition")}}})).unwrap()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_orm-extension-build"))
        .arg(config)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    fs::create_dir_all(output.join("engine/examples")).unwrap();
    fs::copy(
        host.join("extension-build/tests/public/model-composition.rs"),
        output.join("engine/examples/composed.rs"),
    )
    .unwrap();
    let result = Command::new("cargo")
        .args(["run", "--quiet", "--offline", "--locked", "--manifest-path"])
        .arg(output.join("Cargo.toml"))
        .args([
            "-p",
            "orm-engine",
            "--features",
            "model-composition,orm-core/generate-python,orm-core/generate-typescript",
            "--example",
            "composed",
        ])
        .env("ORM_CORE_COMPOSITION", output.join("composition.rs"))
        .env(
            "ORM_ENGINE_COMPOSITION",
            output.join("engine-composition.rs"),
        )
        .env(
            "CARGO_TARGET_DIR",
            host.join("target/composition-feature/native"),
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}
