//! Prepared column roles of the `updated-at`, `soft-delete` and `optimistic-locking`
//! extensions. A build without a feature rejects schemas that use it.
use std::collections::HashMap;

use crate::behavior::Requirements;
#[cfg_attr(not(any(feature = "updated-at", feature = "soft-delete", feature = "optimistic-locking")), allow(unused_imports))]
use crate::ir::ColType;
use crate::schema::{Model, Result};

#[cfg_attr(not(any(feature = "updated-at", feature = "soft-delete", feature = "optimistic-locking")), allow(dead_code))]
fn position(models: &[Model], index: &HashMap<String, usize>, model: &str, field: &str, what: &str, ok: impl Fn(&crate::ir::FieldIr) -> bool) -> Result<(usize, usize)> {
    let m = *index.get(model).ok_or_else(|| format!("{what}: unknown model {model}; rebuild schema"))?;
    let p = models[m].field_pos(field)?;
    let f = &models[m].fields()[p];
    if f.array || !ok(f) { return Err(format!("{model}.{field}: wrong type for {what}; rebuild schema")); }
    Ok((m, p))
}

#[cfg_attr(not(any(feature = "updated-at", feature = "soft-delete", feature = "optimistic-locking")), allow(unused_variables))]
pub fn prepare(models: &mut [Model], index: &HashMap<String, usize>, r: &Requirements) -> Result<()> {
    #[cfg(not(feature = "updated-at"))]
    if !r.updated_at.is_empty() { return Err("@timestamps.updated_at requires an enabled updated-at artifact; rebuild".into()); }
    #[cfg(feature = "updated-at")]
    for u in &r.updated_at {
        let (m, p) = position(models, index, &u.model, &u.field, "updated_at", |f| f.ty == ColType::DateTime)?;
        models[m].updated_at.push(p);
    }
    #[cfg(not(feature = "soft-delete"))]
    if !r.soft_delete.is_empty() { return Err("@soft_delete.deleted_at requires an enabled soft-delete artifact; rebuild".into()); }
    #[cfg(feature = "soft-delete")]
    for s in &r.soft_delete {
        let (m, p) = position(models, index, &s.model, &s.field, "soft delete", |f| f.ty == ColType::DateTime && f.nullable)?;
        if models[m].soft_delete.replace(p).is_some() { return Err(format!("{}: one soft-delete field only", s.model)); }
    }
    #[cfg(not(feature = "optimistic-locking"))]
    if !r.versions.is_empty() { return Err("@locking.version requires an enabled optimistic-locking artifact; rebuild".into()); }
    #[cfg(feature = "optimistic-locking")]
    for v in &r.versions {
        let (m, p) = position(models, index, &v.model, &v.field, "version", |f| matches!(f.ty, ColType::Int | ColType::BigInt))?;
        if models[m].version.replace(p).is_some() { return Err(format!("{}: one version field only", v.model)); }
    }
    Ok(())
}
