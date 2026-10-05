// Handwritten built-in control for BenchItem. Same functions, host primitives,
// field positions, validation policy and result SQL as the generated extension.
use orm_core::behavior::NativeModel;
use sea_query::Value;
use crate::error::{Error, Result};

fn label(value: &mut Value) -> Result<()> {
    let Value::String(Some(value)) = value else { return Err(Error::query("BenchItem.label: validator requires a non-null string")); };
    *value = native_rules::trim(value.as_str()).map_err(Error::query)?;
    native_rules::username(value.as_str()).map_err(Error::query)
}
pub fn field(kind: NativeModel, position: usize, value: &mut Value) -> Result<()> {
    if kind == NativeModel::S0 && position == 1 { label(value) } else { Ok(()) }
}
pub fn expression(kind: NativeModel, position: usize) -> Result<()> { omitted(kind,position) }
pub fn omitted(kind: NativeModel, position: usize) -> Result<()> {
    if kind == NativeModel::S0 && position == 1 {
        Err(Error::query("native validated fields require supplied values; database defaults and expressions need a constraint or explicit transaction strategy"))
    } else { Ok(()) }
}
pub fn upsert(kind: NativeModel) -> Result<()> {
    if kind == NativeModel::S0 { Err(Error::query("native validation does not support upsert conflict outcomes; use an explicit transaction or database constraint")) } else { Ok(()) }
}
pub fn insert_fields(kind: NativeModel, map: &[Option<usize>]) -> Result<()> {
    if kind == NativeModel::S0 && map.get(1).copied().flatten().is_none() {
        Err(Error::query("BenchItem.label: native validation requires an explicitly supplied insert value"))
    } else { Ok(()) }
}
fn validate_record<R: crate::behavior::Values + ?Sized>(map: &[Option<usize>], values: &R) -> Result<()> {
    let Some(slot) = map.get(1).copied().flatten() else { return Ok(()); };
    let Some(Value::String(Some(value))) = values.value(slot) else {
        return Err(Error::query("record validation requires all non-null supplied dependencies; use an explicit transaction for partial records"));
    };
    native_rules::record(&[value.as_str()]).map_err(Error::query)
}
pub fn record<R: crate::behavior::Values + ?Sized>(kind: NativeModel, map: &[Option<usize>], values: &R) -> Result<()> {
    if kind == NativeModel::S0 { validate_record(map,values) } else { Ok(()) }
}
pub fn insert_values(kind: NativeModel,map: &[Option<usize>],rows:&mut [Vec<Option<Value>>],width:usize) -> Result<()> {
    if kind != NativeModel::S0 { return Ok(()); }
    insert_fields(kind,map)?;
    let slot = map.get(1).copied().flatten();
    for row in rows {
        if row.len() != width { return Err(Error::query("write row length does not match fields")); }
        if let Some(slot) = slot {
            match &mut row[slot] { Some(value)=>label(value)?,None=>omitted(kind,1)? }
        }
        validate_record(map,row.as_slice())?;
    }
    Ok(())
}
pub fn update_values(kind: NativeModel,map:&[Option<usize>],rows:&mut [Vec<Value>],width:usize) -> Result<()> {
    if kind != NativeModel::S0 { return Ok(()); }
    let slot = map.get(1).copied().flatten();
    for row in rows {
        if row.len() != width { return Err(Error::query("write row length does not match fields")); }
        if let Some(slot) = slot.filter(|slot|*slot>0) { label(&mut row[slot])?; }
        validate_record(map,row.as_slice())?;
    }
    Ok(())
}
pub fn compute(kind: NativeModel, position: usize, rows: &dyn crate::db::RowSet, column: usize, ty: orm_core::ir::ValueType) -> crate::db::DbResult<Vec<Option<String>>> {
    if kind != NativeModel::S0 || position != 4 { return Err(crate::db::DbError::other("unknown native computed field")); }
    let mut values = Vec::with_capacity(rows.len());
    for row in 0..rows.len() {
        values.push(match rows.cell(row,column,ty)? {
            crate::db::Cell::Text(value) => Some(native_rules::display(value).map_err(crate::db::DbError::other)?),
            crate::db::Cell::Null => None,
            _ => return Err(crate::db::DbError::other("computed dependency is not a string")),
        });
    }
    Ok(values)
}
