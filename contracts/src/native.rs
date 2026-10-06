//! Typed native profile configuration shared by extension authors and the compiler.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FieldRule {
    pub field: String,
    #[serde(default)] pub transforms: Vec<String>,
    #[serde(default)] pub validators: Vec<String>,
    #[serde(default)] pub allow_null: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Computed {
    pub field: String,
    pub dependency: String,
    pub export: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordRule {
    pub dependencies: Vec<String>,
    pub export: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Method {
    pub name: String,
    pub export: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSpec {
    pub model: String,
    #[serde(default)] pub fields: Vec<FieldRule>,
    #[serde(default)] pub computed: Vec<Computed>,
    #[serde(default)] pub records: Vec<RecordRule>,
    #[serde(default)] pub methods: Vec<Method>,
}

