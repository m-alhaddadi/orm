use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyValueError};
use pyo3::PyErr;
use sea_orm::{DbErr, SqlErr};

create_exception!(_native, DatabaseError, PyException, "Error reported by the database or driver.");
create_exception!(_native, IntegrityError, DatabaseError, "Unique, foreign key or not-null violation.");
create_exception!(_native, QueryError, PyValueError, "The query IR does not match the schema.");

pub fn db_err(e: DbErr) -> PyErr {
    match e.sql_err() {
        Some(SqlErr::UniqueConstraintViolation(msg) | SqlErr::ForeignKeyConstraintViolation(msg)) => {
            IntegrityError::new_err(msg)
        }
        _ => DatabaseError::new_err(e.to_string()),
    }
}

pub fn query_err(msg: String) -> PyErr {
    QueryError::new_err(msg)
}
