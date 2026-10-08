//! SQLite DDL and snapshot migrations. Structural changes rebuild managed tables.
use super::{diff, model::{DbSchema, Index, Renames, Table, Trigger}, pg::ident, Step};
use crate::ir::{Deferrable, OnDelete, TriggerEvent, TriggerTiming};
use crate::schema::Result;

fn ids(names: &[String]) -> String { names.iter().map(|n| ident(n)).collect::<Vec<_>>().join(", ") }

fn action(a: OnDelete) -> &'static str {
    match a {
        OnDelete::Cascade => "CASCADE", OnDelete::SetNull => "SET NULL",
        OnDelete::SetDefault => "SET DEFAULT", OnDelete::Restrict => "RESTRICT", OnDelete::NoAction => "NO ACTION",
    }
}

pub fn create_table(t: &Table, name: &str, idempotent: bool) -> String {
    let mut cols = t.columns.iter().map(|c| {
        let mut sql = format!("{} {}", ident(&c.name), c.ty);
        if c.identity { sql.push_str(" PRIMARY KEY AUTOINCREMENT"); }
        if !c.nullable { sql.push_str(" NOT NULL"); }
        if let Some(d) = &c.default { sql.push_str(&format!(" DEFAULT ({d})")); }
        sql
    }).collect::<Vec<_>>();
    if !t.columns.iter().any(|c| c.identity) {
        if let Some(pk) = &t.primary_key { cols.push(format!("CONSTRAINT {} PRIMARY KEY ({})", ident(&pk.name), ids(&pk.columns))); }
    }
    for u in &t.uniques { cols.push(format!("CONSTRAINT {} UNIQUE ({})", ident(&u.name), ids(&u.columns))); }
    for c in &t.checks { cols.push(format!("CONSTRAINT {} CHECK ({})", ident(&c.name), c.expr)); }
    for fk in &t.foreign_keys {
        let mut sql = format!("CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})", ident(&fk.name), ids(&fk.columns), ident(&fk.ref_table), ids(&fk.ref_columns));
        if let Some(a) = fk.on_delete { sql.push_str(&format!(" ON DELETE {}", action(a))); }
        if let Some(a) = fk.on_update { sql.push_str(&format!(" ON UPDATE {}", action(a))); }
        match fk.deferrable {
            Some(Deferrable::Immediate) => sql.push_str(" DEFERRABLE INITIALLY IMMEDIATE"),
            Some(Deferrable::Deferred) => sql.push_str(" DEFERRABLE INITIALLY DEFERRED"), None => {}
        }
        cols.push(sql);
    }
    format!("CREATE TABLE {}{} (\n    {}\n)", if idempotent { "IF NOT EXISTS " } else { "" }, ident(name), cols.join(",\n    "))
}

fn index(t: &str, ix: &Index, idempotent: bool) -> String {
    let keys = ix.keys.iter().map(|k| {
        let mut sql = k.column.as_ref().map(|c| ident(c)).unwrap_or_else(|| format!("({})", k.expr.as_deref().unwrap_or("")));
        if let Some(c) = &k.collation { sql.push_str(&format!(" COLLATE {}", ident(c))); }
        if k.desc { sql.push_str(" DESC"); }
        sql
    }).collect::<Vec<_>>().join(", ");
    let mut sql = format!("CREATE {}INDEX {}{} ON {} ({keys})", if ix.unique { "UNIQUE " } else { "" }, if idempotent { "IF NOT EXISTS " } else { "" }, ident(&ix.name), ident(t));
    if let Some(w) = &ix.where_ { sql.push_str(&format!(" WHERE {w}")); }
    sql
}

fn trigger(t: &str, tr: &Trigger, idempotent: bool) -> String {
    // Trigger names are global in SQLite; qualify the schema-local name by its table.
    let name = super::model::object_name(&[t, &tr.name, "trigger"]);
    let timing = match tr.timing { TriggerTiming::Before => "BEFORE", TriggerTiming::After => "AFTER", TriggerTiming::InsteadOf => "INSTEAD OF" };
    let event = match tr.events[0] {
        TriggerEvent::Insert => "INSERT".into(), TriggerEvent::Delete => "DELETE".into(),
        TriggerEvent::Update if !tr.update_of.is_empty() => format!("UPDATE OF {}", ids(&tr.update_of)),
        TriggerEvent::Update => "UPDATE".into(), TriggerEvent::Truncate => unreachable!("validated schema"),
    };
    let when = tr.when.as_ref().map(|w| format!(" WHEN ({w})")).unwrap_or_default();
    let body = tr.body.as_deref().unwrap_or("").trim();
    format!("CREATE TRIGGER {}{} {timing} {event} ON {} FOR EACH ROW{when} BEGIN\n{};\nEND", if idempotent { "IF NOT EXISTS " } else { "" }, ident(&name), ident(t), body.trim_end_matches(';'))
}

