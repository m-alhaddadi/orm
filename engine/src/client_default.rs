//! Fill `@client_default`s into omitted insert values.
use crate::{error::{Error, Result}, params::{array_type, null_of}};
use orm_core::{client_default::{ClientDefault, Literal}, ir::{ClientCall, ColType}, schema::Model};
use sea_query::Value;

type Rows = Vec<Vec<Option<Value>>>;

fn literal(value: &Literal) -> Value {
    match value {
        Literal::Null(ty) => null_of(Some(*ty)),
        Literal::Int(v) => Value::Int(Some(*v)),
        Literal::BigInt(v) => Value::BigInt(Some(*v)),
        Literal::Float(v) => Value::Double(Some(*v)),
        Literal::Bool(v) => Value::Bool(Some(*v)),
        Literal::Text(v) => Value::String(Some(v.clone())),
        Literal::DateTime(v) => Value::ChronoDateTimeWithTimeZone(Some(*v)),
        Literal::Date(v) => Value::ChronoDate(Some(*v)),
        Literal::Uuid(v) => Value::Uuid(Some(*v)),
        Literal::Decimal(v) => Value::BigDecimal(Some(Box::new(v.clone()))),
        Literal::Json(v) => Value::Json(Some(Box::new(v.clone()))),
        Literal::Array(ty, values) => Value::Array(array_type(*ty), Some(Box::new(values.iter().map(literal).collect()))),
    }
}

fn call(call: ClientCall, ty: ColType) -> Value {
    let uuid = |id: uuid::Uuid| match ty {
        ColType::Uuid => Value::Uuid(Some(id)),
        _ => Value::String(Some(id.to_string())),
    };
    match call {
        ClientCall::Uuid => uuid(uuid::Uuid::new_v4()),
        ClientCall::Uuid7 => uuid(uuid::Uuid::now_v7()),
        ClientCall::Now if ty == ColType::Date => Value::ChronoDate(Some(chrono::Utc::now().date_naive())),
        ClientCall::Now => Value::ChronoDateTimeWithTimeZone(Some(chrono::Utc::now().fixed_offset())),
    }
}

/// The current time as a value of a `DateTime` or `Date` field.
pub fn now(ty: ColType) -> Value { call(ClientCall::Now, ty) }

/// Explicit SQL NULL is Some(typed null); None is an omitted value. Client defaults
/// fill omission before native transforms/validation or SQL planning.
pub fn fill<'a>(model: &Model, fields: &'a [String], mut rows: Rows) -> Result<(std::borrow::Cow<'a, [String]>, Rows)> {
    if model.client_defaults.is_empty() { return Ok((std::borrow::Cow::Borrowed(fields), rows)); }
    let mut fields = fields.to_vec();
    if rows.iter().any(|r| r.len() != fields.len()) { return Err(Error::query("insert row length does not match fields")); }
    for (position, default) in &model.client_defaults {
        let field = &model.fields()[*position];
        let slot = match fields.iter().position(|f| *f == field.name) {
            Some(slot) => slot,
            None => { let slot = fields.len(); fields.push(field.name.clone()); for row in &mut rows { row.push(None); } slot }
        };
        for row in &mut rows {
            if row[slot].is_none() {
                row[slot] = Some(match default { ClientDefault::Literal(v) => literal(v), ClientDefault::Call(c) => call(*c, field.ty) });
            }
        }
    }
    Ok((std::borrow::Cow::Owned(fields), rows))
}
