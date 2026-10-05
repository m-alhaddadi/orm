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
    Sqlite,
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
    /// `SELECT DISTINCT ON (...)`.
    pub distinct_on: bool,
    /// Most bound parameters one statement can take (the wire protocol's limit).
    /// Statements with more values are split: `update_many` batches, prefetch keys.
    pub max_params: usize,
}

impl Dialect {
    pub fn is_postgres(&self) -> bool { *self == Self::Postgres }

    pub const fn name(self) -> &'static str {
        match self {
            Dialect::Postgres => "postgres",
            Dialect::Sqlite => "sqlite",
        }
    }

    pub const fn capabilities(self) -> Capabilities {
        match self {
            Dialect::Sqlite => Capabilities {
                returning: true, on_conflict: true, ilike: false,
                lock_exclusive: false, lock_shared: false, lock_of: false,
                lock_nowait: false, lock_skip_locked: false,
                update_from_values: false, savepoints: true, distinct_on: false,
                max_params: 32_766,
            },
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
                distinct_on: true,
                max_params: 65_535,
            },
        }
    }
}

/// A dialect with the capabilities queries are planned for: the dialect's own, minus
/// any switched off (tests run the fallback paths on Postgres this way).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub dialect: Dialect,
    pub caps: Capabilities,
}

impl Target {
    pub const fn new(dialect: Dialect) -> Self {
        Target { dialect, caps: dialect.capabilities() }
    }

    /// The same target with the named capabilities switched off; `max_params=N` lowers
    /// the parameter limit (tests run the splitting paths with small inputs this way).
    pub fn without(mut self, features: &[String]) -> Result<Self, String> {
        for f in features {
            if let Some(n) = f.strip_prefix("max_params=") {
                let n: usize = n.parse().map_err(|_| format!("invalid {f:?}"))?;
                self.caps.max_params = n.clamp(2, self.caps.max_params);
                continue;
            }
            let flag = match f.as_str() {
                "returning" => &mut self.caps.returning,
                "on_conflict" => &mut self.caps.on_conflict,
                "ilike" => &mut self.caps.ilike,
                "lock_exclusive" => &mut self.caps.lock_exclusive,
                "lock_shared" => &mut self.caps.lock_shared,
                "lock_of" => &mut self.caps.lock_of,
                "lock_nowait" => &mut self.caps.lock_nowait,
                "lock_skip_locked" => &mut self.caps.lock_skip_locked,
                "update_from_values" => &mut self.caps.update_from_values,
                "savepoints" => &mut self.caps.savepoints,
                "distinct_on" => &mut self.caps.distinct_on,
                other => return Err(format!("unknown capability {other:?}")),
            };
            *flag = false;
        }
        Ok(self)
    }

    /// `Err` naming the missing feature when `supported` is false.
    pub fn require(&self, supported: bool, feature: &str) -> Result<(), String> {
        Capabilities::require(supported, self.dialect, feature)
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
