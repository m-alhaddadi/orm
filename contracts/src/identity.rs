//! Persisted application model identities. Allocation belongs to explicit generation.
use std::collections::BTreeSet;
use serde::{Deserialize, Serialize};

pub const IDENTITY_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelIdentity {
    pub id: i32,
    pub model: String,
    pub retired: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_names: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdentityManifest {
    pub version: u32,
    pub entries: Vec<ModelIdentity>,
}
impl Default for IdentityManifest {
    fn default() -> Self { Self { version: IDENTITY_VERSION, entries: vec![] } }
}
impl IdentityManifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != IDENTITY_VERSION { return Err(format!("unsupported identity manifest version {}", self.version)); }
        let mut ids = BTreeSet::new();
        let mut active = BTreeSet::new();
        for e in &self.entries {
            if e.id <= 0 || !ids.insert(e.id) { return Err(format!("model {} has invalid or duplicate ContentType ID {}", e.model, e.id)); }
            if e.model.is_empty() || e.previous_names.iter().any(|n| n.is_empty() || n == &e.model) {
                return Err(format!("invalid identity name/history for {}", e.model));
            }
            let unique: BTreeSet<_> = e.previous_names.iter().collect();
            if unique.len() != e.previous_names.len() { return Err(format!("duplicate rename history for {}", e.model)); }
            if !e.retired && !active.insert(&e.model) { return Err(format!("duplicate active identity for {}", e.model)); }
        }
        Ok(())
    }
    pub fn active(&self) -> impl Iterator<Item = &ModelIdentity> { self.entries.iter().filter(|e| !e.retired) }

    /// Prior allocations, including tombstones, must remain in every snapshot.
    pub fn validate_successor(&self, next: &Self) -> Result<(), String> {
        self.validate()?; next.validate()?;
        for old in &self.entries {
            let new = next.entries.iter().find(|e| e.id == old.id)
                .ok_or_else(|| format!("ContentType ID {} was removed; retain its tombstone", old.id))?;
            if old.model != new.model && !new.previous_names.contains(&old.model) {
                return Err(format!("ContentType ID {} reassigned from {} to {}; use explicit identity rename", old.id, old.model, new.model));
            }
            if old.previous_names.iter().any(|n| n != &new.model && !new.previous_names.contains(n)) {
                return Err(format!("ContentType ID {} lost its rename history", old.id));
            }
        }
        Ok(())
    }
}
