//! `orm-extension-build CONFIG.json`: resolve selected Cargo dependencies, emit fixed
//! composition, then build the ordinary bindings in an isolated workspace.
use orm_extension_build::Composition;
use orm_contracts::extension::Manifest;
use serde::Deserialize;
use std::{collections::BTreeMap, fs, path::{Path, PathBuf}, process::Command};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedSchema {
    schema: PathBuf,
    models: Vec<orm_extension_build::native::NativeSpec>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalModule {
    alias: String,
    source: PathBuf,
    manifest: Manifest,
    #[serde(default)]
    dependencies: BTreeMap<String, toml::Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    host: PathBuf,
    output: PathBuf,
    #[serde(default)]
    dependencies: BTreeMap<String, toml::Value>,
    #[serde(default = "bindings")]
    bindings: Vec<String>,
    #[serde(default)]
    binding_features: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    offline: bool,
    #[serde(default)]
    prepare_only: bool,
    #[serde(default)]
    specialization: Option<SelectedSchema>,
    #[serde(default)]
    modules: Vec<LocalModule>,
    #[serde(default)]
    inputs: Vec<PathBuf>,
}
fn bindings() -> Vec<String> { vec!["python".into(), "node".into()] }

fn add_feature(manifest: &mut toml::Value, path: &[&str], feature: &str) -> Result<(), String> {
    let mut table = manifest;
    for key in &path[..path.len()-1] {
        table = table.get_mut(*key).ok_or_else(|| format!("missing manifest table {key}"))?;
    }
    let values = table.as_table_mut().ok_or("expected manifest table")?
        .entry(path[path.len()-1]).or_insert_with(|| toml::Value::Array(vec![]))
        .as_array_mut().ok_or("expected feature array")?;
    if !values.iter().any(|value| value.as_str() == Some(feature)) {
        values.push(toml::Value::String(feature.into()));
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let destination = to.join(entry.file_name());
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else { fs::copy(entry.path(), destination).map_err(|e| e.to_string())?; }
    }
    Ok(())
}
fn run(command: &mut Command) -> Result<(), String> {
    let status = command.status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("{command:?} failed: {status}")) }
}
fn main() {
    if let Err(e) = build() { eprintln!("extension build: {e}"); std::process::exit(1); }
}
fn build() -> Result<(), String> {
    let config_path = PathBuf::from(std::env::args().nth(1).ok_or("usage: orm-extension-build CONFIG.json")?).canonicalize().map_err(|e| e.to_string())?;
    let base = config_path.parent().unwrap();
    let mut config: Config = serde_json::from_slice(&fs::read(&config_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    config.host = base.join(&config.host).canonicalize().map_err(|e| e.to_string())?;
    config.output = base.join(&config.output);
    if config.output.exists() { return Err(format!("output {} already exists; choose an empty build directory", config.output.display())); }
    if config.bindings.is_empty() || config.bindings.iter().any(|b| b != "python" && b != "node") { return Err("bindings must contain python and/or node".into()); }
    // Paths in configuration are relative to its file, never to the generated workspace.
    for dependency in config.dependencies.values_mut() {
        if let Some(path) = dependency.get_mut("path") {
            let resolved = base.join(path.as_str().ok_or("dependency path must be a string")?).canonicalize().map_err(|e| e.to_string())?;
            *path = toml::Value::String(resolved.to_string_lossy().into());
        }
    }
    fs::create_dir_all(&config.output).map_err(|e| e.to_string())?;
    config.output = config.output.canonicalize().map_err(|e| e.to_string())?;
    if ["contracts", "core", "engine", "cli", "bindings"].iter().any(|dir| config.output.starts_with(config.host.join(dir))) {
        return Err("output must be outside the host source directories".into());
    }
    for module in &config.modules {
        orm_extension_build::rust_path(&module.alias)?;
        if module.alias.contains("::") || config.dependencies.contains_key(&module.alias) { return Err(format!("invalid or duplicate module alias {}", module.alias)); }
        let source = base.join(&module.source).canonicalize().map_err(|e| e.to_string())?;
        let folder = config.output.join("modules").join(&module.alias);
        fs::create_dir_all(folder.join("src")).map_err(|e| e.to_string())?;
        let mut dependencies = module.dependencies.clone();
        for dependency in dependencies.values_mut() {
            if let Some(path) = dependency.get_mut("path") {
                let resolved = base.join(path.as_str().ok_or("module dependency path must be a string")?).canonicalize().map_err(|e| e.to_string())?;
                *path = toml::Value::String(resolved.to_string_lossy().into());
            }
        }
        if dependencies.contains_key("orm-contracts") { return Err("local module SDK dependency is supplied by the host".into()); }
        dependencies.insert("orm-contracts".into(), toml::Value::try_from(serde_json::json!({"path":config.host.join("contracts")})).map_err(|e| e.to_string())?);
        let package = serde_json::json!({"package": {"name":format!("orm-app-{}",module.alias.replace('_', "-")), "version":module.manifest.version, "edition":"2021", "metadata":{"orm-extension":module.manifest}}, "dependencies":dependencies});
        fs::write(folder.join("Cargo.toml"), toml::to_string(&package).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        fs::write(folder.join("src/lib.rs"), format!("#[path = {:?}] mod application;\npub use application::*;\n", source.to_string_lossy())).map_err(|e| e.to_string())?;
        config.dependencies.insert(module.alias.clone(), toml::Value::try_from(serde_json::json!({"path":folder, "package":format!("orm-app-{}",module.alias.replace('_', "-"))})).map_err(|e| e.to_string())?);
    }
    for dir in ["core", "engine", "cli"] { copy_tree(&config.host.join(dir), &config.output.join(dir))?; }
    for binding in &config.bindings { copy_tree(&config.host.join("bindings").join(binding), &config.output.join("bindings").join(binding))?; }
    for (binding, selected) in &config.binding_features {
        if !config.bindings.contains(binding) { return Err(format!("binding_features names unselected binding {binding}")); }
        let path = config.output.join("bindings").join(binding).join("Cargo.toml");
        let mut manifest: toml::Value = toml::from_str(&fs::read_to_string(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        for feature in selected {
            if feature == "default" || manifest["features"].get(feature).is_none() {
                return Err(format!("{binding}: unknown or non-exact host feature {feature}"));
            }
        }
        manifest["features"]["default"] = toml::Value::Array(selected.iter().cloned().map(toml::Value::String).collect());
        fs::write(path, toml::to_string(&manifest).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    let mut root: toml::Value = toml::from_str(&fs::read_to_string(config.host.join("Cargo.toml")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    root["workspace"]["members"] = toml::Value::Array(["core", "engine", "cli"].into_iter().map(|x| toml::Value::String(x.into())).chain(config.bindings.iter().map(|x| toml::Value::String(format!("bindings/{x}")))).collect());
    fs::write(config.output.join("Cargo.toml"), toml::to_string(&root).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    fs::copy(config.host.join("Cargo.lock"), config.output.join("Cargo.lock")).map_err(|e| e.to_string())?;
    let core_path = config.output.join("core/Cargo.toml");
    let mut core: toml::Value = toml::from_str(&fs::read_to_string(&core_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    core["dependencies"]["orm-contracts"]["path"] = toml::Value::String(config.host.join("contracts").to_string_lossy().into());
    for (alias, dependency) in &config.dependencies {
        orm_extension_build::rust_path(&alias.replace('-', "_"))?;
        if alias.contains("::") || core["dependencies"].get(alias).is_some() { return Err(format!("invalid or occupied dependency alias {alias}")); }
        if dependency.get("optional").and_then(|v| v.as_bool()) == Some(true) { return Err(format!("{alias}: selected composition dependencies cannot be optional")); }
        core["dependencies"].as_table_mut().unwrap().insert(alias.clone(), dependency.clone());
    }
    // Cargo sees the complete dependency manifest before source generation.
    fs::write(&core_path, toml::to_string(&core).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let mut metadata_cmd = Command::new("cargo");
    metadata_cmd.args(["metadata", "--format-version", "1", "--manifest-path"]).arg(config.output.join("Cargo.toml"));
    if config.offline { metadata_cmd.arg("--offline"); }
    let metadata = metadata_cmd.output().map_err(|e| e.to_string())?;
    if !metadata.status.success() { return Err(String::from_utf8_lossy(&metadata.stderr).into()); }
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout).map_err(|e| e.to_string())?;
    let packages = metadata["packages"].as_array().ok_or("Cargo metadata missing packages")?;
    let core_package = packages.iter().find(|p| p["name"] == "orm-core").ok_or("Cargo metadata missing host")?;
    let nodes = metadata["resolve"]["nodes"].as_array().ok_or("Cargo metadata missing resolution")?;
    let node = nodes.iter().find(|n| n["id"] == core_package["id"]).ok_or("Cargo metadata missing host resolution")?;
    let mut manifests = vec![];
    let mut roots = vec![];
    let mut inputs = orm_extension_build::inputs::Inputs::default();
    inputs.file(&config_path)?;
    for input in &config.inputs { inputs.file(&base.join(input))?; }
    for module in &config.modules { inputs.tree(format!("application:{}", module.manifest.id), base.join(&module.source).parent().ok_or("module source has no parent")?)?; }
    for alias in config.dependencies.keys() {
        let dep = node["deps"].as_array().unwrap().iter().find(|d| d["name"].as_str() == Some(&alias.replace('-', "_"))).ok_or_else(|| format!("unresolved dependency {alias}"))?;
        let package = packages.iter().find(|p| p["id"] == dep["pkg"]).ok_or("missing resolved extension package")?;
        roots.push(package["id"].as_str().unwrap().to_owned());
        let value = &package["metadata"]["orm-extension"];
        let mut manifest: Manifest = if let Some(resource) = value.get("manifest").and_then(|v| v.as_str()) {
            let package_path = Path::new(package["manifest_path"].as_str().unwrap()).parent().unwrap();
            let path = package_path.join(resource).canonicalize().map_err(|e| e.to_string())?;
            if !path.starts_with(package_path) { return Err(format!("{alias}: metadata resource must be packaged with its crate")); }
            inputs.file(&path)?;
            toml::from_str(&fs::read_to_string(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?
        } else { serde_json::from_value(value.clone()).map_err(|e| format!("{alias}: invalid package.metadata.orm-extension: {e}"))? };
        if Some(manifest.version.as_str()) != package["version"].as_str() { return Err(format!("{alias}: extension version differs from Cargo package version")); }
        // Paths use Cargo's dependency alias. No extension identity branch is added to the host.
        let rust_alias = alias.replace('-', "_");
        let crate_name = package["name"].as_str().unwrap().replace('-', "_");
        for path in manifest.passes.iter_mut().map(|p| &mut p.rust).chain(manifest.exports.iter_mut().map(|e| &mut e.rust)) {
            let (prefix, suffix) = path.split_once("::").ok_or_else(|| format!("{alias}: export path must name a crate and function"))?;
            if prefix != rust_alias && prefix != crate_name && prefix != "crate" {
                return Err(format!("{alias}: Rust path {path} must name its owning crate"));
            }
            *path = format!("{rust_alias}::{suffix}");
        }
        manifests.push(manifest);
    }
    inputs.resolution(&metadata, &roots)?;
    let composition = Composition::resolve(manifests)?;
    let native = if let Some(selected) = &config.specialization {
        inputs.file(&base.join(&selected.schema))?;
        fs::write(config.output.join("composition.rs"), composition.source()?).map_err(|e| e.to_string())?;
        fs::create_dir_all(config.output.join("core/examples")).map_err(|e| e.to_string())?;
        fs::write(config.output.join("core/examples/orm_extension_normalize.rs"), r#"
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    let path = std::path::Path::new(&args[1]);
    let mut ir = if path.extension().is_some_and(|e| e == "json") {
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?
    } else {
        let project = orm_core::dsl::compile_project_file(path)?;
        std::fs::write(&args[3], serde_json::to_vec(&project.inputs).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        project.ir
    };
    orm_core::behavior::prepare(&mut ir, None)?;
    let json = serde_json::to_vec(&ir).map_err(|e| e.to_string())?;
    orm_core::schema::Schema::from_ir(serde_json::from_slice(&json).map_err(|e| e.to_string())?)?;
    std::fs::write(&args[2], json).map_err(|e| e.to_string())
}
"#).map_err(|e| e.to_string())?;
        let normalized = config.output.join("normalized.schema.json");
        let schema_inputs = config.output.join("schema-inputs.json");
        let mut command = Command::new("cargo");
        command.args(["run", "--quiet", "--locked", "--manifest-path"]).arg(config.output.join("Cargo.toml"))
            .args(["-p", "orm-core", "--features", "composition", "--example", "orm_extension_normalize"]);
        if config.offline { command.arg("--offline"); }
        command.arg("--").arg(base.join(&selected.schema)).arg(&normalized).arg(&schema_inputs)
            .env("ORM_CORE_COMPOSITION", config.output.join("composition.rs"));
        run(&mut command)?;
        if schema_inputs.exists() {
            let paths: Vec<PathBuf> = serde_json::from_slice(&fs::read(schema_inputs).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            for path in paths { inputs.file(&path)?; }
        }
        let mut schema = serde_json::from_slice(&fs::read(normalized).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        Some(orm_extension_build::native::generate_with_sources(&mut schema, &selected.models, &composition.manifests, &inputs.identities)?)
    } else if !config.dependencies.is_empty() {
        let mut schema = serde_json::from_value(serde_json::json!({"models":[]})).map_err(|e| e.to_string())?;
        Some(orm_extension_build::native::generate(&mut schema, &[], &composition.manifests)?)
    } else { None };
    fs::write(config.output.join("composition.rs"), composition.source_with_native(native.as_ref())?).map_err(|e| e.to_string())?;
    fs::write(config.output.join("artifact.json"), serde_json::to_string_pretty(&composition.artifact(native.as_ref())).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    fs::write(config.output.join("manifests.json"), serde_json::to_string_pretty(&composition.manifests).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if let Some(native) = &native {
        fs::write(config.output.join("python-methods.rs"), &native.python).map_err(|e| e.to_string())?;
        fs::write(config.output.join("node-methods.rs"), &native.node).map_err(|e| e.to_string())?;
        fs::write(config.output.join("engine-composition.rs"), &native.engine).map_err(|e| e.to_string())?;
        let path = config.output.join("engine/Cargo.toml");
        let mut engine: toml::Value = toml::from_str(&fs::read_to_string(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        for (alias, dependency) in &config.dependencies { engine["dependencies"].as_table_mut().unwrap().insert(alias.clone(), dependency.clone()); }
        fs::write(path, toml::to_string(&engine).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    if !config.dependencies.is_empty() {
        // Features are selected through manifests, never through build-script cfg tricks.
        for binding in &config.bindings {
            let path = config.output.join("bindings").join(binding).join("Cargo.toml");
            let mut manifest: toml::Value = toml::from_str(&fs::read_to_string(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            if native.is_some() {
                for (alias, dependency) in &config.dependencies { manifest["dependencies"].as_table_mut().unwrap().insert(alias.clone(), dependency.clone()); }
                add_feature(&mut manifest, &["features", "default"], "composition")?;
                add_feature(&mut manifest, &["dependencies", "orm-engine", "features"], "composition")?;
            }
            add_feature(&mut manifest, &["dependencies", "orm-core", "features"], "composition")?;
            fs::write(path, toml::to_string(&manifest).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        }
    }
    if native.is_some() {
        let mut lock = Command::new("cargo");
        lock.args(["metadata", "--format-version", "1", "--manifest-path"]).arg(config.output.join("Cargo.toml"));
        if config.offline { lock.arg("--offline"); }
        let result = lock.output().map_err(|e| e.to_string())?;
        if !result.status.success() { return Err(String::from_utf8_lossy(&result.stderr).into()); }
    }
    if !config.dependencies.is_empty() {
        let mut core: toml::Value = toml::from_str(&fs::read_to_string(&core_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        core.as_table_mut().unwrap().insert("build-dependencies".into(), toml::Value::try_from(serde_json::json!({"sha2":"0.10","serde_json":"1"})).map_err(|e| e.to_string())?);
        fs::write(&core_path, toml::to_string(&core).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        fs::write(config.output.join("core/composition-inputs.json"), inputs.guard_data(&config.output.join("composition.rs"))?).map_err(|e|e.to_string())?;
        fs::write(config.output.join("core/build.rs"), orm_extension_build::inputs::Inputs::guard_source()).map_err(|e| e.to_string())?;
        fs::write(config.output.join("source-identities.json"), serde_json::to_vec_pretty(&inputs.identities).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    if !config.dependencies.is_empty() {
        let mut lock = Command::new("cargo");
        lock.args(["metadata", "--format-version", "1", "--manifest-path"]).arg(config.output.join("Cargo.toml"));
        if config.offline { lock.arg("--offline"); }
        let result = lock.output().map_err(|e| e.to_string())?;
        if !result.status.success() { return Err(String::from_utf8_lossy(&result.stderr).into()); }
    }
    // All source/configuration inputs live in the output snapshot; Cargo tracks source
    // dependencies and the env! include path. A fresh output is required for changes.
    fs::copy(&config_path, config.output.join("build-input.json")).map_err(|e| e.to_string())?;
    if !config.prepare_only {
        let mut command = Command::new("cargo");
        command.args(["build", "--release", "--locked", "--manifest-path"]).arg(config.output.join("Cargo.toml"));
        for binding in &config.bindings { command.args(["-p", &format!("orm-{binding}")]); }
        if config.offline { command.arg("--offline"); }
        command.env("ORM_CORE_COMPOSITION", config.output.join("composition.rs"));
        command.env("ORM_ENGINE_COMPOSITION", config.output.join("engine-composition.rs"));
        command.env("ORM_PYTHON_METHODS", config.output.join("python-methods.rs"));
        command.env("ORM_NODE_METHODS", config.output.join("node-methods.rs"));
        run(&mut command)?;
    }
    println!("{}", config.output.display());
    Ok(())
}
