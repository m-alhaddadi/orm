//! Parameter values: what a binding hands the planner, and SQL `NULL`s by type.
//!
//! Literals travel next to the query IR as a positional list in the binding's own
//! values (Python objects, JS values). The planner asks for each one when it knows what
//! the value is compared with or assigned to, so the binding converts it by that column
//! type: the same `3` becomes an `int4` for an `Int` column and an `int8` for a `BigInt`.

use std::collections::BTreeMap;

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

    /// The parameter that holds the `scope.<name>` value, if the statement has one.
    fn scope(&self, _name: &str) -> Option<usize> {
        None
    }
}

/// `params` with the statement's `scope.<name>` parameter indexes.
pub struct Scoped<'a> {
    pub params: &'a dyn Params,
    pub scope: &'a BTreeMap<String, usize>,
}

impl Params for Scoped<'_> {
    fn len(&self) -> usize {
        self.params.len()
    }
    fn value(&self, i: usize, ty: Option<ValueType>) -> Result<Value> {
        self.params.value(i, ty)
    }
    fn text(&self, i: usize) -> Result<String> {
        self.params.text(i)
    }
    fn count(&self, i: usize) -> Result<u64> {
        self.params.count(i)
    }
    fn scope(&self, name: &str) -> Option<usize> {
        self.scope.get(name).copied()
    }
}

/// `update_many` filters: a list of expressions, or `{"filters": [...], "scope": {...}}`.
pub fn scoped_filters(json: &str) -> Result<(Vec<orm_core::ir::Expr>, BTreeMap<String, usize>)> {
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Filters {
        List(Vec<orm_core::ir::Expr>),
        Scoped { filters: Vec<orm_core::ir::Expr>, scope: BTreeMap<String, usize> },
    }
    match serde_json::from_str(json).map_err(|e| Error::query(format!("invalid filter IR: {e}")))? {
        Filters::List(filters) => Ok((filters, BTreeMap::new())),
        Filters::Scoped { filters, scope } => Ok((filters, scope)),
    }
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
