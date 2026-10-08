//! Cross-language advisory lock keys and SQL. Validation stays in the frontends.
use blake2::{digest::consts::U8, Blake2b, Digest};

/// BLAKE2b with an eight-byte digest (not a truncated 64-byte digest).
pub fn key(name: &[u8]) -> i64 {
    i64::from_be_bytes(Blake2b::<U8>::digest(name).into())
}

pub fn sql(key: i64, exclusive: bool, nowait: bool) -> String {
    format!(
        "SELECT pg_{}advisory_xact_lock{}({key})::text",
        if nowait { "try_" } else { "" },
        if exclusive { "" } else { "_shared" }
    )
}

/// A session lock: held until it is unlocked or the connection closes, across commits.
pub fn session_sql(key: i64, exclusive: bool, nowait: bool) -> String {
    format!(
        "SELECT pg_{}advisory_lock{}({key})::text",
        if nowait { "try_" } else { "" },
        if exclusive { "" } else { "_shared" }
    )
}

pub fn unlock_sql(key: i64, exclusive: bool) -> String {
    format!("SELECT pg_advisory_unlock{}({key})::text", if exclusive { "" } else { "_shared" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_lock_sql() {
        assert_eq!(session_sql(7, true, false), "SELECT pg_advisory_lock(7)::text");
        assert_eq!(session_sql(-7, false, true), "SELECT pg_try_advisory_lock_shared(-7)::text");
        assert_eq!(unlock_sql(7, false), "SELECT pg_advisory_unlock_shared(7)::text");
    }

    #[test]
    fn matches_python_blake2b_8() {
        for (name, expected) in [
            (String::new(), -1970711489451281740),
            ("import".into(), -3453690058906639106),
            ("ورود 🔒".into(), -9085093355493825237),
            ("x".repeat(128), -8062319141637250046),
            ("x".repeat(129), 1791892490181060003),
            ("x".repeat(4096), 860336987148385126),
        ] {
            assert_eq!(key(name.as_bytes()), expected);
        }
    }
}
