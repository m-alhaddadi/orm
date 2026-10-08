//! Column roles that the ORM maintains on writes: `updated_at`, soft delete and the
//! optimistic-locking version. Extension passes fill them; the host executes them.
use serde::{Deserialize, Serialize};

/// Who keeps the column current for writers outside the ORM.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// A row trigger, so every writer gets the behavior.
    #[default]
    Database,
    /// The ORM only; no DDL.
    Application,
}

impl Mode {
    pub fn parse(value: Option<&serde_json::Value>) -> Result<Self, String> {
        match value.map(|v| v.as_str()) {
            None => Ok(Self::Database),
            Some(Some("database")) => Ok(Self::Database),
            Some(Some("application")) => Ok(Self::Application),
            _ => Err("mode must be \"database\" or \"application\"".into()),
        }
    }
}

/// A field the ORM sets to the current time on each update.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpdatedAt {
    pub model: String,
    pub field: String,
    #[serde(default)]
    pub mode: Mode,
}

/// The nullable time field whose value marks a soft-deleted row.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SoftDelete {
    pub model: String,
    pub field: String,
    #[serde(default)]
    pub mode: Mode,
}

/// The integer field that each ORM update increments and an instance write checks.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VersionField {
    pub model: String,
    pub field: String,
}
