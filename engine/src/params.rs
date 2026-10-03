//! Parameter values: what a binding hands the planner, and SQL `NULL`s by type.
//!
//! Literals travel next to the query IR as a positional list in the binding's own
//! values (Python objects, JS values). The planner asks for each one when it knows what
//! the value is compared with or assigned to, so the binding converts it by that column
//! type: the same `3` becomes an `int4` for an `Int` column and an `int8` for a `BigInt`.

use sea_query::{ArrayType, Value};

use crate::error::{Error, Result};
use orm_core::ir::{ColType, ValueType};

/// A binding's parameter list.
pub trait Params {
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Parameter `i` as a bind value for a column of type `ty` (`None`: the planner
    /// doesn't know; infer it from the value). `i` is in range.
    fn value(&self, i: usize, ty: Option<ValueType>) -> Result<Value>;

    /// Parameter `i` as text (a `LIKE` pattern). `i` is in range.
    fn text(&self, i: usize) -> Result<String>;

    /// Parameter `i` as a row count (`LIMIT` / `OFFSET`). `i` is in range.
    fn count(&self, i: usize) -> Result<u64>;
}

/// No parameters.
pub struct NoParams;

impl Params for NoParams {
    fn len(&self) -> usize {
        0
    }
    fn value(&self, i: usize, _: Option<ValueType>) -> Result<Value> {
        Err(Error::query(format!("parameter {i} out of range")))
    }
    fn text(&self, i: usize) -> Result<String> {
        Err(Error::query(format!("parameter {i} out of range")))
    }
    fn count(&self, i: usize) -> Result<u64> {
        Err(Error::query(format!("parameter {i} out of range")))
    }
}

fn scalar_null(ty: ColType) -> Value {
    match ty {
        ColType::BigInt => Value::BigInt(None),
        ColType::Int => Value::Int(None),
        ColType::Float => Value::Double(None),
        ColType::Bool => Value::Bool(None),
        ColType::String | ColType::Text => Value::String(None),
        ColType::DateTime => Value::ChronoDateTimeWithTimeZone(None),
        ColType::Date => Value::ChronoDate(None),
        ColType::Uuid => Value::Uuid(None),
        ColType::Json => Value::Json(None),
        ColType::Decimal => Value::BigDecimal(None),
    }
}

/// The element type of an array parameter.
pub fn array_type(ty: ColType) -> ArrayType {
    match ty {
        ColType::BigInt => ArrayType::BigInt,
        ColType::Int => ArrayType::Int,
        ColType::Float => ArrayType::Double,
        ColType::Bool => ArrayType::Bool,
        ColType::String | ColType::Text => ArrayType::String,
        ColType::DateTime => ArrayType::ChronoDateTimeWithTimeZone,
        ColType::Date => ArrayType::ChronoDate,
        ColType::Uuid => ArrayType::Uuid,
        ColType::Json => ArrayType::Json,
        ColType::Decimal => ArrayType::BigDecimal,
    }
}

/// SQL `NULL` of type `ty` (text when unknown).
pub fn null_of(ty: Option<ValueType>) -> Value {
    match ty {
        Some(t) if t.array => Value::Array(array_type(t.ty), None),
        Some(t) => scalar_null(t.ty),
        None => Value::String(None),
    }
}
