//! Code generation from the schema, one module per language.
#[cfg(feature = "generate-python")]
pub mod python;
#[cfg(feature = "generate-typescript")]
pub mod typescript;

/// Generator options that are not part of the schema.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// The user's query-set class per model: Python `module:Class`, TypeScript
    /// `specifier#Export`.
    pub query_sets: std::collections::BTreeMap<String, String>,
}

impl Options {
    /// The query sets split at `sep` into (module, class), checked against the schema.
    pub fn query_sets(&self, schema: &crate::schema::Schema, sep: char) -> Result<std::collections::BTreeMap<&str, (&str, &str)>, String> {
        let mut out = std::collections::BTreeMap::new();
        for (model, target) in &self.query_sets {
            if !schema.models.iter().any(|m| m.ir.name == *model) {
                return Err(format!("--query-set {model}: the schema has no model {model}"));
            }
            let parts = target.split_once(sep).filter(|(m, c)| !m.is_empty() && is_ident(c));
            let Some((module, class)) = parts else {
                return Err(format!("--query-set {model}={target}: expected {}", if sep == ':' { "module:Class" } else { "specifier#Export" }));
            };
            if sep == ':' && !module.split('.').all(is_ident) {
                return Err(format!("--query-set {model}={target}: {module} is not an absolute module name"));
            }
            out.insert(model.as_str(), (module, class));
        }
        Ok(out)
    }
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_ascii_alphabetic()) && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

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
        // Declared in the source, so a composition build also captures its physical column.
        let ir = crate::dsl::compile("model User {\n id Int @id\n posts Post[]\n}\nmodel Post {\n id Int @id\n load_author Int\n author_id Int?\n author User? @relation(fields: [author_id], references: [id])\n}", None).unwrap();
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        assert!(python::generate(&ir, &schema, "schema.prisma").err().unwrap().contains("collides"));
        assert!(typescript::generate(&ir, &schema, "schema.prisma", "orm").err().unwrap().contains("collides"));
    }

    #[test]
    fn query_sets_reach_both_generators() {
        let (ir, schema) = schema();
        let mut options = Options::default();
        options.query_sets.insert("Post".into(), "app.queries:PostQueries".into());
        let py = python::generate_with(&ir, &schema, "schema.prisma", &options).unwrap();
        assert!(py.module.contains("use_query_set(Post, \"app.queries:PostQueries\")"));
        assert!(py.stub.contains("from app.queries import PostQueries as _PostObjects") && py.stub.contains("objects: ClassVar[_PostObjects]"));
        options.query_sets.insert("Post".into(), "./queries.js#PostQueries".into());
        let ts = typescript::generate_with(&ir, &schema, "schema.prisma", "orm", &options).unwrap();
        assert!(ts.contains("import * as _q0 from \"./queries.js\";") && ts.contains("useQuerySet(Post, () => _q0.PostQueries);"));
        assert!(ts.contains("readonly queries: _q0.PostQueries;") && ts.contains("RelatedSet<PostSpec, \"authorId\" | \"author\"> & QueriesOf<PostSpec>"));
        assert!(python::generate_with(&ir, &schema, "s", &options).err().unwrap().contains("module:Class"));
        options.query_sets = [("Nope".to_string(), "a:B".to_string())].into();
        assert!(python::generate_with(&ir, &schema, "s", &options).err().unwrap().contains("no model Nope"));
    }

    #[test]
    fn client_defaults_make_insert_fields_optional() {
        let ir = crate::dsl::compile("model A {\n id String @id @client_default(uuid())\n name String\n}", None).unwrap();
        let schema = crate::schema::Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
        let py = python::generate(&ir, &schema, "schema.prisma").unwrap().stub;
        assert!(py.contains("    id: NotRequired[str]\n    name: str\n"), "{py}");
        let ts = typescript::generate(&ir, &schema, "schema.prisma", "orm").unwrap();
        assert!(ts.contains("  id?: In<string>;\n  name: In<string>;\n"), "{ts}");
    }
}
