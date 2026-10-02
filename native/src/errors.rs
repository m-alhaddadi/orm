use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyValueError};
use pyo3::PyErr;
use crate::db::{DbError, ErrorKind};

create_exception!(_native, DatabaseError, PyException, "Error reported by the database or driver.");
create_exception!(
    _native,
    IntegrityError,
    DatabaseError,
    "Unique, foreign key, check, exclusion or not-null violation."
);
create_exception!(
    _native,
    LockNotAvailable,
    DatabaseError,
    "A row lock taken with `nowait` (or a lock timeout) found the rows locked."
);
create_exception!(_native, QueryError, PyValueError, "The query IR does not match the schema.");
create_exception!(_native, SchemaError, PyValueError, "The schema file or IR is invalid.");

pub fn db_err(e: DbError) -> PyErr {
    match e.kind {
        ErrorKind::Integrity => IntegrityError::new_err(e.message),
        ErrorKind::LockNotAvailable => LockNotAvailable::new_err(e.message),
        ErrorKind::Other => DatabaseError::new_err(e.message),
    }
}

pub fn query_err(msg: String) -> PyErr {
    QueryError::new_err(msg)
}

pub fn schema_err(msg: String) -> PyErr {
    SchemaError::new_err(msg)
}
