//! JS <-> database value conversion, directed by the schema's column types.
//!
//! | column      | JS in                                  | JS out                     |
//! |-------------|----------------------------------------|----------------------------|
//! | `BigInt`    | `bigint`, or a safe-integer `number`   | `bigint`                   |
//! | `Int`       | an integer `number` in int32 range     | `number`                   |
//! | `Float`     | `number`                               | `number`                   |
//! | `Decimal`   | `Decimal`, finite `number`, decimal text | `Decimal`                |
//! | `DateTime`  | a valid `Date`                         | `Date` (milliseconds)      |
//! | `Date`      | a valid `Date` (its UTC day)           | `Date` at 00:00 UTC        |
//! | `Uuid`      | `string`                               | `string`                   |
//! | `Json`      | anything `JSON.stringify` takes        | parsed JSON                |
//! | arrays      | JS arrays of the element type          | JS arrays                  |
//!
//! Anything else is a `TypeError`, not a silent coercion.

use std::str::FromStr;

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use napi::sys;
use sea_query::Value;

use crate::js::{Js, V};
use orm_core::ir::{ColType, ValueType};
use orm_engine::db::Cell;
use orm_engine::params::{array_type, null_of, Params};
use orm_engine::{Error, Result};

const MAX_SAFE: f64 = 9007199254740991.0;

fn raw(e: napi::Error) -> Error {
    Error::value(e.reason.clone())
}

/// What a JS value is, for conversion and error messages.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Undefined,
    Null,
    Bool,
    Number,
    String,
    BigInt,
    Array,
    Date,
    Decimal,
    Object,
    Other,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Undefined => "undefined",
            Kind::Null => "null",
            Kind::Bool => "a boolean",
            Kind::Number => "a number",
            Kind::String => "a string",
            Kind::BigInt => "a bigint",
            Kind::Array => "an array",
            Kind::Date => "a Date",
            Kind::Decimal => "a Decimal",
            Kind::Object => "an object",
            Kind::Other => "a function or symbol",
        }
    }
}

/// Converts JS values: the `Decimal` class (from `decimal.js`, registered once by the
/// runtime) is recognized with `instanceof`.
#[derive(Clone, Copy)]
pub struct Conv {
    pub js: Js,
    pub decimal: Option<V>,
}

impl Conv {
    fn kind(self, v: V) -> napi::Result<Kind> {
        use sys::ValueType::*;
        Ok(match self.js.type_of(v)? {
            t if t == napi_undefined => Kind::Undefined,
            t if t == napi_null => Kind::Null,
            t if t == napi_boolean => Kind::Bool,
            t if t == napi_number => Kind::Number,
            t if t == napi_string => Kind::String,
            t if t == napi_bigint => Kind::BigInt,
            t if t == napi_object => {
                if self.js.is_array(v)? {
                    Kind::Array
                } else if self.js.is_date(v)? {
                    Kind::Date
                } else if matches!(self.decimal, Some(d) if self.js.instance_of(v, d)?) {
                    Kind::Decimal
                } else {
                    Kind::Object
                }
            }
            _ => Kind::Other,
        })
    }

    fn expected(self, what: &str, v: V) -> Error {
        let got = self.kind(v).map(Kind::name).unwrap_or("an unknown value");
        Error::value(format!("expected {what}, got {got}"))
    }

    fn i64_of(self, v: V, kind: Kind) -> Result<Option<i64>> {
        Ok(match kind {
            Kind::BigInt => match self.js.bigint(v).map_err(raw)? {
                (n, true) => Some(n),
                (_, false) => return Err(Error::value("bigint out of the 64-bit range")),
            },
            Kind::Number => {
                let n = self.js.f64(v).map_err(raw)?;
                if n.fract() != 0.0 || !n.is_finite() || n.abs() > MAX_SAFE {
                    return Err(Error::value(format!("expected an integer, got {n}")));
                }
                Some(n as i64)
            }
            _ => None,
        })
    }

    fn datetime(self, v: V) -> Result<DateTime<Utc>> {
        let ms = self.js.date_ms(v).map_err(raw)?;
        if !ms.is_finite() {
            return Err(Error::value("invalid Date"));
        }
        DateTime::from_timestamp_millis(ms as i64).ok_or_else(|| Error::value("Date out of range"))
    }

