//! Shape-aware native results. Generated implementations dispatch once per selected
//! computation and call the Rust export directly inside its row loop.
use crate::{db::{Cell, DbError, DbResult, RowSet}, exec::{Fetched, Outcome}};
use orm_core::{behavior::NativeModel, ir::ValueType};
use sea_query::Value;

#[allow(unused_variables)]
mod generated { include!(env!("ORM_ENGINE_COMPOSITION")); }
pub use generated::{field, expression, omitted, upsert, insert_fields, record, insert_values, update_values};

/// Map a logical field position to its supplied row slot, once per operation.
pub fn input_map(width: usize, positions: &[usize]) -> crate::error::Result<Vec<Option<usize>>> {
    let mut map = vec![None; width];
    for (slot, &field) in positions.iter().enumerate() {
        if map[field].is_some() { return Err(crate::error::Error::query("duplicate supplied field")); }
        map[field] = Some(slot);
    }
    Ok(map)
}

/// Borrow supplied native values without constructing a callback argument buffer.
pub trait Values {
    fn value(&self, index: usize) -> Option<&Value>;
}
impl Values for [Value] {
    fn value(&self, index: usize) -> Option<&Value> { self.get(index) }
}
impl Values for [Option<Value>] {
    fn value(&self, index: usize) -> Option<&Value> { self.get(index).and_then(Option::as_ref) }
}

#[derive(Clone, Copy)]
pub struct Computation {
    pub kind: NativeModel,
    pub field: usize,
    pub column: usize,
    pub dependency: usize,
}

struct ComputedRows {
    inner: Box<dyn RowSet>,
    /// Indexed by result column; `Some` holds the computed values of that column.
    values: Vec<Option<Vec<Option<String>>>>,
}
impl RowSet for ComputedRows {
    fn len(&self) -> usize { self.inner.len() }
    fn cell(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Cell<'_>> {
        if let Some(Some(values)) = self.values.get(col) {
            return Ok(match &values[row] { Some(v) => Cell::Text(v), None => Cell::Null });
        }
        self.inner.cell(row, col, ty)
    }
    fn value(&self, row: usize, col: usize, ty: ValueType) -> DbResult<Value> {
        if let Some(Some(values)) = self.values.get(col) {
            return Ok(Value::String(values[row].clone()));
        }
        self.inner.value(row, col, ty)
    }
    fn get_i64(&self, row: usize, col: usize) -> DbResult<i64> { self.inner.get_i64(row, col) }
    fn get_bool(&self, row: usize, col: usize) -> DbResult<bool> { self.inner.get_bool(row, col) }
}
fn rows(rows: Box<dyn RowSet>, types: &[ValueType], computations: &[Computation]) -> DbResult<Box<dyn RowSet>> {
    if computations.is_empty() { return Ok(rows); }
    let mut values = vec![None; computations.iter().map(|c| c.column + 1).max().unwrap_or(0)];
    for c in computations {
        let ty = types.get(c.dependency).copied().ok_or_else(|| DbError::other("invalid computed output shape"))?;
        values[c.column] = Some(generated::compute(c.kind, c.field, rows.as_ref(), c.dependency, ty)?);
    }
    Ok(Box::new(ComputedRows { inner: rows, values }))
}
pub fn model_computations(kind: NativeModel, start: usize) -> Vec<Computation> {
    kind.computed().iter().map(|&field| Computation { kind, field, column: start + field, dependency: start + kind.dependency(field).expect("prepared computed dependency") }).collect()
}
fn fetched(mut f: Fetched) -> DbResult<Fetched> {
    f.rows = rows(f.rows, &f.plan.types, &f.plan.computations)?;
    f.children = f.children.into_iter().map(|f| fetched(f)).collect::<DbResult<_>>()?;
    Ok(f)
}
/// Called inside the existing native operation, before language materialization.
pub fn results(kinds: &[NativeModel], out: Outcome) -> DbResult<Outcome> {
    Ok(match out {
        Outcome::Select(mut selected) => {
            selected.rows = rows(selected.rows, &selected.plan.types, &selected.plan.computations)?;
            selected.prefetched = selected.prefetched.into_iter().map(|f| fetched(f)).collect::<DbResult<_>>()?;
            Outcome::Select(selected)
        }
        Outcome::Rows { model, rows: result, types, shape } => {
            let computations = match &shape { Some(shape) => shape_computations(kinds[model], 0, &shape.fields.iter().map(|f| f.field.position).collect::<Vec<_>>()), None => model_computations(kinds[model], 0) };
            Outcome::Rows { model, rows: rows(result, &types, &computations)?, types, shape }
        }
        out => out,
    })
}

pub fn shape_computations(kind: NativeModel, start: usize, positions: &[usize]) -> Vec<Computation> {
    positions.iter().enumerate().filter_map(|(slot, &field)| {
        let dependency = kind.dependency(field)?;
        Some(Computation { kind, field, column: start + slot, dependency: start + positions.iter().position(|&f| f == dependency).expect("selected computed dependency") })
    }).collect()
}