pub fn create_all(db: &DbSchema, idempotent: bool) -> Vec<String> {
    let mut sql = db.tables.iter().map(|t| create_table(t, &t.name, idempotent)).collect::<Vec<_>>();
    for t in &db.tables {
        sql.extend(t.indexes.iter().map(|ix| index(&t.name, ix, idempotent)));
        sql.extend(t.triggers.iter().map(|tr| trigger(&t.name, tr, idempotent)));
    }
    sql
}

pub fn steps(from: &DbSchema, to: &DbSchema, renames: &Renames) -> Result<Vec<Step>> {
    if from.tables == to.tables { return Ok(vec![]); }
    let mut sql = vec![];
    // Rebuilding drops objects attached to a table. Refuse objects absent from the
    // previous snapshot rather than silently destroying user-created indexes/triggers.
    if !from.tables.is_empty() {
        sql.push("CREATE TEMP TABLE __orm_object_guard (name TEXT CONSTRAINT orm_unmanaged_index_or_trigger CHECK (name IS NULL))".into());
        for t in &from.tables {
            let mut names = t.indexes.iter().map(|ix| super::model::quote_literal(&ix.name)).collect::<Vec<_>>();
            names.extend(t.triggers.iter().map(|tr| super::model::quote_literal(&super::model::object_name(&[&t.name, &tr.name, "trigger"]))));
            let known = if names.is_empty() { String::new() } else { format!(" AND name NOT IN ({})", names.join(", ")) };
            sql.push(format!("INSERT INTO __orm_object_guard SELECT name FROM sqlite_schema WHERE tbl_name = {} AND type IN ('index', 'trigger') AND sql IS NOT NULL{known}", super::model::quote_literal(&t.name)));
        }
        sql.push("DROP TABLE __orm_object_guard".into());
    }
    // Foreign keys must be disabled BEFORE the runner starts the migration transaction.
    // Retain AUTOINCREMENT high-water marks even when their highest rows were deleted.
    let sequence = from.tables.iter().any(|t| t.columns.iter().any(|c| c.identity));
    if sequence {
        sql.push("CREATE TEMP TABLE __orm_sequence AS SELECT name, seq FROM sqlite_sequence".into());
    }
    // A rename hint applies only when the current name is new and the hinted name exists, as in `diff::diff`;
    // a hint kept after its migration must not redirect the copy.
    let source = |t: &Table| from.tables.iter().find(|o| o.name == t.name).or_else(|| {
        renames.tables.get(&t.name).and_then(|h| from.tables.iter().find(|o| &o.name == h)).filter(|o| !to.tables.iter().any(|n| n.name == o.name))
    });
    for t in &to.tables {
        if t.name.starts_with("__orm_") { return Err("SQLite table names beginning with __orm_ are reserved".into()); }
        let temp = format!("__orm_new_{}", t.name);
        sql.push(create_table(t, &temp, false));
        if let Some(old) = source(t) {
            let values = t.columns.iter().map(|c| {
                let old_col = old.column(&c.name).map(|_| &c.name).or_else(|| {
                    renames.columns.get(&(t.name.clone(), c.name.clone())).filter(|h| old.column(h).is_some() && t.column(h).is_none())
                });
                match old_col { Some(col) => ident(col), None => c.default.clone().unwrap_or_else(|| "NULL".into()) }
            }).collect::<Vec<_>>().join(", ");
            sql.push(format!("INSERT INTO {} ({}) SELECT {values} FROM {}", ident(&temp), ids(&t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>()), ident(&old.name)));
        }
    }
    for old in &from.tables { sql.push(format!("DROP TABLE {}", ident(&old.name))); }
    for t in &to.tables {
        sql.push(format!("ALTER TABLE {} RENAME TO {}", ident(&format!("__orm_new_{}", t.name)), ident(&t.name)));
        if sequence && t.columns.iter().any(|c| c.identity) {
            let old_name = source(t).map_or(&t.name, |o| &o.name);
            sql.push(format!("UPDATE sqlite_sequence SET seq = MAX(seq, COALESCE((SELECT seq FROM __orm_sequence WHERE name = {}), 0)) WHERE name = {}", super::model::quote_literal(old_name), super::model::quote_literal(&t.name)));
        }
        sql.extend(t.indexes.iter().map(|ix| index(&t.name, ix, false)));
    }
    // SQLite resolves the tables of a trigger body when it creates the trigger.
    for t in &to.tables {
        sql.extend(t.triggers.iter().map(|tr| trigger(&t.name, tr, false)));
    }
    if sequence { sql.push("DROP TABLE __orm_sequence".into()); }
    let warnings = diff::diff(from, to, renames).iter().filter_map(|op| op.warning()).collect::<Vec<_>>();
    Ok(vec![Step { summary: "Rebuild SQLite schema and preserve existing rows".into(), sql: sql.join(";\n"), warning: if warnings.is_empty() { None } else { Some(warnings.join("; ")) } }])
}
