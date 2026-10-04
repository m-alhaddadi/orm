//! Code generation from the schema, one module per language.
pub mod python;
pub mod typescript;

/// Compact schema payload for self-contained generated modules in every language.
/// Compilation and validation stay in Rust; runtimes receive the same schema IR.
pub fn embedded_schema_json(ir: &crate::ir::SchemaIr) -> Result<String, String> {
    serde_json::to_string(ir).map_err(|e| e.to_string())
}
