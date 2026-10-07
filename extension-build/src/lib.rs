//! Build frontend for fixed native composition. Cargo owns package resolution.
pub use orm_contracts::extension::QUERY_DEFAULTS;
use orm_contracts::extension::{AttributeTarget, Manifest, Pass, HOST_CONTRACT, SCHEMA_CONTRACT};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub struct Composition {
    pub manifests: Vec<Manifest>,
    pub passes: Vec<Pass>,
}

impl Composition {
    /// Whether any selected extension declares `capability`.
    pub fn has_capability(&self, capability: &str) -> bool {
        self.manifests.iter().any(|m| m.capabilities.iter().any(|c| c == capability))
    }

    /// Host Cargo features that the selected manifests turn on, in a stable order.
    pub fn host_features(&self) -> Vec<String> {
        orm_contracts::extension::HOST_FEATURE_CAPABILITIES.iter()
            .filter(|feature| self.has_capability(feature)).map(|feature| (*feature).to_owned()).collect()
    }

    pub fn resolve(mut manifests: Vec<Manifest>) -> Result<Self, String> {
        manifests.sort_by(|a, b| a.id.cmp(&b.id));
        let mut extensions = BTreeMap::new();
        let mut attributes = BTreeMap::new();
        let mut exports = BTreeSet::new();
        let mut passes = BTreeMap::new();
        let mut effects: BTreeMap<&str, &str> = BTreeMap::new();
        for m in &manifests {
            if m.host_contract != HOST_CONTRACT || m.schema_contract != SCHEMA_CONTRACT {
                return Err(format!("{}: incompatible host/schema contracts; rebuild", m.id));
            }
            for capability in &m.capabilities {
                if !orm_contracts::extension::HOST_CAPABILITIES.contains(&capability.as_str()) {
                    return Err(format!("{} requires unavailable host capability {capability}; rebuild with a compatible host contract", m.id));
                }
            }
            let version = semver::Version::parse(&m.version).map_err(|e| format!("{}: {e}", m.id))?;
            if extensions.insert(&m.id, version).is_some() {
                return Err(format!("duplicate extension {}", m.id));
            }
            for a in &m.attributes {
                if !a.name.contains('.') { return Err(format!("{}: attribute {} must be namespaced", m.id, a.name)); }
                if let Some(owner) = attributes.insert((&a.name, a.target == AttributeTarget::Field), &m.id) {
                    return Err(format!("attribute {} has duplicate owners {owner} and {}", a.name, m.id));
                }
            }
            for e in &m.exports {
                rust_path(&e.rust)?;
                if !exports.insert(&e.id) { return Err(format!("duplicate export {}", e.id)); }
            }
            for p in &m.passes {
                rust_path(&p.rust)?;
                if passes.insert(p.id.clone(), p.clone()).is_some() { return Err(format!("duplicate pass {}", p.id)); }
                for effect in &p.effects {
                    for (existing, owner) in &effects {
                        if *existing == effect || existing.starts_with(&format!("{effect}.")) || effect.starts_with(&format!("{existing}.")) {
                            return Err(format!("conflicting effect {effect} / {existing}: {owner} and {}", p.id));
                        }
                    }
                    effects.insert(effect, &p.id);
                }
            }
        }
        for m in &manifests {
            for (id, requirement) in &m.dependencies {
                let version = extensions.get(id).ok_or_else(|| format!("{} requires missing extension {id}", m.id))?;
                let requirement = semver::VersionReq::parse(requirement).map_err(|e| e.to_string())?;
                if !requirement.matches(version) { return Err(format!("{} requires {id}@{requirement}, resolved {version}", m.id)); }
            }
        }
        for p in passes.values() {
            for id in &p.after {
                let dependency = passes.get(id).ok_or_else(|| format!("{}: missing pass dependency {id}", p.id))?;
                if dependency.phase > p.phase { return Err(format!("{} depends on later phase {id}", p.id)); }
            }
        }
        let mut ordered = vec![];
        let mut complete = BTreeSet::new();
        while !passes.is_empty() {
            let earliest = passes.values().map(|p| p.phase).min().unwrap();
            let next = passes.values().find(|p| p.phase == earliest && p.after.iter().all(|id| complete.contains(id))).map(|p| p.id.clone());
            let Some(next) = next else { return Err(format!("pass dependency cycle: {:?}", passes.keys().collect::<Vec<_>>())); };
            complete.insert(next.clone());
            ordered.push(passes.remove(&next).unwrap());
        }
        Ok(Self { manifests, passes: ordered })
    }

    pub fn pass_ids(&self) -> Vec<&str> { self.passes.iter().map(|p| p.id.as_str()).collect() }

    /// The generated code uses calls checked by rustc, never an execution callback list.
    pub fn source(&self) -> Result<String, String> {
        self.source_with_native(None)
    }

