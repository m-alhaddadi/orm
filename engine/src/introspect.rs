//! Reading a live database into a [`DbSchema`], the model the migration generator
//! diffs, for `orm pull` and drift detection.
//!
//! Postgres is read from `pg_catalog` for the current schema; SQLite from
//! `sqlite_master` and its `PRAGMA`s. Expressions, defaults and types keep the
//! database's own text (`pg_get_expr`, `format_type`, the declared SQLite type), so a
//! live database is compared with a snapshot through a shadow copy of the snapshot that
//! is read back the same way ([`drift`]), never by text against the snapshot.
//!
//! What a [`DbSchema`] cannot hold (rules, views, generated columns, policies, ...)
//! goes into [`Introspection::gaps`].

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use orm_core::dialect::Dialect;
use orm_core::ir::{Deferrable, ForEach, Nulls, OnDelete, TriggerEvent, TriggerTiming};
use orm_core::migrate::model::{
    Check, Column, DbSchema, EnumType, Exclusion, ExclusionElement, Extension, ForeignKey, Function, Index, IndexKey,
    PrimaryKey, Table, Trigger, Unique,
};
use orm_core::migrate::{diff, Op, Step};

use crate::db::Executor;
use crate::error::{Error, Result};
use crate::migrate::TABLE as MIGRATIONS_TABLE;

/// A database read into the snapshot model.
#[derive(Debug, Clone, Default)]
pub struct Introspection {
    pub schema: DbSchema,
    /// Objects the schema language can't describe, one line each.
    pub gaps: Vec<String>,
}

fn bad(e: impl std::fmt::Display) -> Error {
    Error::Migration(format!("introspection: {e}"))
}

/// One JSON value from a query that selects a single JSON column.
async fn json<T: for<'de> Deserialize<'de>>(conn: &dyn Executor, sql: &str) -> Result<T> {
    let rows = conn.query_text(sql.to_owned()).await?;
    let text = rows.into_iter().next().and_then(|r| r.into_iter().next().flatten()).unwrap_or_else(|| "[]".into());
    serde_json::from_str(&text).map_err(bad)
}

/// The live schema of `conn`: Postgres' current schema, or the SQLite database.
pub async fn introspect(conn: &dyn Executor) -> Result<Introspection> {
    match conn.dialect() {
        Dialect::Postgres => postgres(conn).await,
        Dialect::Sqlite => sqlite(conn).await,
    }
}

// ---------------------------------------------------------------------------------------
// Postgres
// ---------------------------------------------------------------------------------------

/// Objects owned by an extension (`CREATE EXTENSION`) belong to it, not to the schema.
const NOT_EXTENSION: &str = "NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = {oid} AND d.deptype = 'e')";

fn not_extension(oid: &str) -> String {
    NOT_EXTENSION.replace("{oid}", oid)
}

