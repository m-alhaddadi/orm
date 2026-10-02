//! SQL dialects and what each one can do.
//!
//! The query planner and the schema checks consult [`Capabilities`] instead of
//! assuming Postgres, so a query or schema that needs a feature the database lacks
//! fails with a clear error (or is emulated) rather than producing SQL the database
//! rejects. Only Postgres exists today; a new database adds a variant here, a sea-query
//! builder and a driver in the binding.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    #[default]
    Postgres,
}

/// Features the planner can't assume. Each flag names the SQL it guards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// `INSERT / UPDATE / DELETE ... RETURNING`.
    pub returning: bool,
    /// `INSERT ... ON CONFLICT (...) DO UPDATE / DO NOTHING`.
    pub on_conflict: bool,
    /// `ILIKE` (otherwise `LOWER(x) LIKE LOWER(p)`).
    pub ilike: bool,
    /// `SELECT ... FOR UPDATE`.
    pub lock_exclusive: bool,
    /// `SELECT ... FOR SHARE`.
    pub lock_shared: bool,
    /// `FOR UPDATE OF <table>`: lock only some of the joined tables.
    pub lock_of: bool,
    /// `NOWAIT`.
    pub lock_nowait: bool,
    /// `SKIP LOCKED`.
    pub lock_skip_locked: bool,
    /// `UPDATE ... FROM (VALUES ...)`: one statement updating rows to different values.
    pub update_from_values: bool,
    /// `SAVEPOINT` for nested transactions.
    pub savepoints: bool,
}

impl Dialect {
    pub const fn name(self) -> &'static str {
        match self {
            Dialect::Postgres => "postgres",
        }
    }

    pub const fn capabilities(self) -> Capabilities {
        match self {
            Dialect::Postgres => Capabilities {
                returning: true,
                on_conflict: true,
                ilike: true,
                lock_exclusive: true,
                lock_shared: true,
                lock_of: true,
                lock_nowait: true,
                lock_skip_locked: true,
                update_from_values: true,
                savepoints: true,
            },
        }
    }
}

impl Capabilities {
    /// `Err` naming the missing feature when `supported` is false.
    pub fn require(supported: bool, dialect: Dialect, feature: &str) -> Result<(), String> {
        if supported {
            Ok(())
        } else {
            Err(format!("{} does not support {feature}", dialect.name()))
        }
    }
}