    pub fn artifact(&self, native: Option<&native::Sources>) -> orm_contracts::extension::Artifact {
        let versions: BTreeMap<_, _> = self.manifests.iter().map(|m| (&m.id, &m.version)).collect();
        let artifact = orm_contracts::extension::Artifact {
            passes: self.pass_ids().into_iter().map(str::to_owned).collect(),
            capabilities: vec!["schema-transformations".into(), "physical-schema".into()],
            extensions: versions.iter().map(|(k,v)| ((*k).clone(), (*v).clone())).collect(),
            exports: self.manifests.iter().flat_map(|m| m.exports.iter().map(|e| e.id.clone())).collect(),
            ..Default::default()
        };
        let mut artifact = artifact;
        artifact.capabilities.extend(self.host_features());
        if let Some(native) = native {
            artifact.specializations = native.specializations.clone();
            artifact.capabilities.extend(["native-string-values", "native-string-records", "native-string-results"].into_iter().map(str::to_owned));
        }
        artifact
    }

    pub fn source_with_native(&self, native: Option<&native::Sources>) -> Result<String, String> {
        let manifests = serde_json::to_string(&self.manifests).map_err(|e| e.to_string())?;
        let ids = serde_json::to_string(&self.pass_ids()).map_err(|e| e.to_string())?;
        let versions: BTreeMap<_, _> = self.manifests.iter().map(|m| (&m.id, &m.version)).collect();
        let artifact = self.artifact(native);
        let artifact = serde_json::to_string(&artifact).map_err(|e| e.to_string())?;
        let binding = if native.is_some() { "bind(ir)?;" } else { "" };
        let mut source = format!(r#"
use crate::ir::SchemaIr;
use crate::behavior::{{Artifact, Manifest, SCHEMA_CONTRACT, validate_declarations, validate_requirements, capture_storage}};
pub fn artifact() -> Artifact {{ serde_json::from_str({artifact:?}).expect("generated artifact") }}
pub fn manifests() -> Vec<Manifest> {{ serde_json::from_str({manifests:?}).expect("generated manifests") }}
pub fn prepare(ir: &mut SchemaIr, language: Option<&str>) -> Result<(), String> {{
    let manifests = manifests();
    validate_declarations(ir, &manifests, language)?;
    let expected: Vec<String> = serde_json::from_str({ids:?}).expect("generated pass IDs");
    if !ir.behavior.completed_passes.is_empty() && ir.behavior.completed_passes != expected {{ return Err("incompatible lowering state; rebuild schema and native artifact".into()); }}
    if !ir.behavior.completed_passes.is_empty() && ir.behavior.declarations.iter().all(|d| d.lowered) && ir.models.iter().all(|m| ir.behavior.lowered_models.contains(&m.name)) {{
        {binding}
        validate_requirements(ir, &manifests, language)?;
        return Ok(());
    }}
"#);
        if !self.passes.is_empty() {
            source.push_str("    capture_storage(ir)?;\n    let previous: Vec<_> = ir.behavior.declarations.iter().filter(|d| d.lowered).cloned().collect();\n    ir.behavior.declarations.retain(|d| !d.lowered);\n    ir.behavior.completed_passes.clear();\n");
        }
        for p in &self.passes {
            source.push_str(&format!("    let () = {}(ir)?;\n    ir.behavior.completed_passes.push({:?}.into());\n", p.rust, p.id));
        }
        if !self.passes.is_empty() {
            source.push_str("    for declaration in &mut ir.behavior.declarations { declaration.lowered = true; }\n    ir.behavior.declarations.extend(previous);\n");
        }
        source.push_str(&format!(r#"    {binding}
    ir.behavior.lowered_models = ir.models.iter().map(|m| m.name.clone()).collect();
    if !ir.behavior.is_empty() {{
        ir.behavior.schema_contract = SCHEMA_CONTRACT;
        ir.behavior.extensions = serde_json::from_str({:?}).expect("generated versions");
    }}
    validate_requirements(ir, &manifests, language)?;
    Ok(())
}}
"#, serde_json::to_string(&versions).map_err(|e| e.to_string())?));
        if let Some(native) = native {
            source.push_str(&native.core);
        } else {
            source.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum NativeModel { None }\nimpl NativeModel { pub fn computed(self) -> &'static [usize] { &[] } pub fn validated(self) -> &'static [usize] { &[] } pub fn has_records(self) -> bool { false } pub fn dependency(self, _field: usize) -> Option<usize> { None } }\npub fn bind(ir: &mut SchemaIr) -> Result<Vec<NativeModel>, String> { Ok(vec![NativeModel::None; ir.models.len()]) }\n");
        }
        Ok(source)
    }
}

/// Reject source injection in metadata before emitting Rust paths.
pub fn rust_path(path: &str) -> Result<(), String> {
    if path.split("::").any(|part| {
        part.is_empty() || !part.bytes().enumerate().all(|(i, c)| c == b'_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
    }) { return Err(format!("invalid Rust export path {path:?}")); }
    Ok(())
}

pub mod native;

pub mod inputs;