#[derive(Deserialize)]
struct PgTable {
    oid: i64,
    name: String,
    kind: String,
    partition: bool,
    comment: Option<String>,
    rls: bool,
    force_rls: bool,
    unlogged: bool,
    options: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct PgColumn {
    table: i64,
    name: String,
    #[serde(rename = "type")]
    ty: String,
    notnull: bool,
    default: Option<String>,
    identity: String,
    generated: String,
    comment: Option<String>,
    collation: Option<String>,
}

#[derive(Deserialize)]
struct PgConstraint {
    table: i64,
    name: String,
    #[serde(rename = "type")]
    kind: String,
    columns: Option<Vec<String>>,
    ref_table: Option<String>,
    ref_schema: Option<String>,
    #[serde(default)]
    ref_local: bool,
    ref_columns: Option<Vec<String>>,
    on_delete: String,
    on_update: String,
    deferrable: bool,
    deferred: bool,
    def: String,
    index: i64,
    validated: bool,
    nulls_not_distinct: Option<bool>,
    ops: Option<Vec<String>>,
}

#[derive(Deserialize, Clone)]
struct PgIndexKey {
    column: Option<String>,
    def: String,
    desc: bool,
    nulls_first: bool,
    opclass: Option<String>,
    collation: Option<String>,
}

#[derive(Deserialize)]
struct PgIndex {
    table: i64,
    oid: i64,
    name: String,
    unique: bool,
    method: String,
    #[serde(rename = "where")]
    where_: Option<String>,
    nulls_not_distinct: bool,
    options: Option<Vec<String>>,
    constraint: bool,
    keys: Vec<PgIndexKey>,
    include: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct PgTrigger {
    table: i64,
    name: String,
    #[serde(rename = "type")]
    bits: i32,
    columns: Option<Vec<String>>,
    function: String,
    function_schema: String,
    args: String,
    def: String,
    constraint: bool,
    enabled: String,
    transition: bool,
}

#[derive(Deserialize)]
struct PgFunction {
    name: String,
    args: String,
    returns: Option<String>,
    language: String,
    volatility: String,
    security_definer: bool,
    body: String,
    kind: String,
}

#[derive(Deserialize)]
struct PgEnum {
    name: String,
    values: Vec<String>,
    comment: Option<String>,
}

#[derive(Deserialize)]
struct PgNamed {
    name: String,
    table: Option<String>,
    def: Option<String>,
}

#[derive(Deserialize)]
struct PgExtension {
    name: String,
    schema: String,
}

const IN_SCHEMA: &str = "n.nspname = current_schema()";

fn tables_sql() -> String {
    format!(
        "SELECT coalesce(json_agg(json_build_object('oid', c.oid::int8, 'name', c.relname, 'kind', c.relkind, \
         'partition', c.relispartition, 'comment', obj_description(c.oid, 'pg_class'), 'rls', c.relrowsecurity, \
         'force_rls', c.relforcerowsecurity, 'unlogged', c.relpersistence = 'u', 'options', c.reloptions) ORDER BY c.relname), '[]') \
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE {IN_SCHEMA} AND c.relkind IN ('r', 'p', 'v', 'm', 'f') AND c.relname <> '{MIGRATIONS_TABLE}' AND {}",
        not_extension("c.oid")
    )
}

const COLUMNS_SQL: &str = "SELECT coalesce(json_agg(json_build_object('table', a.attrelid::int8, 'name', a.attname, \
     'type', CASE WHEN t.typtype = 'e' THEN chr(34) || replace(t.typname, chr(34), chr(34) || chr(34)) || chr(34) \
                  WHEN et.typtype = 'e' THEN chr(34) || replace(et.typname, chr(34), chr(34) || chr(34)) || chr(34) || '[]' \
                  ELSE format_type(a.atttypid, a.atttypmod) END, 'notnull', a.attnotnull, 'default', pg_get_expr(d.adbin, d.adrelid), \
     'identity', a.attidentity::text, 'generated', a.attgenerated::text, 'comment', col_description(a.attrelid, a.attnum), \
     'collation', CASE WHEN a.attcollation <> t.typcollation THEN co.collname END) ORDER BY a.attrelid, a.attnum), '[]') \
     FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
     JOIN pg_type t ON t.oid = a.atttypid LEFT JOIN pg_type et ON et.oid = t.typelem AND t.typcategory = 'A' \
     LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
     LEFT JOIN pg_collation co ON co.oid = a.attcollation \
     WHERE n.nspname = current_schema() AND c.relkind IN ('r', 'p') AND a.attnum > 0 AND NOT a.attisdropped";

const CONSTRAINTS_SQL: &str = "SELECT coalesce(json_agg(json_build_object('table', con.conrelid::int8, 'name', con.conname, \
     'type', con.contype::text, \
     'columns', (SELECT json_agg(a.attname ORDER BY k.i) FROM unnest(con.conkey) WITH ORDINALITY k(n, i) \
                 JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.n), \
     'ref_table', rc.relname, 'ref_schema', rn.nspname, 'ref_local', coalesce(rn.nspname = current_schema(), true), \
     'ref_columns', (SELECT json_agg(a.attname ORDER BY k.i) FROM unnest(con.confkey) WITH ORDINALITY k(n, i) \
                     JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.n), \
     'on_delete', con.confdeltype::text, 'on_update', con.confupdtype::text, \
     'deferrable', con.condeferrable, 'deferred', con.condeferred, 'def', pg_get_constraintdef(con.oid), \
     'index', con.conindid::int8, 'validated', con.convalidated, \
     'nulls_not_distinct', (SELECT (to_jsonb(i) ->> 'indnullsnotdistinct')::bool FROM pg_index i WHERE i.indexrelid = con.conindid), \
     'ops', (SELECT json_agg(o.oprname ORDER BY k.i) FROM unnest(con.conexclop) WITH ORDINALITY k(op, i) \
             JOIN pg_operator o ON o.oid = k.op) \
     ) ORDER BY con.conname), '[]') \
     FROM pg_constraint con JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
     LEFT JOIN pg_class rc ON rc.oid = con.confrelid LEFT JOIN pg_namespace rn ON rn.oid = rc.relnamespace \
     WHERE n.nspname = current_schema() AND con.contype IN ('p', 'u', 'c', 'f', 'x')";

const INDEXES_SQL: &str = "SELECT coalesce(json_agg(json_build_object('table', i.indrelid::int8, 'oid', i.indexrelid::int8, \
     'name', ic.relname, 'unique', i.indisunique, 'method', am.amname, 'where', pg_get_expr(i.indpred, i.indrelid), \
     'nulls_not_distinct', coalesce((to_jsonb(i) ->> 'indnullsnotdistinct')::bool, false), 'options', ic.reloptions, \
     'constraint', EXISTS (SELECT 1 FROM pg_constraint x WHERE x.conindid = i.indexrelid AND x.contype IN ('p', 'u', 'x')), \
     'keys', (SELECT json_agg(json_build_object( \
         'column', (SELECT attname FROM pg_attribute WHERE attrelid = i.indrelid AND attnum = i.indkey[k - 1] AND i.indkey[k - 1] <> 0), \
         'def', pg_get_indexdef(i.indexrelid, k, true), \
         'desc', (i.indoption[k - 1] & 1) = 1, 'nulls_first', (i.indoption[k - 1] & 2) = 2, \
         'opclass', (SELECT oc.opcname FROM pg_opclass oc WHERE oc.oid = i.indclass[k - 1] AND NOT oc.opcdefault), \
         'collation', (SELECT co.collname FROM pg_collation co WHERE co.oid = i.indcollation[k - 1] AND co.oid <> 100 \
             AND co.oid IS DISTINCT FROM (SELECT attcollation FROM pg_attribute WHERE attrelid = i.indrelid AND attnum = i.indkey[k - 1]))) \
         ORDER BY k) FROM generate_series(1, i.indnkeyatts) k), \
     'include', (SELECT json_agg(a.attname ORDER BY k) FROM generate_series(i.indnkeyatts + 1, i.indnatts) k \
                 JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = i.indkey[k - 1]) \
     ) ORDER BY ic.relname), '[]') \
     FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid JOIN pg_class c ON c.oid = i.indrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace JOIN pg_am am ON am.oid = ic.relam \
     WHERE n.nspname = current_schema() AND c.relkind IN ('r', 'p')";

const TRIGGERS_SQL: &str = "SELECT coalesce(json_agg(json_build_object('table', t.tgrelid::int8, 'name', t.tgname, \
     'type', t.tgtype::int, \
     'columns', (SELECT json_agg(a.attname ORDER BY k.i) FROM unnest(t.tgattr::int2[]) WITH ORDINALITY k(n, i) \
                 JOIN pg_attribute a ON a.attrelid = t.tgrelid AND a.attnum = k.n), \
     'function', p.proname, 'function_schema', pn.nspname, 'args', encode(t.tgargs, 'hex'), \
     'def', pg_get_triggerdef(t.oid, true), 'constraint', t.tgconstraint <> 0, 'enabled', t.tgenabled::text, \
     'transition', t.tgoldtable IS NOT NULL OR t.tgnewtable IS NOT NULL) ORDER BY t.tgname), '[]') \
     FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
     JOIN pg_proc p ON p.oid = t.tgfoid JOIN pg_namespace pn ON pn.oid = p.pronamespace \
     WHERE n.nspname = current_schema() AND NOT t.tgisinternal";

fn functions_sql() -> String {
    format!(
        "SELECT coalesce(json_agg(json_build_object('name', p.proname, 'args', pg_get_function_arguments(p.oid), \
         'returns', pg_get_function_result(p.oid), 'language', l.lanname, 'volatility', p.provolatile::text, \
         'security_definer', p.prosecdef, 'body', p.prosrc, 'kind', p.prokind::text) ORDER BY p.proname, p.oid), '[]') \
         FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace JOIN pg_language l ON l.oid = p.prolang \
         WHERE {IN_SCHEMA} AND {}",
        not_extension("p.oid")
    )
}

fn enums_sql() -> String {
    format!(
        "SELECT coalesce(json_agg(json_build_object('name', t.typname, \
         'values', (SELECT json_agg(e.enumlabel ORDER BY e.enumsortorder) FROM pg_enum e WHERE e.enumtypid = t.oid), \
         'comment', obj_description(t.oid, 'pg_type')) ORDER BY t.typname), '[]') \
         FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE {IN_SCHEMA} AND t.typtype = 'e' AND {}",
        not_extension("t.oid")
    )
}

/// Objects that only become gaps: name, table and definition.
fn others_sql() -> String {
    format!(
        "SELECT coalesce(json_agg(x), '[]') FROM ( \
         SELECT 'rule' AS kind, rulename AS name, tablename AS \"table\", definition AS def FROM pg_rules WHERE schemaname = current_schema() \
         UNION ALL SELECT 'policy', policyname, tablename, NULL FROM pg_policies WHERE schemaname = current_schema() \
         UNION ALL SELECT 'sequence', c.relname, NULL, NULL FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE {IN_SCHEMA} AND c.relkind = 'S' AND {} \
             AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = c.oid AND d.deptype IN ('a', 'i')) \
         UNION ALL SELECT CASE t.typtype WHEN 'd' THEN 'domain' ELSE 'type' END, t.typname, NULL, NULL \
             FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
             WHERE {IN_SCHEMA} AND (t.typtype IN ('d', 'r', 'm') OR (t.typtype = 'c' AND (SELECT relkind FROM pg_class WHERE oid = t.typrelid) = 'c')) \
             AND {} \
         ) x",
        not_extension("c.oid"),
        not_extension("t.oid")
    )
}

#[derive(Deserialize)]
struct PgOther {
    kind: String,
    #[serde(flatten)]
    named: PgNamed,
}

const EXTENSIONS_SQL: &str = "SELECT coalesce(json_agg(json_build_object('name', e.extname, 'schema', n.nspname) \
     ORDER BY e.extname), '[]') FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace WHERE e.extname <> 'plpgsql'";

fn on_action(code: &str) -> Option<OnDelete> {
    match code {
        "r" => Some(OnDelete::Restrict),
        "c" => Some(OnDelete::Cascade),
        "n" => Some(OnDelete::SetNull),
        "d" => Some(OnDelete::SetDefault),
        _ => None,
    }
}

fn deferrable(deferrable: bool, deferred: bool) -> Option<Deferrable> {
    deferrable.then_some(if deferred { Deferrable::Deferred } else { Deferrable::Immediate })
}

/// `inner` without one pair of parentheses around all of it.
pub fn strip_parens(text: &str) -> &str {
    let t = text.trim();
    if !(t.starts_with('(') && t.ends_with(')')) {
        return t;
    }
    let mut depth = 0i32;
    let mut quote = false;
    for (i, ch) in t.char_indices() {
        match ch {
            '\'' => quote = !quote,
            '(' if !quote => depth += 1,
            ')' if !quote => {
                depth -= 1;
                if depth == 0 && i != t.len() - 1 {
                    return t;
                }
            }
            _ => {}
        }
    }
    t[1..t.len() - 1].trim()
}

fn index_key(k: &PgIndexKey) -> IndexKey {
    let nulls = match (k.desc, k.nulls_first) {
        (false, true) => Some(Nulls::First),
        (true, false) => Some(Nulls::Last),
        _ => None,
    };
    IndexKey {
        column: k.column.clone(),
        expr: if k.column.is_some() { None } else { Some(strip_parens(&k.def).to_owned()) },
        collation: k.collation.clone(),
        opclass: k.opclass.clone(),
        desc: k.desc,
        nulls,
    }
}

/// The `WHEN (...)` condition of a `CREATE TRIGGER` statement.
fn trigger_when(def: &str) -> Option<String> {
    let start = def.find(" WHEN (")? + " WHEN ".len();
    let end = def.rfind(" EXECUTE FUNCTION ").or_else(|| def.rfind(" EXECUTE PROCEDURE "))?;
    (end > start).then(|| strip_parens(&def[start..end]).to_owned())
}

fn trigger_args(hex: &str) -> Vec<String> {
    let bytes: Vec<u8> = (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect();
    let mut args: Vec<String> = bytes.split(|b| *b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
    args.pop(); // each argument ends with a NUL
    args
}

async fn postgres(conn: &dyn Executor) -> Result<Introspection> {
    let tables: Vec<PgTable> = json(conn, &tables_sql()).await?;
    let columns: Vec<PgColumn> = json(conn, COLUMNS_SQL).await?;
    let constraints: Vec<PgConstraint> = json(conn, CONSTRAINTS_SQL).await?;
    let indexes: Vec<PgIndex> = json(conn, INDEXES_SQL).await?;
    let triggers: Vec<PgTrigger> = json(conn, TRIGGERS_SQL).await?;
    let functions: Vec<PgFunction> = json(conn, &functions_sql()).await?;
    let enums: Vec<PgEnum> = json(conn, &enums_sql()).await?;
    let others: Vec<PgOther> = json(conn, &others_sql()).await?;
    let extensions: Vec<PgExtension> = json(conn, EXTENSIONS_SQL).await?;
    let current = conn.query_text("SELECT current_schema()".into()).await?;
    let current = current.into_iter().next().and_then(|r| r.into_iter().next().flatten()).unwrap_or_default();

    let mut out = Introspection::default();
    let gaps = &mut out.gaps;
    let db = &mut out.schema;
    db.dialect = Dialect::Postgres;
    db.version = 1;
    db.extensions = extensions
        .into_iter()
        .map(|e| Extension { name: e.name, schema: (e.schema != current).then_some(e.schema), version: None })
        .collect();
    db.enums = enums.into_iter().map(|e| EnumType { name: e.name, values: e.values, comment: e.comment }).collect();

    let mut by_oid: BTreeMap<i64, Table> = BTreeMap::new();
    let mut names: BTreeMap<i64, String> = BTreeMap::new();
    for t in tables {
        names.insert(t.oid, t.name.clone());
        match t.kind.as_str() {
            "r" if t.partition => gaps.push(format!("table {} is a partition; partitions are not supported", t.name)),
            "r" => {
                if t.rls || t.force_rls {
                    let force = if t.force_rls { " (forced)" } else { "" };
                    gaps.push(format!("table {} has row-level security{force}; it is not reproduced", t.name));
                }
                if t.unlogged {
                    gaps.push(format!("table {} is UNLOGGED; it is read as logged", t.name));
                }
                if let Some(options) = t.options.filter(|o| !o.is_empty()) {
                    gaps.push(format!("table {} has storage options ({}); they are not reproduced", t.name, options.join(", ")));
                }
                by_oid.insert(t.oid, Table { name: t.name, comment: t.comment, ..Default::default() });
            }
            "p" => gaps.push(format!("table {} is partitioned; partitioning is not supported", t.name)),
            "v" => gaps.push(format!("view {}: views are not supported", t.name)),
            "m" => gaps.push(format!("materialized view {}: views are not supported", t.name)),
            _ => gaps.push(format!("foreign table {}: foreign tables are not supported", t.name)),
        }
    }
    for c in columns {
        let Some(t) = by_oid.get_mut(&c.table) else { continue };
        if !c.generated.is_empty() {
            gaps.push(format!("column {}.{} is generated ({}); generated columns are not supported", t.name, c.name, c.default.unwrap_or_default()));
            continue;
        }
        if c.identity == "a" {
            gaps.push(format!("column {}.{} is GENERATED ALWAYS AS IDENTITY; the schema has BY DEFAULT only", t.name, c.name));
        }
        if let Some(coll) = &c.collation {
            gaps.push(format!("column {}.{} has collation {coll}; column collations are not supported", t.name, c.name));
        }
        t.columns.push(Column {
            name: c.name,
            ty: c.ty,
            nullable: !c.notnull,
            default: c.default,
            identity: !c.identity.is_empty(),
            comment: c.comment,
        });
    }
    let index_keys: BTreeMap<i64, &PgIndex> = indexes.iter().map(|i| (i.oid, i)).collect();
    for c in constraints {
        let Some(t) = by_oid.get_mut(&c.table) else { continue };
        let columns = c.columns.clone().unwrap_or_default();
        if !c.validated {
            gaps.push(format!("constraint {}.{} is NOT VALID; it is read as valid", t.name, c.name));
        }
        match c.kind.as_str() {
            "p" => t.primary_key = Some(PrimaryKey { name: c.name, columns }),
            "u" if index_keys.get(&c.index).is_some_and(|ix| ix.include.as_ref().is_some_and(|i| !i.is_empty())) => {
                gaps.push(format!("unique constraint {}.{} has INCLUDE columns; it is read without them", t.name, c.name));
                t.uniques.push(Unique {
                    name: c.name,
                    columns,
                    nulls_not_distinct: c.nulls_not_distinct.unwrap_or(false),
                    deferrable: deferrable(c.deferrable, c.deferred),
                });
            }
            "u" => t.uniques.push(Unique {
                name: c.name,
                columns,
                nulls_not_distinct: c.nulls_not_distinct.unwrap_or(false),
                deferrable: deferrable(c.deferrable, c.deferred),
            }),
            "c" => {
                let def = c.def.trim_end_matches(" NOT VALID").trim();
                let expr = def.strip_prefix("CHECK ").unwrap_or(def);
                if expr.ends_with("NO INHERIT") {
                    gaps.push(format!("check {}.{} is NO INHERIT", t.name, c.name));
                }
                t.checks.push(Check { name: c.name, expr: strip_parens(expr.trim_end_matches(" NO INHERIT")).to_owned() });
            }
            "f" if !c.ref_local => {
                // matched by table name, it would point at a local table of the same name
                let ref_table = c.ref_table.unwrap_or_default();
                gaps.push(format!("foreign key {}.{} references {}.{ref_table}, outside the schema; it is left out", t.name, c.name, c.ref_schema.unwrap_or_default()));
            }
            "f" => {
                t.foreign_keys.push(ForeignKey {
                    name: c.name,
                    columns,
                    ref_table: c.ref_table.clone().unwrap_or_default(),
                    ref_columns: c.ref_columns.clone().unwrap_or_default(),
                    on_delete: on_action(&c.on_delete),
                    on_update: on_action(&c.on_update),
                    deferrable: deferrable(c.deferrable, c.deferred),
                });
            }
            _ => {
                let Some(ix) = index_keys.get(&c.index) else { continue };
                let ops = c.ops.unwrap_or_default();
                t.exclusions.push(Exclusion {
                    name: c.name,
                    method: ix.method.clone(),
                    elements: ix
                        .keys
                        .iter()
                        .zip(ops)
                        .map(|(k, operator)| ExclusionElement { key: index_key(k), operator })
                        .collect(),
                    where_: ix.where_.as_deref().map(|w| strip_parens(w).to_owned()),
                    deferrable: deferrable(c.deferrable, c.deferred),
                });
            }
        }
    }
    for ix in indexes.iter().filter(|i| !i.constraint) {
        let Some(t) = by_oid.get_mut(&ix.table) else { continue };
        t.indexes.push(Index {
            name: ix.name.clone(),
            unique: ix.unique,
            method: Some(ix.method.clone()).filter(|m| m != "btree"),
            keys: ix.keys.iter().map(index_key).collect(),
            include: ix.include.clone().unwrap_or_default(),
            where_: ix.where_.as_deref().map(|w| strip_parens(w).to_owned()),
            with: ix
                .options
                .iter()
                .flatten()
                .filter_map(|o| o.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
                .collect(),
            nulls_not_distinct: ix.nulls_not_distinct,
        });
    }
    for tr in triggers {
        let Some(t) = by_oid.get_mut(&tr.table) else { continue };
        if tr.constraint {
            gaps.push(format!("constraint trigger {}.{}: constraint triggers are not supported", t.name, tr.name));
            continue;
        }
        if tr.transition {
            gaps.push(format!("trigger {}.{} has REFERENCING transition tables; it is left out", t.name, tr.name));
            continue;
        }
        if tr.enabled == "D" {
            gaps.push(format!("trigger {}.{} is disabled; it is read as enabled", t.name, tr.name));
        }
        let bits = tr.bits;
        let timing = if bits & 64 != 0 {
            TriggerTiming::InsteadOf
        } else if bits & 2 != 0 {
            TriggerTiming::Before
        } else {
            TriggerTiming::After
        };
        let mut events = vec![];
        for (bit, e) in [(4, TriggerEvent::Insert), (8, TriggerEvent::Delete), (16, TriggerEvent::Update), (32, TriggerEvent::Truncate)] {
            if bits & bit != 0 {
                events.push(e);
            }
        }
        events.sort();
        let current = functions.iter().any(|f| f.name == tr.function);
        if !current {
            gaps.push(format!("trigger {}.{} calls {}.{}, a function outside the schema", t.name, tr.name, tr.function_schema, tr.function));
        }
        t.triggers.push(Trigger {
            name: tr.name,
            timing,
            events,
            update_of: tr.columns.unwrap_or_default(),
            for_each: if bits & 1 != 0 { ForEach::Row } else { ForEach::Statement },
            when: trigger_when(&tr.def),
            function: tr.function,
            body: None,
            args: trigger_args(&tr.args),
        });
    }
    for f in functions {
        if f.kind != "f" {
            let what = match f.kind.as_str() { "p" => "procedure", "a" => "aggregate", _ => "window function" };
            gaps.push(format!("{what} {}({}): only functions are supported", f.name, f.args));
            continue;
        }
        if !matches!(f.language.as_str(), "plpgsql" | "sql") {
            gaps.push(format!("function {}({}) is in language {}; it is pulled as written", f.name, f.args, f.language));
        }
        db.functions.push(Function {
            name: f.name,
            args: f.args,
            returns: f.returns.unwrap_or_default(),
            language: f.language,
            body: f.body.trim().to_owned(),
            volatility: match f.volatility.as_str() {
                "i" => Some("immutable".into()),
                "s" => Some("stable".into()),
                _ => None,
            },
            security_definer: f.security_definer,
        });
    }
    for o in others {
        let n = o.named;
        gaps.push(match o.kind.as_str() {
            "rule" => format!("rule {} on {}: rules are not supported: {}", n.name, n.table.unwrap_or_default(), orm_core::migrate::model::normalize_ws(&n.def.unwrap_or_default())),
            "policy" => format!("row-level security policy {} on {}: policies are not supported", n.name, n.table.unwrap_or_default()),
            "sequence" => format!("sequence {}: a sequence not owned by a column is not supported", n.name),
            kind => format!("{kind} {}: domains, composite and range types are not supported", n.name),
        });
    }
    db.tables = by_oid.into_values().collect();
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// SQLite
// ---------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct LiteTable {
    name: String,
    sql: Option<String>,
}

#[derive(Deserialize)]
struct LiteColumn {
    table: String,
    name: String,
    #[serde(rename = "type")]
    ty: String,
    notnull: i64,
    default: Option<String>,
    pk: i64,
    hidden: i64,
}

#[derive(Deserialize)]
struct LiteForeignKey {
    table: String,
    id: i64,
    ref_table: String,
    from: String,
    to: Option<String>,
    on_update: String,
    on_delete: String,
}

#[derive(Deserialize)]
struct LiteIndex {
    table: String,
    name: String,
    unique: i64,
    origin: String,
    sql: Option<String>,
    columns: Vec<LiteIndexColumn>,
}

#[derive(Deserialize)]
struct LiteIndexColumn {
    name: Option<String>,
    desc: i64,
}

const LITE_TABLES: &str = "SELECT coalesce(json_group_array(json_object('name', name, 'sql', sql)), '[]') FROM \
     (SELECT name, sql FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name <> 'orm_migrations' ORDER BY name)";
const LITE_COLUMNS: &str = "SELECT coalesce(json_group_array(json_object('table', m.name, 'name', c.name, 'type', c.type, \
     'notnull', c.\"notnull\", 'default', c.dflt_value, 'pk', c.pk, 'hidden', c.hidden)), '[]') \
     FROM sqlite_master m, pragma_table_xinfo(m.name) c \
     WHERE m.type = 'table' AND m.name NOT LIKE 'sqlite_%' AND m.name <> 'orm_migrations'";
const LITE_FOREIGN_KEYS: &str = "SELECT coalesce(json_group_array(json_object('table', m.name, 'id', f.id, 'ref_table', f.\"table\", \
     'from', f.\"from\", 'to', f.\"to\", 'on_update', f.on_update, 'on_delete', f.on_delete)), '[]') \
     FROM sqlite_master m, pragma_foreign_key_list(m.name) f \
     WHERE m.type = 'table' AND m.name NOT LIKE 'sqlite_%' AND m.name <> 'orm_migrations'";
const LITE_INDEXES: &str = "SELECT coalesce(json_group_array(json_object('table', m.name, 'name', i.name, 'unique', i.\"unique\", \
     'origin', i.origin, 'sql', (SELECT sql FROM sqlite_master x WHERE x.type = 'index' AND x.name = i.name), \
     'columns', json((SELECT json_group_array(json_object('name', c.name, 'desc', c.\"desc\"))  \
                 FROM pragma_index_xinfo(i.name) c WHERE c.key = 1)))), '[]') \
     FROM sqlite_master m, pragma_index_list(m.name) i \
     WHERE m.type = 'table' AND m.name NOT LIKE 'sqlite_%' AND m.name <> 'orm_migrations'";
const LITE_OTHERS: &str = "SELECT coalesce(json_group_array(json_object('kind', type, 'name', name, 'table', tbl_name)), '[]') \
     FROM sqlite_master WHERE type IN ('view', 'trigger')";

/// `text` split at top-level commas (outside parentheses and quotes).
pub fn split_top(text: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth, mut quote) = (vec![], String::new(), 0i32, None::<char>);
    for ch in text.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"' | '`' | '[') => quote = Some(if ch == '[' { ']' } else { ch }),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ',') if depth == 0 => {
                out.push(std::mem::take(&mut cur).trim().to_owned());
                continue;
            }
            _ => {}
        }
        cur.push(ch);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_owned());
    }
    out
}

