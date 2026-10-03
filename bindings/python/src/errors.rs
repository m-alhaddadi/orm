use orm_engine::db::{DbError, ErrorKind};
use orm_engine::Error;
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyTypeError, PyValueError};
use pyo3::PyErr;

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

/// The Python exception for an engine error; Python errors raised while converting
/// parameters come back as they were.
pub fn engine_err(e: Error) -> PyErr {
    match e {
        Error::Query(m) => QueryError::new_err(m),
        Error::Schema(m) => SchemaError::new_err(m),
        Error::Db(e) => db_err(e),
        Error::Value(m) => PyTypeError::new_err(m),
        Error::Binding(e) => match e.downcast::<PyErr>() {
            Ok(e) => *e,
            Err(e) => PyTypeError::new_err(e.to_string()),
        },
    }
}

/// A Python error raised while converting a value, carried through the engine.
pub fn binding(e: PyErr) -> Error {
    Error::Binding(Box::new(e))
}
