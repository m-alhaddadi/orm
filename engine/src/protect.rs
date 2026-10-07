//! `@@protected_write`: ORM writes to a protected table run only inside a scope that
//! allows them. An application-level check; raw SQL and other clients still write.
//!
//! Protection and permission go by table: a proxy writes the table of its root, and a
//! composed write also writes the tables of its storage ancestors.

use crate::error::{Error, Result};
use orm_core::schema::Schema;

/// Fails with [`Error::WriteProtected`] when a write of model `model` writes a protected
/// table that no model in `allowed` (model names) writes.
pub fn ensure_writable(schema: &Schema, model: &str, allowed: &[String]) -> Result<()> {
    let idx = schema.model_idx(model).map_err(Error::Query)?;
    for table in written_tables(schema, idx) {
        let Some(owner) = protector(schema, table) else { continue };
        let permitted = allowed.iter().filter_map(|name| schema.model_idx(name).ok()).any(|m| schema.model(m).table() == table);
        if !permitted {
            return Err(Error::WriteProtected(owner.to_owned()));
        }
    }
    Ok(())
}

/// The model whose `@@protected_write` protects `table`, if one does.
fn protector<'s>(schema: &'s Schema, table: &str) -> Option<&'s str> {
    schema.models.iter().chain(&schema.physical().models)
        .find(|m| m.ir.protected_write && m.table() == table)
        .map(|m| m.ir.name.as_str())
}

/// The tables a write of model `idx` writes: its own, and with model composition the
/// tables of its storage ancestors.
fn written_tables(schema: &Schema, idx: usize) -> Vec<&str> {
    let model = schema.model(idx);
    #[allow(unused_mut)]
    let mut tables = vec![model.table()];
    #[cfg(feature = "model-composition")]
    {
        let owners = &schema.physical().models;
        let mut current = model.owner;
        while let Some(link) = schema.owner_links.iter().find(|l| l.child == current) {
            tables.push(owners[link.parent.0].table());
            current = link.parent;
        }
    }
    tables
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Schema {
        let ir = serde_json::json!({"models": [
            {"name": "Post", "table": "posts", "protected_write": true, "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}]},
            {"name": "Tag", "table": "tags", "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}]},
            {"name": "PostAlias", "table": "posts", "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}]}
        ]});
        Schema::from_ir(serde_json::from_value(ir).unwrap()).unwrap()
    }

    #[test]
    fn a_protected_table_needs_a_model_that_writes_it_in_the_scope() {
        let schema = schema();
        assert!(matches!(ensure_writable(&schema, "Post", &[]), Err(Error::WriteProtected(m)) if m == "Post"));
        assert!(matches!(ensure_writable(&schema, "Post", &["Tag".into()]), Err(Error::WriteProtected(_))));
        assert!(ensure_writable(&schema, "Post", &["Post".into()]).is_ok());
        assert!(ensure_writable(&schema, "Tag", &[]).is_ok());
        // A second model of the same table (as a proxy is) has the same protection.
        assert!(matches!(ensure_writable(&schema, "PostAlias", &[]), Err(Error::WriteProtected(m)) if m == "Post"));
        assert!(ensure_writable(&schema, "PostAlias", &["Post".into()]).is_ok());
        assert!(ensure_writable(&schema, "Post", &["PostAlias".into(), "Unknown".into()]).is_ok());
    }
}
