//! Behavioral extension setup. Database catalogs remain in `ext`.
pub use orm_contracts::extension::*;

#[cfg(feature = "composition")]
mod composition { include!(env!("ORM_CORE_COMPOSITION")); }

pub fn artifact() -> Artifact { compiled_artifact().clone() }

/// Every schema preparation checks the artifact; build it once per process.
fn compiled_artifact() -> &'static Artifact {
    static ARTIFACT: std::sync::OnceLock<Artifact> = std::sync::OnceLock::new();
    ARTIFACT.get_or_init(build_artifact)
}

fn build_artifact() -> Artifact {
    #[cfg(feature = "composition")]
    let mut artifact = composition::artifact();
    #[cfg(not(feature = "composition"))]
    let mut artifact = Artifact::default();
    let compiled = [
        (cfg!(feature = "file-storage"), "file-storage"),
        (cfg!(feature = "reference-loading"), "reference-loading"),
        (cfg!(feature = "model-composition"), "model-composition"),
        (cfg!(feature = "proxy-models"), "proxy-models"),
        (cfg!(feature = "generic-relations"), "generic-relations"),
        (cfg!(feature = "query-defaults"), QUERY_DEFAULTS),
    ];
    for (_, capability) in compiled.into_iter().filter(|(on, _)| *on) {
        if !artifact.capabilities.iter().any(|c| c == capability) { artifact.capabilities.push(capability.into()); }
    }
    artifact
}

/// The manifests compiled into this artifact; none without composition.
pub fn manifests() -> Vec<Manifest> {
    #[cfg(feature = "composition")]
    { composition::manifests() }
    #[cfg(not(feature = "composition"))]
    { Vec::new() }
}

pub fn prepare(ir: &mut crate::ir::SchemaIr, language: Option<&str>) -> Result<(), String> {
    if ir.behavior.storage.is_none() && (!ir.behavior.field_storage.is_empty() || !ir.behavior.owner_links.is_empty()) { return Err("storage ownership contributions require an explicit physical schema".into()); }
    #[cfg(feature = "composition")]
    { composition::prepare(ir, language)?; }
    #[cfg(not(feature = "composition"))]
    { validate_declarations(ir, &[], language)?; }
    check_requirements(ir, compiled_artifact())
}

#[cfg(feature = "composition")]
pub use composition::{bind, NativeModel};
