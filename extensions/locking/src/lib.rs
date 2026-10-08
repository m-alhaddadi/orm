//! `@locking.version`: ORM updates increment the field, and instance writes check it.
//! No DDL. Execution requires the `optimistic-locking` capability.
use orm_contracts::{extension::Declaration, ir::{ColType, SchemaIr}, tracking::VersionField};

fn error(d: &Declaration, message: impl std::fmt::Display) -> String {
    format!("{}:{}:{}: @{}: {message}", d.location.file, d.location.line, d.location.column, d.attribute)
}

pub fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations: Vec<_> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute.starts_with("locking.")).cloned().collect();
    for d in &declarations {
        if d.attribute != "locking.version" { return Err(error(d, "unknown locking attribute")); }
        let name = d.field.clone().ok_or_else(|| error(d, "is a field attribute"))?;
        let model = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| error(d, "unknown model"))?;
        let field = model.fields.iter().find(|f| f.name == name).ok_or_else(|| error(d, "unknown field"))?;
        if !matches!(field.ty, ColType::Int | ColType::BigInt) || field.array || field.nullable || field.primary_key {
            return Err(error(d, format!("{}.{name} must be a required Int or BigInt field", d.model)));
        }
        if ir.behavior.versions.iter().any(|v| v.model == d.model) { return Err(error(d, format!("{} has more than one version field", d.model))); }
        ir.behavior.versions.push(VersionField { model: d.model.clone(), field: name });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(ty: &str) -> SchemaIr {
        serde_json::from_value(serde_json::json!({
            "models": [{"name": "Post", "table": "posts", "fields": [
                {"name": "id", "column": "id", "type": "int", "primary_key": true},
                {"name": "version", "column": "version", "type": ty, "default": 0}
            ]}],
            "behavior": {"schema_contract": 1, "declarations": [
                {"attribute": "locking.version", "model": "Post", "field": "version", "positional": [], "arguments": {}, "location": {"file": "schema.prisma", "line": 3, "column": 3}}
            ]}
        })).unwrap()
    }

    #[test]
    fn preparation_records_the_version_field_once_and_adds_no_ddl() {
        let effects = orm_contracts::extension::pass_effects(&schema("int"), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.versions"]);
        let mut ir = schema("big_int");
        prepare(&mut ir).unwrap();
        assert_eq!(ir.behavior.versions, [VersionField { model: "Post".into(), field: "version".into() }]);
    }

    #[test]
    fn a_version_field_is_an_integer() {
        let err = prepare(&mut schema("string")).unwrap_err();
        assert_eq!(err, "schema.prisma:3:3: @locking.version: Post.version must be a required Int or BigInt field");
    }
}
