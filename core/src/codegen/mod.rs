//! Code generation from the schema, one module per language.
#[cfg(feature = "generate-python")]
pub mod python;
#[cfg(feature = "generate-typescript")]
pub mod typescript;

/// Compact schema payload for self-contained generated modules in every language.
/// Compilation and validation stay in Rust; runtimes receive the same schema IR.
pub fn embedded_schema_json(ir: &crate::ir::SchemaIr) -> Result<String, String> {
    serde_json::to_string(ir).map_err(|e| e.to_string())
}

/// A prepared default filter on `target` can hide it, so a reference to it can be absent.
#[cfg(feature = "reference-loading")]
pub fn reference_target_filtered(ir: &crate::ir::SchemaIr, target: &str) -> bool {
    let behavior = serde_json::to_value(&ir.behavior).expect("serializable schema behavior");
    behavior.get("query_defaults").and_then(serde_json::Value::as_array).is_some_and(|policies| {
        policies.iter().any(|policy| policy.get("model").and_then(serde_json::Value::as_str) == Some(target)
            && policy.get("filter").is_some_and(|filter| !filter.is_null()))
    })
}

/// The loader result is optional for a reverse reference, a nullable key, or a filtered target.
#[cfg(feature = "reference-loading")]
pub fn reference_loader_nullable(ir: &crate::ir::SchemaIr, model: &crate::schema::Model, r: &crate::ir::RelationIr) -> Result<bool, String> {
    Ok(!r.foreign_key || model.field(&r.from)?.nullable || reference_target_filtered(ir, &r.target))
}

#[cfg(test)]
mod reference_tests {
    use super::*;
    fn schema() -> (crate::ir::SchemaIr, crate::schema::Schema) {
        let ir = crate::dsl::compile("model User {\n id Int @id\n posts Post[]\n}\nmodel Post {\n id Int @id\n author_id Int?\n author User? @relation(fields: [author_id], references: [id])\n}", None).unwrap();
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        (ir, schema)
    }
    #[test]
    fn loader_generation_matches_artifact_selection() {
        let (ir, schema) = schema();
        let py = python::generate(&ir, &schema, "schema.prisma").unwrap();
        let ts = typescript::generate(&ir, &schema, "schema.prisma", "orm").unwrap();
        if cfg!(feature = "reference-loading") {
            assert!(py.stub.contains("async def load_author(self, *, reload: bool = False) -> User | None"));
            assert!(ts.contains("loadAuthor(options?: { readonly reload?: boolean }): Promise<User | null>"));
            assert!(py.module.contains("required_capabilities=(\"reference-loading\",)"));
            assert!(ts.contains("requiredCapabilities: [\"reference-loading\"]"));
        } else {
            assert!(!py.stub.contains("load_author"));
            assert!(!ts.contains("loadAuthor"));
            assert!(!py.module.contains("reference-loading"));
            assert!(!ts.contains("reference-loading"));
        }
        assert!(!py.stub.contains("load_posts"));
        assert!(!ts.contains("loadPosts"));
    }
    #[cfg(feature = "reference-loading")]
    #[test]
    fn loader_collisions_rejected_by_both_generators() {
        let (mut ir, _) = schema();
        let mut field = ir.models[1].fields[0].clone();
        field.name = "load_author".into(); field.column = "load_author".into(); field.primary_key = false;
        ir.models[1].fields.push(field);
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        assert!(python::generate(&ir, &schema, "schema.prisma").err().unwrap().contains("collides"));
        assert!(typescript::generate(&ir, &schema, "schema.prisma", "orm").err().unwrap().contains("collides"));
    }
}

/// A proxy client default lets an insert omit the field.
fn has_client_default(ir: &crate::ir::SchemaIr, model: &str, field: &str) -> bool {
    ir.behavior.proxy_models.iter().any(|p| p.model == model && p.defaults.contains_key(field))
}
