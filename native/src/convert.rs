//! Python <-> database value conversion, directed by the schema's column types.

use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDate, PyDateTime, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use pyo3::IntoPyObjectExt;
use sea_orm::sea_query::Value;
use sea_orm::{DbErr, QueryResult};

use crate::ir::ColType;

fn null_of(ty: Option<ColType>) -> Value {
    match ty {
        Some(ColType::BigInt) => Value::BigInt(None),
        Some(ColType::Int) => Value::Int(None),
        Some(ColType::Float) => Value::Double(None),
        Some(ColType::Bool) => Value::Bool(None),
        Some(ColType::String | ColType::Text) | None => Value::String(None),
        Some(ColType::DateTime) => Value::ChronoDateTimeWithTimeZone(None),
        Some(ColType::Date) => Value::ChronoDate(None),
        Some(ColType::Uuid) => Value::Uuid(None),
        Some(ColType::Json) => Value::Json(None),
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

fn json_to_py(py: Python<'_>, v: &serde_json::Value) -> PyResult<Py<PyAny>> {
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

/// Converts a Python value into a bind parameter. `ty` is the type of the column the
/// value is compared with or assigned to, when the planner knows it.
pub fn py_to_value(obj: &Bound<'_, PyAny>, ty: Option<ColType>) -> PyResult<Value> {
    if obj.is_none() {
        return Ok(null_of(ty));
    }
    Ok(match ty {
        Some(ColType::BigInt) => Value::BigInt(Some(obj.extract()?)),
        Some(ColType::Int) => Value::Int(Some(obj.extract()?)),
        Some(ColType::Float) => Value::Double(Some(obj.extract()?)),
        Some(ColType::Bool) => Value::Bool(Some(obj.extract()?)),
        Some(ColType::String | ColType::Text) => Value::String(Some(obj.extract()?)),
        Some(ColType::DateTime) => Value::ChronoDateTimeWithTimeZone(Some(extract_datetime(obj)?)),
        Some(ColType::Date) => Value::ChronoDate(Some(obj.extract()?)),
        Some(ColType::Uuid) => Value::Uuid(Some(extract_uuid(obj)?)),
        Some(ColType::Json) => Value::Json(Some(Box::new(py_to_json(obj)?))),
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

/// Reads column `idx` of `row` as a Python object.
pub fn cell_to_py(py: Python<'_>, row: &QueryResult, idx: usize, ty: ColType) -> Result<Py<PyAny>, PyErr> {
    fn get<T: sea_orm::TryGetable>(row: &QueryResult, idx: usize) -> PyResult<Option<T>> {
        row.try_get_by_index::<Option<T>>(idx).map_err(crate::errors::db_err)
    }
    let obj = match ty {
        ColType::BigInt => get::<i64>(row, idx)?.into_py_any(py)?,
        ColType::Int => get::<i32>(row, idx)?.into_py_any(py)?,
        ColType::Float => get::<f64>(row, idx)?.into_py_any(py)?,
        ColType::Bool => get::<bool>(row, idx)?.into_py_any(py)?,
        ColType::String | ColType::Text => get::<String>(row, idx)?.into_py_any(py)?,
        // Postgres returns timestamptz in UTC; going through `DateTime<Utc>` reuses the
        // `datetime.timezone.utc` singleton instead of building a tzinfo per row.
        ColType::DateTime => get::<DateTime<Utc>>(row, idx)?.into_py_any(py)?,
        ColType::Date => get::<NaiveDate>(row, idx)?.into_py_any(py)?,
        ColType::Uuid => get::<uuid::Uuid>(row, idx)?.into_py_any(py)?,
        ColType::Json => match get::<serde_json::Value>(row, idx)? {
            Some(v) => json_to_py(py, &v)?,
            None => py.None(),
        },
    };
    Ok(obj)
}

/// Reads column `idx` of `row` as a bind parameter (used for prefetch keys).
pub fn cell_to_value(row: &QueryResult, idx: usize, ty: ColType) -> Result<Value, DbErr> {
    Ok(match ty {
        ColType::BigInt => Value::BigInt(row.try_get_by_index(idx)?),
        ColType::Int => Value::Int(row.try_get_by_index(idx)?),
        ColType::Float => Value::Double(row.try_get_by_index(idx)?),
        ColType::Bool => Value::Bool(row.try_get_by_index(idx)?),
        ColType::String | ColType::Text => Value::String(row.try_get_by_index(idx)?),
        ColType::DateTime => Value::ChronoDateTimeWithTimeZone(
            row.try_get_by_index::<Option<DateTime<Utc>>>(idx)?.map(|d| d.fixed_offset()),
        ),
        ColType::Date => Value::ChronoDate(row.try_get_by_index(idx)?),
        ColType::Uuid => Value::Uuid(row.try_get_by_index(idx)?),
        ColType::Json => Value::Json(row.try_get_by_index::<Option<serde_json::Value>>(idx)?.map(Box::new)),
    })
}