/// The text between the first `(` and its matching `)`, and what follows it.
fn parens_body(text: &str) -> Option<(&str, &str)> {
    let start = text.find('(')?;
    let (mut depth, mut quote) = (0i32, None::<char>);
    for (i, ch) in text.char_indices().skip_while(|(i, _)| *i < start) {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(ch),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth -= 1;
                if depth == 0 {
                    return Some((&text[start + 1..i], &text[i + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

fn unquote(name: &str) -> String {
    let n = name.trim();
    match n.chars().next() {
        Some('"') | Some('`') | Some('[') if n.len() >= 2 => n[1..n.len() - 1].replace("\"\"", "\""),
        _ => n.to_owned(),
    }
}

/// The first word of `text` (a quoted name or a bare word) and the rest.
fn first_word(text: &str) -> (String, &str) {
    let t = text.trim_start();
    let close = match t.chars().next() {
        Some('"') => t[1..].find('"').map(|i| i + 2),
        Some('`') => t[1..].find('`').map(|i| i + 2),
        Some('[') => t.find(']').map(|i| i + 1),
        _ => None,
    };
    let end = close.unwrap_or_else(|| t.find(|c: char| c.is_whitespace() || c == '(').unwrap_or(t.len()));
    (unquote(&t[..end]), &t[end..])
}

fn names_in(list: &str) -> Vec<String> {
    split_top(list).iter().map(|n| unquote(n)).collect()
}

/// Named table constraints of a `CREATE TABLE` statement: `(kind, name, body)`.
fn lite_constraints(sql: &str) -> Vec<(String, String, String)> {
    let Some((body, _)) = parens_body(sql) else { return vec![] };
    let mut out = vec![];
    for item in split_top(body) {
        let upper = item.to_ascii_uppercase();
        if !upper.starts_with("CONSTRAINT") {
            continue;
        }
        let (_, rest) = first_word(&item);
        let (name, rest) = first_word(rest);
        let rest = rest.trim();
        let kind = rest.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
        out.push((kind, name, rest.to_owned()));
    }
    out
}

/// The byte offset of each top-level keyword `kw` (outside quotes and parentheses) in `text`.
fn top_keyword(text: &str, kw: &str) -> Vec<usize> {
    let upper = text.to_ascii_uppercase();
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let (mut out, mut depth, mut quote) = (vec![], 0i32, None::<char>);
    for (i, ch) in text.char_indices() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"' | '`') => quote = Some(ch),
            (None, '[') => quote = Some(']'),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            _ if depth == 0
                && upper[i..].starts_with(kw)
                && !word(text[..i].chars().next_back())
                && !word(text[i + kw.len()..].chars().next()) =>
            {
                out.push(i)
            }
            _ => {}
        }
    }
    out
}

/// A check written in a column definition or as an unnamed table constraint: its column
/// (none for a table check), its name if it has one, and its expression.
type LiteCheck = (Option<String>, Option<String>, String);

/// The clauses of a `CREATE TABLE` that are not named table constraints: unnamed table
/// checks, column checks, and inline `REFERENCES ... DEFERRABLE` of each column.
fn lite_inline(sql: &str) -> (Vec<LiteCheck>, BTreeMap<String, Deferrable>) {
    let Some((body, _)) = parens_body(sql) else { return (vec![], BTreeMap::new()) };
    let (mut checks, mut deferrable) = (vec![], BTreeMap::new());
    for item in split_top(body) {
        let upper = item.to_ascii_uppercase();
        let table_check = upper.starts_with("CHECK");
        if !table_check && ["CONSTRAINT", "PRIMARY", "UNIQUE", "FOREIGN"].iter().any(|k| upper.starts_with(k)) {
            continue;
        }
        let column = (!table_check).then(|| first_word(&item).0);
        for at in top_keyword(&item, "CHECK") {
            let Some((expr, _)) = parens_body(&item[at..]) else { continue };
            // `CONSTRAINT name CHECK (...)` on a column
            let before: Vec<&str> = item[..at].split_whitespace().collect();
            let name = match before.as_slice() {
                [.., c, n] if c.eq_ignore_ascii_case("CONSTRAINT") => Some(unquote(n)),
                _ => None,
            };
            checks.push((column.clone(), name, expr.trim().to_owned()));
        }
        if let (Some(c), Some(&at)) = (&column, top_keyword(&item, "REFERENCES").first()) {
            let tail = &upper[at..];
            if tail.contains("INITIALLY DEFERRED") {
                deferrable.insert(c.clone(), Deferrable::Deferred);
            } else if tail.contains("DEFERRABLE") && !tail.contains("NOT DEFERRABLE") {
                deferrable.insert(c.clone(), Deferrable::Immediate);
            }
        }
    }
    (checks, deferrable)
}

/// The columns of `CREATE TABLE` declared `PRIMARY KEY AUTOINCREMENT`.
fn lite_autoincrement(sql: &str) -> Vec<String> {
    let Some((body, _)) = parens_body(sql) else { return vec![] };
    split_top(body)
        .iter()
        .filter(|item| item.to_ascii_uppercase().contains("AUTOINCREMENT") && !item.to_ascii_uppercase().starts_with("CONSTRAINT"))
        .map(|item| first_word(item).0)
        .collect()
}

fn lite_action(a: &str) -> Option<OnDelete> {
    match a.to_ascii_uppercase().as_str() {
        "CASCADE" => Some(OnDelete::Cascade),
        "SET NULL" => Some(OnDelete::SetNull),
        "SET DEFAULT" => Some(OnDelete::SetDefault),
        "RESTRICT" => Some(OnDelete::Restrict),
        _ => None,
    }
}

/// An index's keys and `WHERE` from its `CREATE INDEX` statement.
fn lite_index(sql: &str, columns: &[LiteIndexColumn]) -> (Vec<IndexKey>, Option<String>) {
    let on = sql.to_ascii_uppercase().find(" ON ").map(|i| i + 4).unwrap_or(0);
    let Some((list, rest)) = parens_body(&sql[on..]) else { return (vec![], None) };
    let keys = split_top(list)
        .into_iter()
        .zip(columns.iter().map(Some).chain(std::iter::repeat(None)))
        .map(|(item, col)| {
            let mut text = item.trim().to_owned();
            let upper = text.to_ascii_uppercase();
            if upper.ends_with(" DESC") || upper.ends_with(" ASC") {
                text.truncate(text.rfind(' ').unwrap());
            }
            let collation = text.to_ascii_uppercase().rfind(" COLLATE ").map(|i| {
                let c = unquote(&text[i + 9..]);
                text.truncate(i);
                c
            });
            let column = col.and_then(|c| c.name.clone());
            IndexKey {
                expr: if column.is_some() { None } else { Some(strip_parens(&text).to_owned()) },
                column,
                collation,
                opclass: None,
                desc: col.is_some_and(|c| c.desc == 1),
                nulls: None,
            }
        })
        .collect();
    let rest = rest.trim();
    let where_ = (rest.len() > 6 && rest[..6].eq_ignore_ascii_case("WHERE ")).then(|| rest[6..].trim().to_owned());
    (keys, where_)
}

async fn sqlite(conn: &dyn Executor) -> Result<Introspection> {
    let tables: Vec<LiteTable> = json(conn, LITE_TABLES).await?;
    let columns: Vec<LiteColumn> = json(conn, LITE_COLUMNS).await?;
    let all_columns = &columns;
    let fks: Vec<LiteForeignKey> = json(conn, LITE_FOREIGN_KEYS).await?;
    let indexes: Vec<LiteIndex> = json(conn, LITE_INDEXES).await?;
    #[derive(Deserialize)]
    struct Other {
        kind: String,
        name: String,
        table: String,
    }
    let others: Vec<Other> = json(conn, LITE_OTHERS).await?;

    let mut out = Introspection::default();
    out.schema.dialect = Dialect::Sqlite;
    out.schema.version = 2;
    let gaps = &mut out.gaps;
    for lt in tables {
        let sql = lt.sql.unwrap_or_default();
        let named = lite_constraints(&sql);
        let autoinc = lite_autoincrement(&sql);
        let mut t = Table { name: lt.name.clone(), ..Default::default() };
        let mut pk: Vec<(i64, String)> = vec![];
        for c in columns.iter().filter(|c| c.table == lt.name) {
            if c.hidden != 0 {
                gaps.push(format!("column {}.{} is generated; generated columns are not supported", t.name, c.name));
                continue;
            }
            if c.pk > 0 {
                pk.push((c.pk, c.name.clone()));
            }
            t.columns.push(Column {
                name: c.name.clone(),
                ty: c.ty.clone(),
                nullable: c.notnull == 0 && c.pk == 0,
                default: c.default.as_deref().map(|d| strip_parens(d).to_owned()),
                identity: autoinc.contains(&c.name),
                comment: None,
            });
        }
        pk.sort();
        if !pk.is_empty() {
            let name = named.iter().find(|(k, _, _)| k == "PRIMARY").map(|(_, n, _)| n.clone());
            t.primary_key = Some(PrimaryKey {
                name: name.unwrap_or_else(|| orm_core::migrate::model::object_name(&[&t.name, "pkey"])),
                columns: pk.into_iter().map(|(_, c)| c).collect(),
            });
        }
        for (kind, name, body) in &named {
            if kind == "CHECK" {
                if let Some((expr, _)) = parens_body(body) {
                    t.checks.push(Check { name: name.clone(), expr: expr.trim().to_owned() });
                }
            }
        }
        let (inline_checks, inline_deferrable) = lite_inline(&sql);
        for (column, name, expr) in inline_checks {
            // Postgres' names for unnamed checks, with a number when one is taken
            let base = match &column {
                Some(c) => orm_core::migrate::model::object_name(&[&t.name, c, "check"]),
                None => orm_core::migrate::model::object_name(&[&t.name, "check"]),
            };
            let name = name.unwrap_or_else(|| {
                let mut n = base.clone();
                let mut i = 1;
                while t.checks.iter().any(|c| c.name == n) {
                    n = format!("{base}{i}");
                    i += 1;
                }
                n
            });
            t.checks.push(Check { name, expr });
        }
        let mut ids: Vec<i64> = fks.iter().filter(|f| f.table == lt.name).map(|f| f.id).collect();
        ids.dedup();
        for id in ids {
            let parts: Vec<&LiteForeignKey> = fks.iter().filter(|f| f.table == lt.name && f.id == id).collect();
            let columns: Vec<String> = parts.iter().map(|f| f.from.clone()).collect();
            let declared = named.iter().find(|(k, _, body)| {
                k == "FOREIGN" && parens_body(body).is_some_and(|(cols, _)| names_in(cols) == columns)
            });
            let body = declared.map(|(_, _, b)| b.to_ascii_uppercase()).unwrap_or_default();
            let inline = (declared.is_none() && columns.len() == 1).then(|| inline_deferrable.get(&columns[0]).copied()).flatten();
            // `REFERENCES parent` without columns means the parent's primary key
            let parent_pk = |i: usize| {
                let mut pk: Vec<(i64, &str)> =
                    all_columns.iter().filter(|c| c.table == parts[0].ref_table && c.pk > 0).map(|c| (c.pk, c.name.as_str())).collect();
                pk.sort();
                pk.get(i).map(|(_, n)| (*n).to_owned()).unwrap_or_default()
            };
            let mut parts_name = vec![t.name.as_str()];
            parts_name.extend(columns.iter().map(String::as_str));
            parts_name.push("fkey");
            t.foreign_keys.push(ForeignKey {
                name: declared.map(|(_, n, _)| n.clone()).unwrap_or_else(|| orm_core::migrate::model::object_name(&parts_name)),
                ref_table: parts[0].ref_table.clone(),
                ref_columns: parts.iter().enumerate().map(|(i, f)| f.to.clone().unwrap_or_else(|| parent_pk(i))).collect(),
                columns,
                on_delete: lite_action(&parts[0].on_delete),
                on_update: lite_action(&parts[0].on_update),
                deferrable: if body.contains("INITIALLY DEFERRED") {
                    Some(Deferrable::Deferred)
                } else if body.contains("DEFERRABLE") {
                    Some(Deferrable::Immediate)
                } else {
                    inline
                },
            });
        }
        for ix in indexes.iter().filter(|i| i.table == lt.name) {
            match ix.origin.as_str() {
                "pk" => {}
                "u" => {
                    let columns: Vec<String> = ix.columns.iter().filter_map(|c| c.name.clone()).collect();
                    let declared = named.iter().find(|(k, _, body)| {
                        k == "UNIQUE" && parens_body(body).is_some_and(|(cols, _)| names_in(cols) == columns)
                    });
                    let mut parts = vec![t.name.as_str()];
                    parts.extend(columns.iter().map(String::as_str));
                    parts.push("key");
                    t.uniques.push(Unique {
                        name: declared.map(|(_, n, _)| n.clone()).unwrap_or_else(|| orm_core::migrate::model::object_name(&parts)),
                        columns,
                        nulls_not_distinct: false,
                        deferrable: None,
                    });
                }
                _ => {
                    let (keys, where_) = lite_index(ix.sql.as_deref().unwrap_or(""), &ix.columns);
                    t.indexes.push(Index {
                        name: ix.name.clone(),
                        unique: ix.unique != 0,
                        method: None,
                        keys,
                        include: vec![],
                        where_,
                        with: vec![],
                        nulls_not_distinct: false,
                    });
                }
            }
        }
        out.schema.tables.push(t);
    }
    for o in others {
        gaps.push(match o.kind.as_str() {
            "view" => format!("view {}: views are not supported", o.name),
            _ => format!("trigger {} on {}: SQLite triggers are not read", o.name, o.table),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Drift
// ---------------------------------------------------------------------------------------

/// The shadow schema's name; the backend id makes it unique per session.
const SHADOW: &str = "orm_shadow_";

/// `db` read back from a database: its DDL is run in a shadow (a Postgres schema
/// in a transaction that is rolled back, or a new in-memory SQLite database) and the
/// shadow is introspected. Extensions are left out: they are per database.
pub async fn materialize(conn: &dyn Executor, db: &DbSchema) -> Result<Introspection> {
    match conn.dialect() {
        Dialect::Postgres => {
            let path = conn.query_text("SHOW search_path".into()).await?;
            let path = path.into_iter().next().and_then(|r| r.into_iter().next().flatten()).unwrap_or_default();
            let tx = conn.begin().await?;
            let run = async {
                let pid = tx.query_text("SELECT pg_backend_pid()".into()).await?;
                let pid = pid.into_iter().next().and_then(|r| r.into_iter().next().flatten()).unwrap_or_default();
                // live text (comments, defaults) runs as DDL here: a backslash must not end a literal
                tx.batch(format!(
                    "SET LOCAL standard_conforming_strings = on; CREATE SCHEMA {SHADOW}{pid}; \
                     SET LOCAL search_path TO {SHADOW}{pid}, {path}; SET LOCAL check_function_bodies = off"
                ))
                .await?;
                for sql in orm_core::migrate::create_statements(db) {
                    tx.batch(sql).await.map_err(|e| Error::Migration(format!("the snapshot's DDL fails in a shadow schema: {e}")))?;
                }
                introspect(&*tx).await
            }
            .await;
            let _ = crate::db::Transaction::rollback(&*tx).await;
            run
        }
        Dialect::Sqlite => {
            let shadow = crate::db::connect("sqlite://:memory:", 1).await?;
            let run = async {
                for sql in orm_core::migrate::sqlite::create_all(db, false) {
                    shadow.batch(sql).await?;
                }
                introspect(&*shadow).await
            }
            .await;
            shadow.close().await;
            run
        }
    }
}

/// The steps that turn the live database `live` into `target`, ignoring extensions
/// that only one of them lists.
pub fn differences(live: &DbSchema, target: &DbSchema) -> Vec<Step> {
    let mut a = live.clone();
    let mut b = target.clone();
    a.extensions.clear();
    b.extensions.clear();
    let ops = diff::diff(&a, &b, &Default::default());
    let mut steps: Vec<Step> = if live.dialect == Dialect::Sqlite && !ops.is_empty() {
        // SQLite rebuilds tables: one step with SQLite's SQL that names every change
        let sql = orm_core::migrate::sqlite::steps(&a, &b, &Default::default()).unwrap_or_default();
        let warnings: Vec<String> = ops.iter().filter_map(|op| op.warning()).collect();
        vec![Step {
            summary: ops.iter().map(orm_core::migrate::pg::summary).collect::<Vec<_>>().join("; "),
            sql: sql.into_iter().map(|s| s.sql).collect::<Vec<_>>().join("\n"),
            warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
        }]
    } else {
        ops.iter()
            .map(|op| Step { summary: orm_core::migrate::pg::summary(op), sql: orm_core::migrate::pg::render(op, false), warning: op.warning() })
            .collect()
    };
    for e in &target.extensions {
        if !live.extensions.iter().any(|x| x.name == e.name) {
            let op = Op::CreateExtension(e.clone());
            steps.insert(0, Step { summary: orm_core::migrate::pg::summary(&op), sql: orm_core::migrate::pg::render(&op, false), warning: None });
        }
    }
    steps
}

/// `live` with each expression-bearing object (column type and default, check, index,
/// exclusion, trigger) replaced by `target`'s where `live_again` (live re-created and
/// read back) equals `target`.
fn adopt(live: &DbSchema, live_again: &DbSchema, target: &DbSchema) -> DbSchema {
    fn same<'a, T: PartialEq>(a: &'a [T], b: &'a [T], name: impl Fn(&T) -> &str, n: &str) -> Option<&'a T> {
        let x = a.iter().find(|x| name(x) == n)?;
        let y = b.iter().find(|y| name(y) == n)?;
        (x == y).then_some(y)
    }
    let mut out = live.clone();
    for t in &mut out.tables {
        let (Some(a), Some(b)) = (live_again.tables.iter().find(|x| x.name == t.name), target.tables.iter().find(|x| x.name == t.name)) else {
            continue;
        };
        for c in &mut t.columns {
            if let Some(y) = same(&a.columns, &b.columns, |c| c.name.as_str(), &c.name) {
                *c = y.clone();
            }
        }
        for c in &mut t.checks {
            if let Some(y) = same(&a.checks, &b.checks, |c| c.name.as_str(), &c.name) {
                *c = y.clone();
            }
        }
        for c in &mut t.indexes {
            if let Some(y) = same(&a.indexes, &b.indexes, |c| c.name.as_str(), &c.name) {
                *c = y.clone();
            }
        }
        for c in &mut t.exclusions {
            if let Some(y) = same(&a.exclusions, &b.exclusions, |c| c.name.as_str(), &c.name) {
                *c = y.clone();
            }
        }
        for c in &mut t.triggers {
            if let Some(y) = same(&a.triggers, &b.triggers, |c| c.name.as_str(), &c.name) {
                *c = y.clone();
            }
        }
    }
    out
}

/// What [`pull`] wrote and checked.
#[derive(Debug, Clone)]
pub struct Pull {
    /// The schema file.
    pub schema: String,
    /// What the schema leaves out.
    pub gaps: Vec<String>,
    /// Steps a migration from the schema would still run on the database; empty when
    /// the schema reproduces it.
    pub steps: Vec<Step>,
}

/// The live database as a schema file, checked by creating it again in a shadow.
pub async fn pull(conn: &dyn Executor) -> Result<Pull> {
    let live = introspect(conn).await?;
    let pulled = orm_core::migrate::pull::pull(&live.schema, &live.gaps);
    let (_, compiled) = orm_core::dsl::compile(&pulled.schema, None)
        .and_then(orm_core::dsl::check)
        .map_err(|e| Error::Migration(format!("the pulled schema doesn't compile (a bug in pull): {e}")))?;
    let snapshot = orm_core::migrate::snapshot(&compiled).map_err(Error::Migration)?;
    let (mut gaps, schema) = (pulled.gaps, pulled.schema);
    // the check needs CREATE and enough locks; the schema file does not
    let steps = match compare(conn, &snapshot).await {
        Ok((steps, _)) => steps,
        Err(e) => {
            gaps.push(format!("the schema was not checked against the database: {e}"));
            vec![]
        }
    };
    Ok(Pull { schema, gaps, steps })
}

/// What [`drift`] found.
#[derive(Debug, Clone)]
pub struct Drift {
    /// The migration whose snapshot was compared, if any.
    pub migration: Option<String>,
    /// Steps that bring the database to the snapshot; empty when they match.
    pub steps: Vec<Step>,
    /// Live objects that drift can't compare.
    pub gaps: Vec<String>,
}

/// Compares the live database with the snapshot of the newest migration in `dir`.
pub async fn drift(conn: &dyn Executor, dir: &Path) -> Result<Drift> {
    let list = orm_core::migrate::files::list(dir).map_err(Error::Migration)?;
    let snapshot = orm_core::migrate::files::latest_snapshot(dir).map_err(Error::Migration)?;
    if list.last().is_some() && snapshot.dialect != conn.dialect() {
        return Err(Error::Migration(format!("the migrations target {}, the connection uses {}", snapshot.dialect.name(), conn.dialect().name())));
    }
    compare(conn, &snapshot).await.map(|(steps, gaps)| Drift { migration: list.last().map(|m| m.name.clone()), steps, gaps })
}

/// The live database against `target`: the steps between them and the live gaps.
pub async fn compare(conn: &dyn Executor, target: &DbSchema) -> Result<(Vec<Step>, Vec<String>)> {
    let live = introspect(conn).await?;
    let mut target = target.clone();
    target.dialect = conn.dialect();
    let shadow = materialize(conn, &target).await?.schema;
    let shadow = DbSchema { extensions: target.extensions.clone(), ..shadow };
    // Postgres' deparsed text is not always stable when it is parsed again, so the live
    // schema is also re-created; an object both shadows agree on is the same.
    let live_schema = match materialize(conn, &live.schema).await {
        Ok(again) => adopt(&live.schema, &again.schema, &shadow),
        Err(_) => live.schema,
    };
    Ok((differences(&live_schema, &shadow), live.gaps))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parentheses() {
        assert_eq!(strip_parens("((a >= 0))"), "(a >= 0)");
        assert_eq!(strip_parens("(a) AND (b)"), "(a) AND (b)");
        assert_eq!(strip_parens("('(')"), "'('");
        assert_eq!(split_top("a, f(b, c), 'x,y'"), vec!["a", "f(b, c)", "'x,y'"]);
    }

    #[test]
    fn trigger_parts() {
        let def = "CREATE TRIGGER t BEFORE UPDATE ON posts FOR EACH ROW WHEN ((old.title IS DISTINCT FROM new.title)) EXECUTE FUNCTION f('a', 'b')";
        assert_eq!(trigger_when(def).as_deref(), Some("(old.title IS DISTINCT FROM new.title)"));
        assert_eq!(trigger_args("610062636400"), vec!["a", "bcd"]);
        assert!(trigger_args("").is_empty());
    }

    #[test]
    fn sqlite_ddl() {
        let sql = "CREATE TABLE \"t\" (\n    \"id\" INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,\n    CONSTRAINT \"t_a_key\" UNIQUE (\"a\"),\n    CONSTRAINT \"t_c\" CHECK (a IN (1, 2))\n)";
        let named = lite_constraints(sql);
        assert_eq!(named[0].0, "UNIQUE");
        assert_eq!(named[1].1, "t_c");
        assert_eq!(lite_autoincrement(sql), vec!["id"]);
        let (keys, w) = lite_index("CREATE INDEX \"i\" ON \"t\" ((lower(a)) DESC, \"b\" COLLATE \"NOCASE\") WHERE b > 0", &[]);
        assert_eq!(keys[0].expr.as_deref(), Some("lower(a)"));
        assert_eq!(keys[1].collation.as_deref(), Some("NOCASE"));
        assert_eq!(w.as_deref(), Some("b > 0"));
    }
}
