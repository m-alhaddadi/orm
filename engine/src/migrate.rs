//! Applying and reverting the migrations of a directory (`orm_core::migrate::files`) on
//! a database. The bindings and the `orm` CLI all run this one implementation.
//!
//! Applied migrations are recorded with the SHA-256 of their `up.sql` in the
//! `orm_migrations` table; a migration whose file changed after it was applied stops
//! [`upgrade`]. Each migration runs in its own transaction together with its row, under
//! an advisory lock, so a failing migration leaves the database at the previous one and
//! concurrent migrators apply each migration once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use orm_core::migrate::files::{self, MigrationFolder};

use crate::db::{Executor, Transaction};
use crate::error::{Error, Result};

pub const TABLE: &str = "orm_migrations";
/// Arbitrary constant: the advisory lock serializing concurrent migrators.
pub const LOCK_ID: i64 = 0x6F72_6D6D;

/// One migration folder.
#[derive(Debug, Clone)]
pub struct Migration {
    pub name: String,
    pub path: PathBuf,
}

impl Migration {
    fn read(&self, file: &str) -> Result<String> {
        let p = self.path.join(file);
        std::fs::read_to_string(&p).map_err(|e| Error::Migration(format!("{}: {e}", p.display())))
    }

    pub fn up_sql(&self) -> Result<String> {
        self.read("up.sql")
    }

    pub fn down_sql(&self) -> Result<String> {
        self.read("down.sql")
    }

    /// Hex SHA-256 of `up.sql`.
    pub fn checksum(&self) -> Result<String> {
        Ok(checksum(&self.up_sql()?))
    }
}

pub fn checksum(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

impl From<MigrationFolder> for Migration {
    fn from(f: MigrationFolder) -> Self {
        Migration { name: f.name, path: f.path }
    }
}

/// The migrations of `dir`, in order.
pub fn list(dir: &Path) -> Result<Vec<Migration>> {
    Ok(files::list(dir).map_err(Error::Migration)?.into_iter().map(Migration::from).collect())
}

/// A migration by folder name or number (`"3"`, `"0003"`, `"0003_add_tags"`).
pub fn find(dir: &Path, name: &str) -> Result<Migration> {
    files::find(dir, name).map(Migration::from).map_err(Error::Migration)
}

#[derive(Debug, Clone)]
pub struct Status {
    pub migration: Migration,
    pub applied: bool,
    /// When it was applied, as Postgres prints a `timestamptz`.
    pub applied_at: Option<String>,
}

/// What [`downgrade`] reverts.
#[derive(Debug, Clone)]
pub enum Down {
    /// The last `n` applied migrations.
    Steps(usize),
    /// Every migration after this one; `"zero"` reverts all.
    To(String),
}

fn lit(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

async fn ensure_table(conn: &dyn Executor) -> Result<()> {
    conn.batch(format!(
        "CREATE TABLE IF NOT EXISTS {TABLE} (name text PRIMARY KEY, checksum text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now())"
    ))
    .await?;
    Ok(())
}

/// name → (checksum, applied_at)
async fn applied(conn: &dyn Executor) -> Result<BTreeMap<String, (String, String)>> {
    let rows = conn.query_text(format!("SELECT name, checksum, applied_at::text FROM {TABLE} ORDER BY name")).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let mut it = r.into_iter();
            match (it.next().flatten(), it.next().flatten(), it.next().flatten()) {
                (Some(n), Some(c), Some(a)) => Some((n, (c, a))),
                _ => None,
            }
        })
        .collect())
}

/// Every migration of `dir` and whether it is applied. Migrations recorded in the
/// database but missing from the directory are an error.
pub async fn status(conn: &dyn Executor, dir: &Path) -> Result<Vec<Status>> {
    ensure_table(conn).await?;
    let mut done = applied(conn).await?;
    let out: Vec<Status> = list(dir)?
        .into_iter()
        .map(|m| {
            let at = done.remove(&m.name).map(|(_, at)| at);
            Status { applied: at.is_some(), applied_at: at, migration: m }
        })
        .collect();
    if !done.is_empty() {
        let names: Vec<&str> = done.keys().map(String::as_str).collect();
        return Err(Error::Migration(format!(
            "the database has migrations missing from the directory: {}",
            names.join(", ")
        )));
    }
    Ok(out)
}

