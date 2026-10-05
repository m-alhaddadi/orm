//! Build-level contract: local application module, ordinary bindings, public APIs.
use std::{fs, path::{Path, PathBuf}, process::Command};

fn run(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(output.status.success(), "{command:?}\n{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}
fn copy_python(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() && entry.path().extension().is_some_and(|s| s == "py" || s == "pyi") {
            fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}
#[test]
fn application_rust_runs_through_both_bindings() {
    let host = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let temporary = std::env::temp_dir().join(format!("orm-native-public-{}", std::process::id()));
    fs::create_dir_all(&temporary).unwrap();
    let target = host.join("target/extension-proof");
    let python = std::env::var("PYO3_PYTHON").map(PathBuf::from).unwrap_or_else(|_| host.join(".venv/bin/python"));
    let fixture: toml::Value = toml::from_str(&fs::read_to_string(host.join("extension-build/tests/fixtures/native/Cargo.toml")).unwrap()).unwrap();
    let manifest = &fixture["package"]["metadata"]["orm-extension"];
    let mut backends = vec![("sqlite", "sqlite://:memory:".to_owned())];
    if let Ok(url) = std::env::var("ORM_TEST_DATABASE_URL") { backends.push(("postgres", url)); }
    run(Command::new(host.join("js/node_modules/.bin/tsc")).arg("-p").arg(host.join("js/tsconfig.json")));
    for (dialect, url) in backends {
        let schema = serde_json::json!({"dialect":dialect,"models":[{"name":"NativeUser","table":"native_users","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true,"auto_increment":true},
            {"name":"username","column":"username","type":"string"}
        ]},{"name":"NativeRecord","table":"native_records","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true,"auto_increment":true},
            {"name":"left","column":"left","type":"string"},
            {"name":"right","column":"right","type":"string"}
        ]},{"name":"LoweredUser","table":"lowered_users","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true},
            {"name":"name","column":"name","type":"string"},
            {"name":"secret","column":"secret","type":"string","nullable":true}
        ]},{"name":"NativeNullable","table":"native_nullable","fields":[
            {"name":"id","column":"id","type":"int","primary_key":true,"auto_increment":true},
            {"name":"username","column":"username","type":"string","nullable":true}
        ]}],"behavior":{"schema_contract":1,"declarations":[
            {"attribute":"example.rename","model":"LoweredUser","field":"name","arguments":{"to":"public_name"},"positional":[],"location":{"file":"profile.json","line":1,"column":1}},
            {"attribute":"example.hide","model":"LoweredUser","field":"secret","arguments":{},"positional":[],"location":{"file":"profile.json","line":2,"column":1}}
        ]}});
        let schema_path = temporary.join(format!("{dialect}.json"));
        fs::write(&schema_path, serde_json::to_vec(&schema).unwrap()).unwrap();
        let output = temporary.join(dialect);
        let config = temporary.join(format!("{dialect}-build.json"));
        fs::write(&config, serde_json::to_vec(&serde_json::json!({
            "host":host, "output":output, "offline":true,
            "dependencies":{"rename":{"package":"example-rename","path":host.join("extension-build/tests/fixtures/rename")}},
            "modules":[{"alias":"native_rules","source":host.join("extension-build/tests/fixtures/native/src/lib.rs"),"manifest":manifest}],
            "specialization":{"schema":schema_path,"models":[{"model":"NativeUser", "fields":[{"field":"username","transforms":["example.trim"],"validators":["example.username"]}],
                "records":[{"export":"example.record","dependencies":["username"]}],
                "methods":[{"name":"validate_username","export":"example.username"}],
                "computed":[{"field":"display","dependency":"username","export":"example.display"}]},
                {"model":"NativeRecord","records":[{"export":"example.record","dependencies":["left","right"]}]},
                {"model":"NativeNullable","fields":[{"field":"username","validators":["example.username"],"allow_null":true}],
                    "computed":[{"field":"display","dependency":"username","export":"example.display"}]}]}
        })).unwrap()).unwrap();
        run(Command::new(env!("CARGO_BIN_EXE_orm-extension-build")).arg(&config).env("CARGO_TARGET_DIR", &target).env("PYO3_PYTHON", &python));
        fs::create_dir_all(output.join("core/examples")).unwrap();
        fs::write(output.join("core/examples/generate_models.rs"), r#"
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    let dir = std::path::Path::new(&args[1]);
    let mut ir = serde_json::from_slice(&std::fs::read(dir.join("normalized.schema.json")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    orm_core::behavior::prepare(&mut ir, None)?;
    let schema = orm_core::schema::Schema::from_ir(serde_json::from_slice(&serde_json::to_vec(&ir).unwrap()).unwrap())?;
    let py = orm_core::codegen::python::generate(&ir, &schema, "native-profile.json")?;
    std::fs::write(dir.join("generated.py"), py.module).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("generated.pyi"), py.stub).map_err(|e| e.to_string())?;
    let ts = orm_core::codegen::typescript::generate(&ir, &schema, "native-profile.json", &args[2])?;
    std::fs::write(dir.join("generated.ts"), ts).map_err(|e| e.to_string())
}
"#).unwrap();
        run(Command::new("cargo").args(["run", "--quiet", "--release", "--offline", "--locked", "--manifest-path"])
            .arg(output.join("Cargo.toml")).args(["-p", "orm-core", "--features", "composition", "--example", "generate_models", "--"])
            .arg(&output).arg(host.join("js/dist/src/index.js"))
            .env("CARGO_TARGET_DIR", &target).env("ORM_CORE_COMPOSITION", output.join("composition.rs")));
        fs::write(output.join("package.json"), r#"{"type":"module"}"#).unwrap();
        run(Command::new(host.join("js/node_modules/.bin/tsc")).arg(output.join("generated.ts"))
            .args(["--module", "nodenext", "--target", "es2022", "--skipLibCheck"]));
        let lib = if cfg!(target_os="macos") { "dylib" } else { "so" };
        let package = output.join("public/python/orm");
        copy_python(&host.join("python/orm"), &package);
        fs::copy(target.join(format!("release/lib_native.{lib}")), package.join("_native.so")).unwrap();
        let addon = output.join("orm.node");
        fs::copy(target.join(format!("release/liborm_node.{lib}")), &addon).unwrap();
        run(Command::new(&python).arg(host.join("extension-build/tests/public/python.py"))
            .env("PYTHONPATH", format!("{}:{}", output.join("public/python").display(), output.display()))
            .env("ORM_EXTENSION_TEST_SCHEMA", &schema_path).env("ORM_EXTENSION_TEST_URL", &url).env("ORM_EXTENSION_GENERATED", output.join("generated.js")));
        run(Command::new("node").arg(host.join("extension-build/tests/public/node.mjs"))
            .env("ORM_NATIVE", &addon).env("ORM_EXTENSION_TEST_JS", host.join("js/dist/src/index.js"))
            .env("ORM_EXTENSION_TEST_SCHEMA", &schema_path).env("ORM_EXTENSION_TEST_URL", &url).env("ORM_EXTENSION_GENERATED", output.join("generated.js")));
    }
    fs::remove_dir_all(temporary).unwrap();
}
