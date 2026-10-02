//! Python <-> database value conversion, directed by the schema's column types.

use chrono::{DateTime, FixedOffset};
use std::str::FromStr;

use bigdecimal::BigDecimal;
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDate, PyDateTime, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use pyo3::IntoPyObjectExt;
use sea_query::{ArrayType, Value};

use orm_core::ir::{ColType, ValueType};

static DECIMAL: PyOnceLock<Py<PyAny>> = PyOnceLock::new();

/// Python's `decimal.Decimal`.
pub fn decimal_class(py: Python<'_>) -> PyResult<&Bound<'_, PyAny>> {
    Ok(DECIMAL.get_or_try_init(py, || py.import("decimal")?.getattr("Decimal").map(Bound::unbind))?.bind(py))
}

/// A `decimal.Decimal` from decimal text.
pub fn decimal_to_py(py: Python<'_>, text: &str) -> PyResult<Py<PyAny>> {
    Ok(decimal_class(py)?.call1((text,))?.unbind())
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

pub fn null_of(ty: Option<ValueType>) -> Value {
    match ty {
        Some(t) if t.array => Value::Array(array_type(t.ty), None),
        Some(t) => scalar_null(t.ty),
        None => Value::String(None),
    }
}

fn py_to_json(obj: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    use serde_json::Value as J;
    Ok(if obj.is_none() {
        J::Null
    } else if obj.is_instance_of::<PyBool>() {
        J::Bool(obj.extract()?)
    } else if obj.is_instance_of::<PyInt>() {
        match obj.extract::<i64>() {
            Ok(i) => J::from(i),
            Err(_) => J::from(obj.extract::<u64>()?),
        }
    } else if obj.is_instance_of::<PyFloat>() {
        serde_json::Number::from_f64(obj.extract()?)
            .map(J::Number)
            .ok_or_else(|| PyTypeError::new_err("NaN and infinity are not valid JSON"))?
    } else if obj.is_instance_of::<PyString>() {
        J::String(obj.extract()?)
    } else if let Ok(d) = obj.cast::<PyDict>() {
        let mut map = serde_json::Map::with_capacity(d.len());
        for (k, v) in d.iter() {
            let key = k
                .extract::<String>()
                .map_err(|_| PyTypeError::new_err("JSON object keys must be strings"))?;
            map.insert(key, py_to_json(&v)?);
        }
        J::Object(map)
    } else if obj.is_instance_of::<PyList>() || obj.is_instance_of::<PyTuple>() {
        J::Array(obj.try_iter()?.map(|v| py_to_json(&v?)).collect::<PyResult<_>>()?)
    } else {
        return Err(PyTypeError::new_err(format!(
            "{} is not JSON serializable",
            obj.get_type().name()?
        )));
    })
}

pub fn json_to_py(py: Python<'_>, v: &serde_json::Value) -> PyResult<Py<PyAny>> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => py.None(),
        J::Bool(b) => b.into_py_any(py)?,
        J::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.into_py_any(py)?,
            (None, Some(u)) => u.into_py_any(py)?,
            _ => n.as_f64().unwrap_or(f64::NAN).into_py_any(py)?,
        },
        J::String(s) => s.into_py_any(py)?,
        J::Array(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(json_to_py(py, item)?)?;
            }
            list.into_py_any(py)?
        }
        J::Object(map) => {
            let d = PyDict::new(py);
            for (k, item) in map {
                d.set_item(k, json_to_py(py, item)?)?;
            }
            d.into_py_any(py)?
        }
    })
}

fn extract_uuid(obj: &Bound<'_, PyAny>) -> PyResult<uuid::Uuid> {
    if obj.is_instance_of::<PyString>() {
        let s: String = obj.extract()?;
        return uuid::Uuid::parse_str(&s).map_err(|e| PyTypeError::new_err(format!("invalid UUID {s:?}: {e}")));
    }
    obj.extract()
}

