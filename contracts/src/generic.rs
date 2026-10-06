//! Generic relation setup data; runtime preparation resolves numeric host positions.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenericRelation {
    pub model: String,
    pub name: String,
    pub type_field: String,
    pub key_field: String,
    /// Concrete target model names in canonical order. The schema manifest supplies IDs.
    pub targets: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenericReverse {
    pub model: String,
    pub name: String,
    pub source: String,
    pub relation: String,
}

#[derive(Clone, Debug)]
pub struct PreparedGenericRelation {
    pub model: crate::extension::ModelId,
    pub name: String,
    pub discriminator: crate::extension::FieldId,
    pub key: crate::extension::FieldId,
    pub nullable: bool,
    pub targets: Vec<GenericTarget>,
}
#[derive(Clone, Copy, Debug)]
pub struct GenericTarget {
    pub content_type: i32,
    pub model: crate::extension::ModelId,
    pub key: crate::extension::FieldId,
}
#[derive(Clone, Debug)]
pub struct PreparedGenericReverse {
    pub model: crate::extension::ModelId,
    pub name: String,
    pub source: crate::extension::ModelId,
    pub relation: usize,
    pub content_type: i32,
}
