//! Compile namespaced file declarations into provider-independent binding metadata.
use orm_contracts::{
    extension::{validate_field_adapters, FieldAdapter, FileField, FILE_REFERENCE_ADAPTER},
    ir::{ColType, SchemaIr},
};

pub fn lower(ir: &mut SchemaIr) -> Result<(), String> {
    // Validate the entire batch before mutation, so failed definition is atomic.
    validate_field_adapters(ir)?;
    let mut contributions = Vec::new();
    for declaration in &ir.behavior.declarations {
        if declaration.attribute != "storage.file" || declaration.lowered {
            continue;
        }
        let fail = |message: &str| {
            format!(
                "{}:{}:{}: @storage.file: {message}",
                declaration.location.file, declaration.location.line, declaration.location.column
            )
        };
        let name = declaration
            .field
            .as_ref()
            .ok_or_else(|| fail("field declaration required"))?;
        let model = ir
            .models
            .iter()
            .find(|m| m.name == declaration.model)
            .ok_or_else(|| fail("unknown model"))?;
        let field = model
            .fields
            .iter()
            .find(|f| f.name == *name)
            .ok_or_else(|| fail("unknown field"))?;
        if field.ty != ColType::Json || field.array || field.enum_name.is_some() {
            return Err(fail("scalar Json field required"));
        }
        if field.default.is_some()
            || field.default_sql.is_some()
            || field.default_now
            || field.primary_key
            || field.auto_increment
        {
            return Err(fail(
                "file fields cannot have database defaults or be primary keys",
            ));
        }
        let storage = declaration
            .arguments
            .get("storage")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty() && !s.contains('\0'))
            .ok_or_else(|| fail("nonempty storage identity required"))?;
        if ir
            .behavior
            .field_adapters
            .iter()
            .any(|a| a.model == model.name && a.field == *name)
            || contributions
                .iter()
                .any(|f: &FileField| f.model == model.name && f.field == *name)
        {
            return Err(fail("duplicate file-field adapter"));
        }
        orm_contracts::extension::check_file_methods(ir, model, name).map_err(|e| fail(&e))?;
        contributions.push(FileField {
            model: model.name.clone(),
            field: name.clone(),
            storage: storage.to_owned(),
            reference_contract: 1,
        });
    }
    for field in contributions {
        ir.behavior.field_adapters.push(FieldAdapter {
            model: field.model.clone(),
            field: field.field.clone(),
            adapter: FILE_REFERENCE_ADAPTER.into(),
        });
        ir.behavior.file_fields.push(field);
    }
    validate_field_adapters(ir)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn schema(ty: &str) -> SchemaIr {
        serde_json::from_value(serde_json::json!({"models":[{"name":"Report","table":"reports","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"file","column":"file","type":ty}]}],"behavior":{"declarations":[{"attribute":"storage.file","model":"Report","field":"file","arguments":{"storage":"reports"},"positional":[],"location":{"file":"schema.prisma","line":3,"column":9}}]}})).unwrap()
    }
    #[test]
    fn json_metadata_without_provider_configuration() {
        let mut ir = schema("json");
        lower(&mut ir).unwrap();
        assert_eq!(
            ir.behavior.field_adapters[0].adapter,
            FILE_REFERENCE_ADAPTER
        );
        assert_eq!(ir.behavior.file_fields[0].storage, "reports");
        ir.behavior.declarations[0].lowered = true;
        lower(&mut ir).unwrap();
        assert_eq!(ir.behavior.file_fields.len(), 1);
    }
    #[test]
    fn invalid_shape_and_collision_fail_without_mutation() {
        let mut invalid = schema("string");
        assert!(lower(&mut invalid).is_err());
        assert!(invalid.behavior.file_fields.is_empty());
        let mut collision = schema("json");
        let mut field = collision.models[0].fields[0].clone();
        field.name = "file_open".into();
        collision.models[0].fields.push(field);
        assert!(lower(&mut collision).unwrap_err().contains("collides"));
        assert!(collision.behavior.file_fields.is_empty());
    }
    #[test]
    fn disabled_artifact_rejects_file_contract() {
        let mut ir = schema("json");
        lower(&mut ir).unwrap();
        assert!(
            orm_contracts::extension::check_requirements(&ir, &Default::default())
                .unwrap_err()
                .contains("rebuild")
        );
    }
}
