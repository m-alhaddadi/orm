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
use std::sync::Arc;

use sha2::{Digest, Sha256};

use orm_core::dialect::Dialect;
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
    let timestamp = match conn.dialect() {
        Dialect::Postgres => "timestamptz NOT NULL DEFAULT now()",
        Dialect::Sqlite => "text NOT NULL DEFAULT CURRENT_TIMESTAMP",
    };
    conn.batch(format!("CREATE TABLE IF NOT EXISTS {TABLE} (name text PRIMARY KEY, checksum text NOT NULL, applied_at {timestamp})"))
    .await?;
    Ok(())
}

/// name → (checksum, applied_at)
async fn applied(conn: &dyn Executor) -> Result<BTreeMap<String, (String, String)>> {
    let rows = conn.query_text(format!("SELECT name, checksum, CAST(applied_at AS text) FROM {TABLE} ORDER BY name")).await?;
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
    for m in list(dir)? {
        let snapshot = m.path.join("snapshot.json");
        if snapshot.exists() {
            let text = std::fs::read_to_string(&snapshot).map_err(|e| Error::Migration(e.to_string()))?;
            let target = orm_core::migrate::parse_snapshot(&text).map_err(Error::Migration)?.dialect;
            if target != conn.dialect() {
                return Err(Error::Migration(format!("{} targets {}, connection uses {}", m.name, target.name(), conn.dialect().name())));
            }
        }
    }
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
    let tx = conn.begin_migration().await?;
    let run = async {
        if conn.dialect() == Dialect::Postgres {
            tx.batch(format!("SELECT pg_advisory_xact_lock({LOCK_ID})")).await?;
        }
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

/// Files a migration may hold that run in a host language, not in this engine.
pub const DATA_FILES: [&str; 2] = ["data.py", "data.ts"];

impl Migration {
    /// The data step (`data.py` / `data.ts`) of this migration, if it has one.
    pub fn data_file(&self) -> Option<&'static str> {
        DATA_FILES.into_iter().find(|f| self.path.join(f).is_file())
    }
}

/// The migrations [`upgrade`] would apply, in order: pending ones up to and including
/// `target` (default: all), after the same checks.
pub async fn pending(conn: &dyn Executor, dir: &Path, target: Option<&str>) -> Result<Vec<Migration>> {
    let statuses = status(conn, dir).await?;
    verify(&statuses, &applied(conn).await?)?;
    let stop = target.map(|t| find(dir, t)).transpose()?.map(|m| m.name);
    Ok(statuses
        .into_iter()
        .filter(|s| !s.applied && stop.as_ref().is_none_or(|stop| s.migration.name <= *stop))
        .map(|s| s.migration)
        .collect())
}

/// Starts applying `m`: a migration transaction holding the migration lock, with
/// `up.sql` run in it. `None` when another migrator applied `m` meanwhile. The caller
/// runs the data step in the transaction, then [`finish_apply`] (or rolls back).
pub async fn begin_apply(conn: &dyn Executor, m: &Migration) -> Result<Option<Arc<dyn Transaction>>> {
    let up = m.up_sql()?;
    let tx = conn.begin_migration().await?;
    let run = async {
        if conn.dialect() == Dialect::Postgres {
            tx.batch(format!("SELECT pg_advisory_xact_lock({LOCK_ID})")).await?;
        }
        if applied(&*tx).await?.contains_key(&m.name) {
            return Ok(false);
        }
        tx.batch(up).await?;
        Ok::<_, Error>(true)
    };
    match run.await {
        Ok(true) => Ok(Some(tx)),
        Ok(false) => {
            let _ = Transaction::rollback(&*tx).await;
            Ok(None)
        }
        Err(e) => {
            let _ = Transaction::rollback(&*tx).await;
            Err(e)
        }
    }
}

/// Records `m` in the transaction of [`begin_apply`] and commits; rolls back on error.
pub async fn finish_apply(tx: &dyn Transaction, m: &Migration) -> Result<()> {
    let record = format!("INSERT INTO {TABLE} (name, checksum) VALUES ({}, {})", lit(&m.name), lit(&m.checksum()?));
    match tx.batch(record).await {
        Ok(_) => Ok(tx.commit().await?),
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e.into())
        }
    }
}

/// Applies pending migrations up to and including `target` (default: all); gives the
/// ones it applied. A migration with a data step (`data.py`, `data.ts`) is refused
/// before anything runs: the Python or TypeScript migrator applies it.
pub async fn upgrade(conn: &dyn Executor, dir: &Path, target: Option<&str>) -> Result<Vec<Migration>> {
    let todo = pending(conn, dir, target).await?;
    if let Some((m, file)) = todo.iter().find_map(|m| m.data_file().map(|f| (m, f))) {
        let tool = if file == "data.py" { "python -m orm migrate (or Migrator.upgrade())" } else { "npx orm migrate (or Migrator.upgrade())" };
        return Err(Error::Migration(format!("{} has a data step ({file}); apply it with {tool}", m.name)));
    }
    let mut done = vec![];
    for m in todo {
        if let Some(tx) = begin_apply(conn, &m).await? {
            finish_apply(&*tx, &m).await?;
            done.push(m);
        }
    }
    Ok(done)
}

/// The names of the applied migrations, in order.
pub async fn applied_names(conn: &dyn Executor) -> Result<Vec<String>> {
    ensure_table(conn).await?;
    Ok(applied(conn).await?.into_keys().collect())
}

/// Records the first migration of `dir` as applied without running it, for a database
/// that already has its schema (`orm pull`). Refused once any migration is applied.
pub async fn baseline(conn: &dyn Executor, dir: &Path) -> Result<Migration> {
    let first = list(dir)?.into_iter().next().ok_or_else(|| Error::Migration(format!("no migration in {}", dir.display())))?;
    ensure_table(conn).await?;
    let record = format!("INSERT INTO {TABLE} (name, checksum) VALUES ({}, {})", lit(&first.name), lit(&first.checksum()?));
    let mut applied = vec![];
    let ran = locked(conn, |now| {
        applied = now.keys().cloned().collect();
        now.is_empty().then(|| ["SELECT 1".to_owned(), record])
    })
    .await?;
    if !ran {
        return Err(Error::Migration(format!("the database already has applied migrations ({}); baseline only marks the first", applied.join(", "))));
    }
    Ok(first)
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
