//! The ORM intermediate representation exchanged with language frontends.
//!
//! Frontends describe the schema once (at connect time) and then send one query IR
//! document per operation. The IR speaks in models, fields and relation paths; it never
//! mentions tables, joins or SeaORM. Literal values travel out of band as a positional
//! parameter list (`Param { i }`) so the document itself stays plain JSON.
//!
//! The schema half also carries what migrations need: indexes, constraints, triggers,
//! functions and database extensions. None of it affects query planning except the
//! per-column `read_sql` / `write_sql` templates extension types use.

use serde::{Deserialize, Serialize};

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ColType {
    BigInt,
    Int,
    Float,
    Bool,
    String,
    Text,
    DateTime,
    Date,
    Uuid,
    /// `jsonb`; values are JSON-compatible Python objects.
    Json,
}

#[derive(Deserialize, Debug, Default)]
pub struct SchemaIr {
    pub models: Vec<ModelIr>,
    /// Database extensions the schema needs (`CREATE EXTENSION`), on top of the ones
    /// required implicitly by column types, index methods and operator classes.
    #[serde(default)]
    pub extensions: Vec<ExtensionIr>,
    /// Stand-alone SQL functions (trigger functions declared next to their trigger are
    /// collected from the models instead).
    #[serde(default)]
    pub functions: Vec<FunctionIr>,
}

