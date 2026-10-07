use crate::db::DbError;

pub type Result<T> = std::result::Result<T, Error>;

/// What went wrong, by the exception (or error class) a binding raises for it.
#[derive(Debug)]
pub enum Error {
    /// The query IR does not match the schema, or asks for something unsupported.
    Query(String),
    /// The schema file or IR is invalid.
    Schema(String),
    /// The migrations directory and the database's migration history disagree.
    Migration(String),
    /// Reported by the database or the driver.
    Db(DbError),
    /// A parameter value of the wrong type for its column (a `TypeError`).
    Value(String),
    /// A write to a `@@protected_write` model (named here) outside a scope that allows it.
    WriteProtected(String),
    /// An error a binding raised itself while converting its values (e.g. a Python
    /// exception), handed back to it unchanged.
    Binding(Box<dyn std::error::Error + Send + Sync>),
}

impl Error {
    pub fn query(msg: impl Into<String>) -> Self {
        Error::Query(msg.into())
    }

    pub fn value(msg: impl Into<String>) -> Self {
        Error::Value(msg.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Query(m) | Error::Schema(m) | Error::Migration(m) | Error::Value(m) => f.write_str(m),
            Error::WriteProtected(m) => write!(f, "{m} is write-protected (@@protected_write); write it inside a scope that allows writes to {m}"),
            Error::Db(e) => write!(f, "{e}"),
            Error::Binding(e) => write!(f, "{e}"),
        }
    }
}

impl From<DbError> for Error {
    fn from(e: DbError) -> Self {
        Error::Db(e)
    }
}

/// `QueryError` from a message, for `map_err` on the schema's `Result<_, String>`s.
pub(crate) fn query_err(msg: String) -> Error {
    Error::Query(msg)
}