    fn decimal(self, v: V, kind: Kind) -> Result<BigDecimal> {
        let text = match kind {
            Kind::Decimal | Kind::BigInt => self.js.coerce_string(v).map_err(raw)?,
            Kind::Number => {
                let n = self.js.f64(v).map_err(raw)?;
                if !n.is_finite() {
                    return Err(Error::value(format!("expected a finite decimal, got {n}")));
                }
                self.js.coerce_string(v).map_err(raw)?
            }
            Kind::String => self.js.string(v).map_err(raw)?,
            _ => return Err(self.expected("a Decimal, a number or decimal text", v)),
        };
        BigDecimal::from_str(text.trim()).map_err(|_| Error::value(format!("expected a finite decimal, got {text:?}")))
    }

    fn json(self, v: V) -> Result<serde_json::Value> {
        let text = self.js.json_stringify(v).map_err(raw)?.ok_or_else(|| self.expected("a JSON value", v))?;
        serde_json::from_str(&text).map_err(|e| Error::value(e.to_string()))
    }

    /// A JS value as a bind parameter for a column of type `ty` (`None`: inferred from
    /// the value).
    pub fn value(self, v: V, ty: Option<ValueType>) -> Result<Value> {
        let kind = self.kind(v).map_err(raw)?;
        match kind {
            Kind::Null => return Ok(null_of(ty)),
            Kind::Undefined => return Err(Error::value("undefined is not a value; use null for NULL")),
            _ => {}
        }
        if let Some(t) = ty.filter(|t| t.array) {
            if kind != Kind::Array {
                return Err(self.expected("an array", v));
            }
            let items = self.js.elements(v).map_err(raw)?;
            let items = items.into_iter().map(|i| self.value(i, Some(t.element()))).collect::<Result<Vec<_>>>()?;
            return Ok(Value::Array(array_type(t.ty), Some(Box::new(items))));
        }
        let Some(ty) = ty else { return self.infer(v, kind) };
        Ok(match ty.ty {
            ColType::BigInt => match self.i64_of(v, kind)? {
                Some(n) => Value::BigInt(Some(n)),
                None => return Err(self.expected("a bigint or an integer number", v)),
            },
            ColType::Int => match self.i64_of(v, kind)? {
                Some(n) => Value::Int(Some(
                    i32::try_from(n).map_err(|_| Error::value(format!("{n} is out of the int32 range")))?,
                )),
                None => return Err(self.expected("an integer number", v)),
            },
            ColType::Float => match kind {
                Kind::Number => Value::Double(Some(self.js.f64(v).map_err(raw)?)),
                _ => return Err(self.expected("a number", v)),
            },
            ColType::Bool => match kind {
                Kind::Bool => Value::Bool(Some(self.js.bool(v).map_err(raw)?)),
                _ => return Err(self.expected("a boolean", v)),
            },
            ColType::String | ColType::Text => match kind {
                Kind::String => Value::String(Some(self.js.string(v).map_err(raw)?)),
                _ => return Err(self.expected("a string", v)),
            },
            ColType::DateTime => match kind {
                Kind::Date => Value::ChronoDateTimeWithTimeZone(Some(self.datetime(v)?.fixed_offset())),
                _ => return Err(self.expected("a Date", v)),
            },
            ColType::Date => match kind {
                Kind::Date => Value::ChronoDate(Some(self.datetime(v)?.date_naive())),
                _ => return Err(self.expected("a Date", v)),
            },
            ColType::Uuid => match kind {
                Kind::String => {
                    let s = self.js.string(v).map_err(raw)?;
                    Value::Uuid(Some(
                        uuid::Uuid::parse_str(&s).map_err(|e| Error::value(format!("invalid UUID {s:?}: {e}")))?,
                    ))
                }
                _ => return Err(self.expected("a UUID string", v)),
            },
            ColType::Json => Value::Json(Some(Box::new(self.json(v)?))),
            ColType::Decimal => Value::BigDecimal(Some(Box::new(self.decimal(v, kind)?))),
        })
    }