#[derive(Deserialize, Debug)]
pub struct ModelIr {
    pub name: String,
    pub table: String,
    pub fields: Vec<FieldIr>,
    #[serde(default)]
    pub relations: Vec<RelationIr>,
    #[serde(default)]
    pub indexes: Vec<IndexIr>,
    #[serde(default)]
    pub constraints: Vec<ConstraintIr>,
    #[serde(default)]
    pub triggers: Vec<TriggerIr>,
    /// Previous table name, so the migration generator emits a rename instead of a
    /// drop + create.
    #[serde(default)]
    pub renamed_from: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct FieldIr {
    pub name: String,
    pub column: String,
    #[serde(rename = "type")]
    pub ty: ColType,
    #[serde(default)]
    pub nullable: bool,
    #[serde(default)]
    pub primary_key: bool,
    #[serde(default)]
    pub auto_increment: bool,
    #[serde(default)]
    pub unique: bool,
    #[serde(default)]
    pub index: bool,
    #[serde(default)]
    pub max_length: Option<u32>,
    /// Literal server-side default (number, bool or string).
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// Server-side `DEFAULT now()`.
    #[serde(default)]
    pub default_now: bool,
    /// Server-side default as a raw SQL expression (`gen_random_uuid()`).
    #[serde(default)]
    pub default_sql: Option<String>,
    /// SQL type overriding the one derived from `type` (`citext`, `vector(3)`). `type`
    /// then only says how values convert to and from the frontend language.
    #[serde(default)]
    pub db_type: Option<String>,
    /// SQL wrapped around the column when it is read, `{}` standing for the column
    /// (`CAST({} AS text)`), for types the driver can't decode directly.
    #[serde(default)]
    pub read_sql: Option<String>,
    /// SQL wrapped around every bound value written to or compared with the column,
    /// `{}` standing for the parameter (`CAST({} AS citext)`).
    #[serde(default)]
    pub write_sql: Option<String>,
    /// Column-level `CHECK` expression (raw SQL).
    #[serde(default)]
    pub check: Option<String>,
    #[serde(default)]
    pub renamed_from: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    /// Extensions this column's type needs.
    #[serde(default)]
    pub requires: Vec<String>,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelKind {
    /// At most one target row per source row (belongs-to / has-one).
    One,
    /// Any number of target rows per source row (has-many).
    Many,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnDelete {
    Cascade,
    SetNull,
    SetDefault,
    Restrict,
    NoAction,
}

/// `source.from == target.to` links a source row to its related target rows.
#[derive(Deserialize, Debug)]
pub struct RelationIr {
    pub name: String,
    pub kind: RelKind,
    pub target: String,
    pub from: String,
    pub to: String,
    /// True on the side that owns the foreign key constraint (`from` is the FK column).
    #[serde(default)]
    pub foreign_key: bool,
    #[serde(default)]
    pub on_delete: Option<OnDelete>,
    #[serde(default)]
    pub on_update: Option<OnDelete>,
    #[serde(default)]
    pub deferrable: Option<Deferrable>,
}

// ---------------------------------------------------------------------------------------
// Schema objects for migrations
// ---------------------------------------------------------------------------------------

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Deferrable {
    /// `DEFERRABLE INITIALLY IMMEDIATE`
    Immediate,
    /// `DEFERRABLE INITIALLY DEFERRED`
    Deferred,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Nulls {
    First,
    Last,
}

/// One key of an index: a field of the model or a raw SQL expression.
#[derive(Deserialize, Debug)]
pub struct IndexColumnIr {
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub expr: Option<String>,
    #[serde(default)]
    pub opclass: Option<String>,
    #[serde(default)]
    pub collation: Option<String>,
    #[serde(default)]
    pub desc: bool,
    #[serde(default)]
    pub nulls: Option<Nulls>,
}

#[derive(Deserialize, Debug)]
pub struct IndexIr {
    #[serde(default)]
    pub name: Option<String>,
    pub columns: Vec<IndexColumnIr>,
    #[serde(default)]
    pub unique: bool,
    /// Access method (`btree` when absent): `gin`, `gist`, `brin`, `hnsw`, ...
    #[serde(default)]
    pub method: Option<String>,
    /// Partial index predicate (raw SQL).
    #[serde(default, rename = "where")]
    pub where_: Option<String>,
    /// Non-key fields stored in the index (`INCLUDE`).
    #[serde(default)]
    pub include: Vec<String>,
    /// Storage parameters (`WITH (m = 16)`), values rendered as given.
    #[serde(default)]
    pub with: Vec<(String, String)>,
    #[serde(default)]
    pub nulls_not_distinct: bool,
    #[serde(default)]
    pub requires: Vec<String>,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConstraintIr {
    Unique {
        #[serde(default)]
        name: Option<String>,
        fields: Vec<String>,
        #[serde(default)]
        nulls_not_distinct: bool,
        #[serde(default)]
        deferrable: Option<Deferrable>,
    },
    Check {
        #[serde(default)]
        name: Option<String>,
        expr: String,
    },
    /// `EXCLUDE USING <method> (<element> WITH <operator>, ...)`.
    Exclude {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        method: Option<String>,
        elements: Vec<ExcludeElementIr>,
        #[serde(default, rename = "where")]
        where_: Option<String>,
        #[serde(default)]
        deferrable: Option<Deferrable>,
        #[serde(default)]
        requires: Vec<String>,
    },
}

#[derive(Deserialize, Debug)]
pub struct ExcludeElementIr {
    #[serde(flatten)]
    pub column: IndexColumnIr,
    pub operator: String,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TriggerEvent {
    Insert,
    Update,
    Delete,
    Truncate,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerTiming {
    Before,
    After,
    InsteadOf,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ForEach {
    #[default]
    Row,
    Statement,
}

/// A trigger runs either a schema-level function (`function`) or its own body
/// (`body`), from which a function named `<table>_<trigger>` is generated.
#[derive(Deserialize, Debug)]
pub struct TriggerIr {
    pub name: String,
    pub timing: TriggerTiming,
    pub events: Vec<TriggerEvent>,
    /// `UPDATE OF <fields>`.
    #[serde(default)]
    pub update_of: Vec<String>,
    #[serde(default)]
    pub for_each: ForEach,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub function: Option<String>,
    /// Literal arguments passed to the function (`TG_ARGV`).
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct FunctionIr {
    pub name: String,
    /// Argument list as SQL (`a integer, b text`); empty for trigger functions.
    #[serde(default)]
    pub args: String,
    pub returns: String,
    #[serde(default)]
    pub language: Option<String>,
    pub body: String,
    /// `immutable`, `stable` or `volatile`.
    #[serde(default)]
    pub volatility: Option<String>,
    #[serde(default)]
    pub security_definer: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ExtensionIr {
    pub name: String,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    /// What the extension adds, so references to it pull it in automatically. Known
    /// extensions (`pg_trgm`, `vector`, ...) don't need this; see `ext.rs`.
    #[serde(default)]
    pub provides: Provides,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Provides {
    #[serde(default)]
    pub types: Vec<String>,
    #[serde(default)]
    pub index_methods: Vec<String>,
    #[serde(default)]
    pub opclasses: Vec<String>,
    #[serde(default)]
    pub functions: Vec<String>,
}

// ---------------------------------------------------------------------------------------
// Query IR
// ---------------------------------------------------------------------------------------

#[derive(Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Expr {
    /// A field reached from the query's root model through `path` (relation names).
    Col { path: Vec<String>, name: String },
    Param { i: usize },
    Const { value: bool },
    Cmp { op: CmpOp, l: Box<Expr>, r: Box<Expr> },
    Arith { op: ArithOp, l: Box<Expr>, r: Box<Expr> },
    And { items: Vec<Expr> },
    Or { items: Vec<Expr> },
    Not { item: Box<Expr> },
    In { item: Box<Expr>, values: Vec<Expr>, #[serde(default)] neg: bool },
    IsNull { item: Box<Expr>, #[serde(default)] neg: bool },
    Like { item: Box<Expr>, pattern: Box<Expr>, #[serde(default)] ci: bool, #[serde(default)] neg: bool },
}

#[derive(Deserialize, Debug)]
pub struct Order {
    pub expr: Expr,
    #[serde(default)]
    pub desc: bool,
}

/// One entry per `filter()` call. Entries are AND-ed, but each is planned on its own:
/// conditions inside one entry that go through the same to-many relation must hold for
/// the same related row, while separate entries are independent (Django semantics).
#[derive(Deserialize, Debug)]
pub struct Select {
    pub model: String,
    #[serde(default)]
    pub filters: Vec<Expr>,
    #[serde(default)]
    pub order: Vec<Order>,
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default)]
    pub offset: Option<u64>,
    /// To-one relation paths loaded with LEFT JOINs in the same statement.
    #[serde(default)]
    pub select_related: Vec<Vec<String>>,
    /// To-many relations of the root model loaded with one extra `IN (...)` query each,
    /// inside the same frontend call.
    #[serde(default)]
    pub prefetch: Vec<String>,
}

#[derive(Deserialize, Debug)]
pub struct Assignment {
    pub field: String,
    pub value: Expr,
}

#[derive(Deserialize, Debug)]
pub struct Update {
    pub model: String,
    #[serde(default)]
    pub filters: Vec<Expr>,
    pub set: Vec<Assignment>,
    /// Return the updated rows (every column) instead of a row count.
    #[serde(default)]
    pub returning: bool,
}

#[derive(Deserialize, Debug)]
pub struct Delete {
    pub model: String,
    #[serde(default)]
    pub filters: Vec<Expr>,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    Select(Select),
    Count(Select),
    Exists(Select),
    Update(Update),
    Delete(Delete),
}
