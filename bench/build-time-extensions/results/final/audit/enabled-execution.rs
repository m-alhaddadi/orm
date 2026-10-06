use orm_core::behavior::NativeModel;
use sea_query::Value;
use crate::error::{Error, Result};
fn field_s0_1(value: &mut Value) -> Result<()> {
let v = match value { Value::String(Some(v)) => v, Value::String(None) if false => return Ok(()), _ => return Err(Error::query("BenchItem.label: validator requires a non-null string")) };
*v = native_rules::trim(v.as_str()).map_err(Error::query)?;
let () = native_rules::username(v.as_str()).map_err(Error::query)?;
Ok(()) }
fn record_s0<R: crate::behavior::Values + ?Sized>(map: &[Option<usize>], values: &R) -> Result<()> {
if [1].iter().any(|d| map.get(*d).copied().flatten().is_some()) {
let args = [match map.get(1).copied().flatten().and_then(|p| values.value(p)) { Some(Value::String(Some(v))) => v.as_str(), _ => return Err(Error::query("record validation requires all non-null supplied dependencies; use an explicit transaction for partial records")) }];
let () = native_rules::record(&args).map_err(Error::query)?;
}
Ok(()) }
pub fn insert_values(kind: NativeModel, map: &[Option<usize>], rows: &mut [Vec<Option<Value>>], width: usize) -> Result<()> { match kind { NativeModel::S0 => {
insert_fields(NativeModel::S0, map)?;
let slot_1 = map.get(1).copied().flatten();
for row in rows { if row.len() != width { return Err(Error::query("write row length does not match fields")); }
if let Some(slot) = slot_1 { match &mut row[slot] { Some(value) => field_s0_1(value)?, None => omitted(NativeModel::S0, 1)? } }
record_s0(map, row.as_slice())?;
}
Ok(()) },
 _ => Ok(()) } }
pub fn update_values(kind: NativeModel, map: &[Option<usize>], rows: &mut [Vec<Value>], width: usize) -> Result<()> { match kind { NativeModel::S0 => {
let slot_1 = map.get(1).copied().flatten();
for row in rows { if row.len() != width { return Err(Error::query("write row length does not match fields")); }
if let Some(slot) = slot_1.filter(|slot| *slot > 0) { field_s0_1(&mut row[slot])?; }
record_s0(map, row.as_slice())?;
}
Ok(()) },
 _ => Ok(()) } }
pub fn field(kind: NativeModel, position: usize, value: &mut Value) -> Result<()> { match (kind, position) {
(NativeModel::S0, 1) => field_s0_1(value),
_ => Ok(()),
} }
pub fn expression(kind: NativeModel, position: usize) -> Result<()> { match (kind, position) { (NativeModel::S0, 1) => Err(Error::query("native validated fields require supplied values; database defaults and expressions need a constraint or explicit transaction strategy")),
 _ => Ok(()) } }
pub fn omitted(kind: NativeModel, position: usize) -> Result<()> { expression(kind, position) }
pub fn upsert(kind: NativeModel) -> Result<()> { match kind { NativeModel::S0 => Err(Error::query("native validation does not support upsert conflict outcomes; use an explicit transaction or database constraint")),
 _ => Ok(()) } }
pub fn insert_fields(kind: NativeModel, map: &[Option<usize>]) -> Result<()> { match kind { NativeModel::S0 => {
if map.get(1).copied().flatten().is_none() { return Err(Error::query("BenchItem.label: native validation requires an explicitly supplied insert value")); }
if map.get(1).copied().flatten().is_none() { return Err(Error::query("record validation requires all insert dependencies")); }
Ok(()) },
 _ => Ok(()) } }
pub fn record<R: crate::behavior::Values + ?Sized>(kind: NativeModel, map: &[Option<usize>], values: &R) -> Result<()> { match kind { NativeModel::S0 => record_s0(map, values),
 _ => Ok(()) } }
pub fn compute(kind: NativeModel, position: usize, rows: &dyn crate::db::RowSet, column: usize, ty: orm_core::ir::ValueType) -> crate::db::DbResult<Vec<Option<String>>> { match (kind, position) {
(NativeModel::S0, 4) => {
    let mut values = Vec::with_capacity(rows.len());
    for row in 0..rows.len() {
        values.push(match rows.cell(row, column, ty)? {
            crate::db::Cell::Text(value) => Some(native_rules::display(value).map_err(crate::db::DbError::other)?),
            crate::db::Cell::Null => None,
            _ => return Err(crate::db::DbError::other("computed dependency is not a string")),
        });
    }
    Ok(values)
},
_ => Err(crate::db::DbError::other("missing compiled computation")),
} }
