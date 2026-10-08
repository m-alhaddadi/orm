use orm_engine::db::{DbError, ErrorKind};
use orm_engine::Error;
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyTypeError, PyValueError};
use pyo3::prelude::*;
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
create_exception!(
    _native,
    MigrationError,
    PyException,
    "The migrations directory and the database's migration history disagree."
);

create_exception!(
    _native,
    WriteProtected,
    PyException,
    "An ORM write to a `@@protected_write` model outside `orm.allow_writes(...)`."
);

pub fn db_err(e: DbError) -> PyErr {
    let err = match e.kind {
        ErrorKind::Integrity => IntegrityError::new_err(e.message),
        ErrorKind::LockNotAvailable => LockNotAvailable::new_err(e.message),
        ErrorKind::Other => DatabaseError::new_err(e.message),
    };
    if e.sqlstate.is_none() && e.constraint.is_none() && e.detail.is_none() {
        return err;
    }
    Python::attach(|py| {
        let value = err.value(py);
        // A failed setattr leaves the class default (None); the error itself still raises.
        let _ = value.setattr("sqlstate", e.sqlstate);
        let _ = value.setattr("constraint", e.constraint);
        let _ = value.setattr("detail", e.detail);
    });
    err
}

/// `sqlstate`, `constraint` and `detail` default to None on every database error.
pub fn add_defaults(py: Python<'_>) -> PyResult<()> {
    let cls = py.get_type::<DatabaseError>();
    for name in ["sqlstate", "constraint", "detail"] {
        cls.setattr(name, py.None())?;
    }
    Ok(())
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
        Error::Migration(m) => MigrationError::new_err(m),
        Error::Db(e) => db_err(e),
        Error::Value(m) => PyTypeError::new_err(m),
        Error::WriteProtected(m) => WriteProtected::new_err(format!("{m} is write-protected (@@protected_write); write it inside orm.allow_writes({m})")),
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
