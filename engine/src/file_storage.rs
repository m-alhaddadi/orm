//! Fixed native validation for selected JSON file fields; no provider dispatch.
use orm_core::schema::Model;
use sea_query::Value;
use crate::{Error, Result};

pub fn value(model: &Model, position: usize, value: &Value) -> Result<()> {
    let Some(field) = model.file_fields.iter().find(|f| f.position == position) else {
        return Ok(());
    };
    let json = match value {
        Value::Json(Some(json)) => json.as_ref(),
        Value::Json(None) if field.nullable => return Ok(()),
        _ => return Err(Error::query("file field requires a durable JSON reference")),
    };
    field.validate(json).map_err(Error::query)
}

pub fn rows(model: &Model, fields: &[String], rows: &[Vec<Option<Value>>]) -> Result<()> {
    for (slot, name) in fields.iter().enumerate() {
        let position = model.field_pos(name).map_err(Error::query)?;
        for row in rows {
            if let Some(Some(v)) = row.get(slot) { value(model, position, v)?; }
        }
    }
    Ok(())
}