    /// A parameter whose column type the planner doesn't know.
    fn infer(self, v: V, kind: Kind) -> Result<Value> {
        Ok(match kind {
            Kind::Bool => Value::Bool(Some(self.js.bool(v).map_err(raw)?)),
            Kind::Number => {
                let n = self.js.f64(v).map_err(raw)?;
                if n.fract() == 0.0 && n.abs() <= MAX_SAFE {
                    Value::BigInt(Some(n as i64))
                } else {
                    Value::Double(Some(n))
                }
            }
            Kind::BigInt => Value::BigInt(self.i64_of(v, kind)?),
            Kind::String => Value::String(Some(self.js.string(v).map_err(raw)?)),
            Kind::Date => Value::ChronoDateTimeWithTimeZone(Some(self.datetime(v)?.fixed_offset())),
            Kind::Decimal => Value::BigDecimal(Some(Box::new(self.decimal(v, kind)?))),
            Kind::Array | Kind::Object => Value::Json(Some(Box::new(self.json(v)?))),
            Kind::Null | Kind::Undefined | Kind::Other => {
                return Err(Error::value(format!("unsupported parameter: {}", kind.name())))
            }
        })
    }

    // -- out ------------------------------------------------------------------------------

    /// A decoded cell as a JS value (enum values as stored).
    pub fn cell(self, cell: Cell<'_>) -> napi::Result<V> {
        let js = self.js;
        match cell {
            Cell::Null => js.null(),
            Cell::Bool(b) => js.boolean(b),
            Cell::Int(n) => js.int(n),
            Cell::BigInt(n) => js.bigint_from(n),
            Cell::Float(n) => js.number(n),
            Cell::Text(s) => js.str(s),
            Cell::DateTime(d) => js.date(d.timestamp_millis() as f64),
            Cell::Date(d) => js.date(midnight_ms(d)),
            Cell::Uuid(u) => js.str(u.hyphenated().encode_lower(&mut uuid::Uuid::encode_buffer())),
            Cell::Json(j) => self.json_out(&j),
            Cell::Decimal(s) => {
                let text = js.str(&s)?;
                match self.decimal {
                    Some(ctor) => js.construct(ctor, text),
                    None => Ok(text),
                }
            }
            Cell::Array(items) => {
                let arr = js.array(items.len())?;
                for (i, item) in items.into_iter().enumerate() {
                    js.set_element(arr, i as u32, self.cell(item)?)?;
                }
                Ok(arr)
            }
        }
    }

    pub fn json_out(self, v: &serde_json::Value) -> napi::Result<V> {
        use serde_json::Value as J;
        let js = self.js;
        match v {
            J::Null => js.null(),
            J::Bool(b) => js.boolean(*b),
            J::Number(n) => js.number(n.as_f64().unwrap_or(f64::NAN)),
            J::String(s) => js.str(s),
            J::Array(items) => js.array_of(items.iter().map(|i| self.json_out(i))),
            J::Object(map) => {
                let obj = js.object()?;
                for (k, item) in map {
                    js.set(obj, k, self.json_out(item)?)?;
                }
                Ok(obj)
            }
        }
    }
}

fn midnight_ms(d: NaiveDate) -> f64 {
    d.and_hms_opt(0, 0, 0).map(|t| t.and_utc().timestamp_millis() as f64).unwrap_or(f64::NAN)
}

/// A query's parameters: JS values converted when the planner asks for them.
pub struct JsParams {
    pub conv: Conv,
    pub values: Vec<V>,
}

impl Params for JsParams {
    fn len(&self) -> usize {
        self.values.len()
    }

    fn value(&self, i: usize, ty: Option<ValueType>) -> Result<Value> {
        self.conv.value(self.values[i], ty)
    }

    fn text(&self, i: usize) -> Result<String> {
        let v = self.values[i];
        match self.conv.kind(v).map_err(raw)? {
            Kind::String => self.conv.js.string(v).map_err(raw),
            _ => Err(self.conv.expected("a string", v)),
        }
    }

    fn count(&self, i: usize) -> Result<u64> {
        let v = self.values[i];
        let kind = self.conv.kind(v).map_err(raw)?;
        match self.conv.i64_of(v, kind) {
            Ok(Some(n)) if n >= 0 => Ok(n as u64),
            _ => Err(Error::query("LIMIT and OFFSET take a non-negative integer")),
        }
    }
}
