//! Frontend selection must preserve backend/tooling defaults after static wiring.
use std::{fs, path::Path, process::Command};

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() { copy(&entry.path(), &to.join(entry.file_name())); }
        else { fs::copy(entry.path(), to.join(entry.file_name())).unwrap(); }
    }
}

#[test]
fn frontend_preserves_defaults_and_accepts_exact_binding_features() {
    let original = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let base = original.join("target/extension-proof-tests").join(format!("binding-features-{}", std::process::id()));
    let host = base.join("host");
    for component in ["core", "engine", "cli", "contracts", "bindings", "storage/reference", "extensions/proxy-runtime", "extension-build/tests/fixtures/rename"] {
        copy(&original.join(component), &host.join(component));
    }
    for file in ["Cargo.toml", "Cargo.lock"] { fs::copy(original.join(file), host.join(file)).unwrap(); }
    for binding in ["python", "node"] {
        let path = host.join(format!("bindings/{binding}/Cargo.toml"));
        let mut manifest: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let features = manifest["features"].as_table_mut().unwrap();
        features.insert("default".into(), toml::Value::try_from(vec!["sqlite", "generate-python"]).unwrap());
        features.entry("sqlite").or_insert(toml::Value::Array(vec![]));
        features.entry("generate-python").or_insert(toml::Value::Array(vec![]));
        fs::write(path, toml::to_string(&manifest).unwrap()).unwrap();
    }
    for (case, selection) in [("preserve", None), ("exact", Some(serde_json::json!({"python":["sqlite"],"node":[]}))), ("unknown", Some(serde_json::json!({"python":["unknown"]}))), ("unselected", Some(serde_json::json!({"ruby":[]})))] {
        let output = base.join(case);
        let mut config = serde_json::json!({"host":host,"output":output,"offline":true,"prepare_only":true,
            "dependencies":{"rename":{"package":"example-rename","path":host.join("extension-build/tests/fixtures/rename")}}});
        if let Some(selection) = selection { config["binding_features"] = selection; }
        let path = base.join(format!("{case}.json"));
        fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_orm-extension-build")).arg(path).output().unwrap();
        if case == "unknown" || case == "unselected" {
            assert!(!result.status.success());
            let error = String::from_utf8_lossy(&result.stderr);
            assert!(error.contains(if case == "unknown" { "unknown or non-exact host feature unknown" } else { "unselected binding ruby" }), "{error}");
            continue;
        }
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        for binding in ["python", "node"] {
            let manifest: toml::Value = toml::from_str(&fs::read_to_string(output.join(format!("bindings/{binding}/Cargo.toml"))).unwrap()).unwrap();
            let actual = manifest["features"]["default"].as_array().unwrap();
            let expected = if case == "preserve" { vec!["sqlite", "generate-python", "composition"] }
                else if binding == "python" { vec!["sqlite", "composition"] } else { vec!["composition"] };
            assert_eq!(*actual, expected.into_iter().map(|value| toml::Value::String(value.into())).collect::<Vec<_>>());
        }
    }
    fs::remove_dir_all(base).unwrap();
}
