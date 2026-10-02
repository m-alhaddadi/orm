use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyValueError};
use pyo3::PyErr;
use sea_orm::{DbErr, RuntimeErr};

create_exception!(_native, DatabaseError, PyException, "Error reported by the database or driver.");
create_exception!(
    _native,
    IntegrityError,
    DatabaseError,
    "Unique, foreign key, check, exclusion or not-null violation."
);
create_exception!(_native, QueryError, PyValueError, "The query IR does not match the schema.");

/// SQLSTATE of a database-reported error.
fn sqlstate(e: &DbErr) -> Option<String> {
    match e {
        DbErr::Exec(RuntimeErr::SqlxError(e)) | DbErr::Query(RuntimeErr::SqlxError(e)) => match e.as_ref() {
            sea_orm::sqlx::Error::Database(d) => d.code().map(|c| c.into_owned()),
            _ => None,
        },
        _ => None,
    }
}

pub fn db_err(e: DbErr) -> PyErr {
    // Class 23: integrity constraint violation.
    match sqlstate(&e) {
        Some(code) if code.starts_with("23") => IntegrityError::new_err(e.to_string()),
        _ => DatabaseError::new_err(e.to_string()),
    }
}

pub fn query_err(msg: String) -> PyErr {
    QueryError::new_err(msg)
}
