//! Selected durable-reference codec. Preparation resolves schema names once;
//! providers and async upload execution belong exclusively to the bindings.
use crate::ir::SchemaIr;

#[derive(Clone, Debug)]
pub struct PreparedFileField {
    pub position: usize,
    pub storage: String,
    pub nullable: bool,
}

impl PreparedFileField {
    pub fn validate(&self, value: &serde_json::Value) -> Result<(), String> {
        if value.is_null() && self.nullable {
            return Ok(());
        }
        let reference: storage_reference::Reference =
            serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if reference.storage() != self.storage {
            return Err(format!("file reference requires storage {}", self.storage));
        }
        Ok(())
    }
}

pub fn prepare(ir: &SchemaIr) -> Result<Vec<Vec<PreparedFileField>>, String> {
    crate::behavior::validate_field_adapters(ir)?;
    let mut prepared = vec![vec![]; ir.models.len()];
    for file in &ir.behavior.file_fields {
        let model = ir.models.iter().position(|m| m.name == file.model)
            .ok_or_else(|| format!("unknown file model {}", file.model))?;
        let position = ir.models[model].fields.iter().position(|f| f.name == file.field)
            .ok_or_else(|| format!("unknown file field {}", file.field))?;
        let field = &ir.models[model].fields[position];
        if field.primary_key || field.default.is_some() || field.default_now
            || field.default_sql.is_some() || field.auto_increment {
            return Err(format!("{}.{}: file fields cannot be keys or have defaults", file.model, file.field));
        }
        prepared[model].push(PreparedFileField {
            position, storage: file.storage.clone(), nullable: field.nullable,
        });
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::{FieldAdapter, FileField, FILE_REFERENCE_ADAPTER, SCHEMA_CONTRACT};

    fn schema() -> SchemaIr {
        let mut ir = crate::dsl::compile("model Report {\n id Int @id\n file Json?\n}", None).unwrap();
        ir.behavior.schema_contract = SCHEMA_CONTRACT;
        ir.behavior.field_adapters.push(FieldAdapter { model: "Report".into(), field: "file".into(), adapter: FILE_REFERENCE_ADAPTER.into() });
        ir.behavior.file_fields.push(FileField { model: "Report".into(), field: "file".into(), storage: "reports".into(), reference_contract: 1 });
        ir
    }

    #[test]
    fn definition_resolves_positions_and_validates_wire_without_providers() {
        let schema = crate::schema::Schema::from_ir(schema()).unwrap();
        let field = &schema.models[0].file_fields[0];
        assert_eq!(field.position, 1);
        for valid in [serde_json::Value::Null, serde_json::json!({"v":1,"storage":"reports","key":"x","version":"object-version"})] {
            field.validate(&valid).unwrap();
        }
        for invalid in [serde_json::json!({"v":2,"storage":"reports","key":"x"}), serde_json::json!({"v":1,"storage":"other","key":"x"}), serde_json::json!({"v":1,"storage":"reports","key":"x","credentials":"secret"})] {
            assert!(field.validate(&invalid).is_err());
        }
    }

    #[test]
    fn direct_metadata_cannot_bypass_default_rejection() {
        let mut ir = schema();
        ir.models[0].fields[1].default = Some(serde_json::json!({}));
        assert!(crate::schema::Schema::from_ir(ir).err().unwrap().contains("defaults"));
    }
}
