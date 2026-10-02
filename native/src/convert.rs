//! Python <-> database value conversion, directed by the schema's column types.

use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDate, PyDateTime, PyFloat, PyInt, PyString};
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
    }
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
    })
}
