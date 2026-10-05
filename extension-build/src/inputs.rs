//! Content identity and Cargo rebuild guards for an exact composition.
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::{Path, PathBuf}};

#[derive(Default)]
pub struct Inputs {
    pub identities: BTreeMap<String, String>,
    pub files: BTreeMap<PathBuf, String>,
    directories: BTreeMap<PathBuf, Vec<String>>,
}
impl Inputs {
    pub fn file(&mut self, path: &Path) -> Result<(), String> {
        let path = path.canonicalize().map_err(|e| format!("{}: {e}", path.display()))?;
        let content = fs::read(&path).map_err(|e| e.to_string())?;
        self.files.insert(path, format!("{:x}", Sha256::digest(content)));
        Ok(())
    }
    /// File names are relative, so relocating equivalent sources preserves identity.
    pub fn tree(&mut self, identity: String, root: &Path) -> Result<(), String> {
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let mut paths = vec![];
        collect(&root, &mut paths, &mut self.directories)?;
        paths.sort();
        let mut hash = Sha256::new();
        for path in paths {
            let name = path.strip_prefix(&root).map_err(|e| e.to_string())?.to_string_lossy();
            // Cargo manifests contain physical path locations. Dependency identity is
            // represented by Cargo resolution, not by those machine-local locations.
            if name != "Cargo.toml" && name != "Cargo.lock" {
                let content = fs::read(&path).map_err(|e| e.to_string())?;
                hash.update((name.len() as u64).to_le_bytes()); hash.update(name.as_bytes());
                hash.update((content.len() as u64).to_le_bytes()); hash.update(&content);
            }
            self.file(&path)?;
        }
        self.identities.insert(identity, format!("{:x}", hash.finalize()));
        Ok(())
    }
    pub fn resolution(&mut self, metadata: &serde_json::Value, roots: &[String]) -> Result<(), String> {
        let nodes = metadata["resolve"]["nodes"].as_array().ok_or("missing Cargo resolution")?;
        let packages = metadata["packages"].as_array().ok_or("missing Cargo packages")?;
        let mut pending = roots.to_vec();
        let mut seen = std::collections::BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) { continue; }
            let node = nodes.iter().find(|n| n["id"].as_str() == Some(&id)).ok_or("missing resolved package node")?;
            let package = packages.iter().find(|p| p["id"].as_str() == Some(&id)).ok_or("missing resolved package")?;
            let key = format!("{}@{}:{}", package["name"].as_str().unwrap(), package["version"].as_str().unwrap(), package["source"].as_str().unwrap_or("local"));
            let descriptor = serde_json::json!({"name":package["name"], "version":package["version"], "source":package["source"], "features":node["features"]});
            self.identities.insert(format!("resolution:{key}"), format!("{:x}", Sha256::digest(serde_json::to_vec(&descriptor).map_err(|e| e.to_string())?)));
            if package["source"].is_null() && !package["name"].as_str().unwrap().starts_with("orm-app-") {
                self.tree(format!("source:{key}"), Path::new(package["manifest_path"].as_str().unwrap()).parent().unwrap())?;
            }
            for dep in node["dependencies"].as_array().ok_or("missing dependencies")? { pending.push(dep.as_str().ok_or("invalid dependency ID")?.into()); }
        }
        Ok(())
    }
    /// A build script detects stale inputs and requests frontend regeneration. It
    /// adds no dependencies, feature flags, recursive build, or runtime machinery.
    pub fn guard_data(&self, composition: &Path) -> Result<Vec<u8>, String> {
        serde_json::to_vec_pretty(&serde_json::json!({"composition":composition,"files":self.files,"directories":self.directories})).map_err(|e|e.to_string())
    }
    pub fn guard(&self, _composition: &Path) -> String {
        // Identical source across profiles. A cached build-script executable must
        // read the current workspace's inputs, never compiled-in paths from another.
        r#"use sha2::{Digest, Sha256};
fn main() {
    println!("cargo:rerun-if-env-changed=ORM_CORE_COMPOSITION");
    let base = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let file = base.join("composition-inputs.json");
    println!("cargo:rerun-if-changed={}", file.display());
    let data: serde_json::Value = serde_json::from_slice(&std::fs::read(file).expect("composition inputs unavailable; regenerate composition")).unwrap();
    let composition = std::env::var("ORM_CORE_COMPOSITION").unwrap_or_else(|_|data["composition"].as_str().unwrap().to_owned());
    println!("cargo:rustc-env=ORM_CORE_COMPOSITION={composition}");
    println!("cargo:rerun-if-changed={composition}");
    for (path,digest) in data["files"].as_object().unwrap() {
        println!("cargo:rerun-if-changed={path}");
        let input=std::fs::read(path).expect("extension build input unavailable; regenerate composition");
        assert_eq!(format!("{:x}",Sha256::digest(input)),digest.as_str().unwrap(),"extension build inputs changed; rerun orm-extension-build with a fresh output directory");
    }
    for (path,expected) in data["directories"].as_object().unwrap() {
        println!("cargo:rerun-if-changed={path}");
        let mut names: Vec<_> = std::fs::read_dir(path).expect("extension source directory unavailable").map(|e|e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| !matches!(n.as_str(), "target" | ".git" | "node_modules" | "__pycache__" | ".mypy_cache" | ".pytest_cache")).collect();
        names.sort();
        assert_eq!(serde_json::json!(names),*expected,"extension source files changed; rerun orm-extension-build with a fresh output directory");
    }
}
"#.to_owned()
    }

}
fn collect(root: &Path, paths: &mut Vec<PathBuf>, directories: &mut BTreeMap<PathBuf, Vec<String>>) -> Result<(), String> {
    let mut names = vec![];
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if matches!(name.to_str(), Some("target" | ".git" | "node_modules" | "__pycache__" | ".mypy_cache" | ".pytest_cache")) { continue; }
        names.push(name.to_string_lossy().into_owned());
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() { collect(&entry.path(), paths, directories)?; }
        else if kind.is_file() { paths.push(entry.path()); }
    }
    names.sort();
    directories.insert(root.to_path_buf(), names);
    Ok(())
}