/// Applied migrations must be a prefix of the directory and unchanged since.
fn verify(statuses: &[Status], applied: &BTreeMap<String, (String, String)>) -> Result<()> {
    let mut pending: Option<&str> = None;
    for s in statuses {
        if !s.applied {
            pending = pending.or(Some(&s.migration.name));
        } else if let Some(p) = pending {
            return Err(Error::Migration(format!(
                "{} is applied but the earlier {p} is not; renumber the unapplied migration after the applied ones",
                s.migration.name
            )));
        } else if applied[&s.migration.name].0 != s.migration.checksum()? {
            return Err(Error::Migration(format!("{}/up.sql changed after it was applied", s.migration.name)));
        }
    }
    Ok(())
}

/// In one transaction holding the migration lock: runs the scripts `scripts` gives for
/// the migrations applied by then (none: nothing to do), and commits.
async fn locked(
    conn: &dyn Executor,
    scripts: impl FnOnce(&BTreeMap<String, (String, String)>) -> Option<[String; 2]>,
) -> Result<bool> {
    let tx = conn.begin().await?;
    let run = async {
        tx.batch(format!("SELECT pg_advisory_xact_lock({LOCK_ID})")).await?;
        let Some(scripts) = scripts(&applied(&*tx).await?) else { return Ok(false) };
        for sql in scripts {
            tx.batch(sql).await?;
        }
        Ok::<_, Error>(true)
    };
    match run.await {
        Ok(ran) => {
            tx.commit().await?;
            Ok(ran)
        }
        Err(e) => {
            let _ = Transaction::rollback(&*tx).await;
            Err(e)
        }
    }
}

/// Applies pending migrations up to and including `target` (default: all); gives the
/// ones it applied.
pub async fn upgrade(conn: &dyn Executor, dir: &Path, target: Option<&str>) -> Result<Vec<Migration>> {
    let statuses = status(conn, dir).await?;
    verify(&statuses, &applied(conn).await?)?;
    let stop = target.map(|t| find(dir, t)).transpose()?.map(|m| m.name);
    let mut done = vec![];
    for s in statuses {
        let m = s.migration;
        if s.applied || stop.as_ref().is_some_and(|stop| m.name > *stop) {
            continue;
        }
        let up = m.up_sql()?;
        let record = format!("INSERT INTO {TABLE} (name, checksum) VALUES ({}, {})", lit(&m.name), lit(&checksum(&up)));
        // skipped if another migrator applied it meanwhile
        let ran = locked(conn, |now| (!now.contains_key(&m.name)).then_some([up, record])).await?;
        if ran {
            done.push(m);
        }
    }
    Ok(done)
}

/// Reverts applied migrations, newest first; gives the ones it reverted.
pub async fn downgrade(conn: &dyn Executor, dir: &Path, down: Down) -> Result<Vec<Migration>> {
    let applied: Vec<Migration> = status(conn, dir).await?.into_iter().filter(|s| s.applied).map(|s| s.migration).collect();
    let revert: Vec<Migration> = match down {
        Down::To(t) => {
            let keep = if t == "zero" { String::new() } else { find(dir, &t)?.name };
            applied.into_iter().filter(|m| m.name > keep).collect()
        }
        Down::Steps(n) => {
            let skip = applied.len().saturating_sub(n);
            applied.into_iter().skip(skip).collect()
        }
    };
    let mut done = vec![];
    for m in revert.into_iter().rev() {
        let scripts = [m.down_sql()?, format!("DELETE FROM {TABLE} WHERE name = {}", lit(&m.name))];
        if locked(conn, |now| now.contains_key(&m.name).then_some(scripts)).await? {
            done.push(m);
        }
    }
    Ok(done)
}