fn extract_datetime(obj: &Bound<'_, PyAny>) -> PyResult<DateTime<FixedOffset>> {
    obj.extract::<DateTime<FixedOffset>>().map_err(|_| {
        PyTypeError::new_err(format!(
            "expected a timezone-aware datetime, got {}",
            obj.repr().map(|r| r.to_string()).unwrap_or_default()
        ))
    })
}

/// A decimal parameter from a `Decimal`, an int, a float or decimal text.
fn extract_decimal(obj: &Bound<'_, PyAny>) -> PyResult<BigDecimal> {
    if obj.is_instance_of::<PyBool>() {
        return Err(PyTypeError::new_err("expected a decimal, got a bool"));
    }
    let text = if obj.is_instance_of::<PyString>() { obj.extract::<String>()? } else { obj.str()?.extract::<String>()? };
    BigDecimal::from_str(text.trim())
        .map_err(|_| PyTypeError::new_err(format!("expected a finite decimal, got {}", obj.repr().map(|r| r.to_string()).unwrap_or_default())))
}

/// Converts a Python value into a bind parameter. `ty` is the type of the column the
/// value is compared with or assigned to, when the planner knows it.
pub fn py_to_value(obj: &Bound<'_, PyAny>, ty: Option<ValueType>) -> PyResult<Value> {
    if obj.is_none() {
        return Ok(null_of(ty));
    }
    if let Some(t) = ty.filter(|t| t.array) {
        if obj.is_instance_of::<PyString>() || !(obj.is_instance_of::<PyList>() || obj.is_instance_of::<PyTuple>()) {
            return Err(PyTypeError::new_err(format!(
                "expected a list, got {}",
                obj.get_type().name()?
            )));
        }
        let items = obj.try_iter()?.map(|v| py_to_value(&v?, Some(t.element()))).collect::<PyResult<Vec<_>>>()?;
        return Ok(Value::Array(array_type(t.ty), Some(Box::new(items))));
    }
    Ok(match ty.map(|t| t.ty) {
        Some(ColType::BigInt) => Value::BigInt(Some(obj.extract()?)),
        Some(ColType::Int) => Value::Int(Some(obj.extract()?)),
        Some(ColType::Float) => Value::Double(Some(obj.extract()?)),
        Some(ColType::Bool) => Value::Bool(Some(obj.extract()?)),
        Some(ColType::String | ColType::Text) => Value::String(Some(obj.extract()?)),
        Some(ColType::DateTime) => Value::ChronoDateTimeWithTimeZone(Some(extract_datetime(obj)?)),
        Some(ColType::Date) => Value::ChronoDate(Some(obj.extract()?)),
        Some(ColType::Uuid) => Value::Uuid(Some(extract_uuid(obj)?)),
        Some(ColType::Json) => Value::Json(Some(Box::new(py_to_json(obj)?))),
        Some(ColType::Decimal) => Value::BigDecimal(Some(Box::new(extract_decimal(obj)?))),
        None => {
            if obj.is_instance_of::<PyBool>() {
                Value::Bool(Some(obj.extract()?))
            } else if obj.is_instance_of::<PyInt>() {
                Value::BigInt(Some(obj.extract()?))
            } else if obj.is_instance_of::<PyFloat>() {
                Value::Double(Some(obj.extract()?))
            } else if obj.is_instance_of::<PyString>() {
                Value::String(Some(obj.extract()?))
            } else if obj.is_instance_of::<PyDateTime>() {
                Value::ChronoDateTimeWithTimeZone(Some(extract_datetime(obj)?))
            } else if obj.is_instance_of::<PyDate>() {
                Value::ChronoDate(Some(obj.extract()?))
            } else if obj.is_instance(decimal_class(obj.py())?)? {
                Value::BigDecimal(Some(Box::new(extract_decimal(obj)?)))
            } else if let Ok(u) = obj.extract::<uuid::Uuid>() {
                Value::Uuid(Some(u))
            } else if obj.is_instance_of::<PyDict>() || obj.is_instance_of::<PyList>() {
                Value::Json(Some(Box::new(py_to_json(obj)?)))
            } else {
                return Err(PyTypeError::new_err(format!(
                    "unsupported parameter type {}",
                    obj.get_type().name()?
                )));
            }
        }
    })
}
