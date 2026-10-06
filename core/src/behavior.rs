//! Behavioral extension setup. Database catalogs remain in `ext`.
pub use orm_contracts::extension::*;

#[cfg(feature = "composition")]
mod composition { include!(env!("ORM_CORE_COMPOSITION")); }

pub fn artifact() -> Artifact {
    #[cfg(feature = "composition")]
    #[allow(unused_mut)]
    let mut artifact = composition::artifact();
    #[cfg(not(feature = "composition"))]
    #[allow(unused_mut)]
    let mut artifact = Artifact::default();
    #[cfg(feature = "proxy-models")]
    artifact.capabilities.push("proxy-models".into());
    artifact
}

pub fn prepare(ir: &mut crate::ir::SchemaIr, language: Option<&str>) -> Result<(), String> {
    if ir.behavior.storage.is_none() && (!ir.behavior.field_storage.is_empty() || !ir.behavior.owner_links.is_empty()) { return Err("storage ownership contributions require an explicit physical schema".into()); }
    #[cfg(feature = "composition")]
    { composition::prepare(ir, language)?; }
    #[cfg(not(feature = "composition"))]
    { validate_declarations(ir, &[], language)?; }
    check_requirements(ir, &artifact())
}

#[cfg(feature = "composition")]
pub use composition::{bind, NativeModel};
