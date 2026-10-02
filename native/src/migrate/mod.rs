//! Migration generator.
//!
//! ```text
//! ORM schema ──build──▶ DbSchema (snapshot) ──diff(previous snapshot)──▶ [Op] ──render──▶ SQL
//! ```
//!
//! The previous snapshot is whatever the last migration recorded, so generating a
//! migration needs no database. The down migration is the diff in the other direction.

pub mod diff;
pub mod model;
pub mod pg;

use serde::Serialize;

pub use diff::Op;
pub use model::DbSchema;

use crate::schema::{Result, Schema};

#[derive(Serialize, Debug)]
pub struct Step {
    pub summary: String,
    pub sql: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct MigrationPlan {
    pub up: Vec<Step>,
    pub down: Vec<Step>,
    /// The schema after `up`: the snapshot to store with the migration.
    pub snapshot: DbSchema,
}

fn steps(ops: &[Op]) -> Vec<Step> {
    ops.iter()
        .map(|op| Step { summary: pg::summary(op), sql: pg::render(op, false), warning: op.warning() })
        .collect()
}

pub fn parse_snapshot(json: &str) -> Result<DbSchema> {
    let s: DbSchema = serde_json::from_str(json).map_err(|e| format!("invalid schema snapshot: {e}"))?;
    if s.version > model::SNAPSHOT_VERSION {
        return Err(format!(
            "schema snapshot version {} is newer than this engine supports ({})",
            s.version,
            model::SNAPSHOT_VERSION
        ));
    }
    Ok(s)
}

pub fn snapshot(schema: &Schema) -> Result<DbSchema> {
    model::build(schema).map(|(s, _)| s)
}

/// The migration from `previous` (an earlier snapshot; empty for the first migration)
/// to `schema`.
pub fn plan(schema: &Schema, previous: &DbSchema) -> Result<MigrationPlan> {
    let (current, renames) = model::build(schema)?;
    let up = diff::diff(previous, &current, &renames);
    let down = diff::diff(&current, previous, &renames.reversed());
    Ok(MigrationPlan { up: steps(&up), down: steps(&down), snapshot: current })
}

/// The whole schema as idempotent DDL (`IF NOT EXISTS` / `OR REPLACE`), for
/// `create_tables()` in development and tests.
pub fn create_all(schema: &Schema) -> Result<Vec<String>> {
    let current = snapshot(schema)?;
    let ops = diff::diff(&DbSchema::default(), &current, &Default::default());
    Ok(ops.iter().map(|op| pg::render(op, true)).collect())
}

/// Drops every table and generated function of the schema (`IF EXISTS ... CASCADE`);
/// extensions stay.
pub fn drop_all(schema: &Schema) -> Result<Vec<String>> {
    let current = snapshot(schema)?;
    let mut out: Vec<String> = current
        .tables
        .iter()
        .rev()
        .map(|t| format!("DROP TABLE IF EXISTS {} CASCADE", pg::ident(&t.name)))
        .collect();
    out.extend(
        current.functions.iter().map(|f| format!("DROP FUNCTION IF EXISTS {}({}) CASCADE", pg::ident(&f.name), f.args)),
    );
    Ok(out)
}

#[cfg(test)]
mod tests;
