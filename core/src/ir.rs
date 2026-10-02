//! The ORM intermediate representation exchanged with language frontends.
//!
//! Frontends describe the schema once (at connect time) and then send one query IR
//! document per operation. The IR speaks in models, fields and relation paths; it never
//! mentions tables, joins or a database driver. Literal values travel out of band as a positional
//! parameter list (`Param { i }`) so the document itself stays plain JSON.
//!
//! The schema half also carries what migrations need: indexes, constraints, triggers,
//! functions and database extensions. None of it affects query planning except the
//! per-column `read_sql` / `write_sql` templates extension types use.

use serde::{Deserialize, Serialize};

pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}

pub(crate) fn yes() -> bool {
    true
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Deserialize, Serialize, Debug, Default)]
pub struct SchemaIr {
    pub models: Vec<ModelIr>,
    /// Database extensions the schema needs (`CREATE EXTENSION`), on top of the ones
    /// required implicitly by column types, index methods and operator classes.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub extensions: Vec<ExtensionIr>,
    /// Stand-alone SQL functions (trigger functions declared next to their trigger are
    /// collected from the models instead).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub functions: Vec<FunctionIr>,
    /// Extensions the schema knows about (imported extension files) without
    /// requiring them: they are created only once something they provide is used.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub catalog: Vec<ExtensionIr>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct ModelIr {
    pub name: String,
    pub table: String,
    pub fields: Vec<FieldIr>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub relations: Vec<RelationIr>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub indexes: Vec<IndexIr>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub constraints: Vec<ConstraintIr>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub triggers: Vec<TriggerIr>,
    /// Previous table name, so the migration generator emits a rename instead of a
    /// drop + create.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub renamed_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub comment: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct FieldIr {
    pub name: String,
    pub column: String,
    #[serde(rename = "type")]
    pub ty: ColType,
    #[serde(skip_serializing_if = "crate::ir::is_false", default)]
    pub nullable: bool,
    #[serde(skip_serializing_if = "crate::ir::is_false", default)]
    pub primary_key: bool,
    #[serde(skip_serializing_if = "crate::ir::is_false", default)]
    pub auto_increment: bool,
    #[serde(skip_serializing_if = "crate::ir::is_false", default)]
    pub unique: bool,
    #[serde(skip_serializing_if = "crate::ir::is_false", default)]
    pub index: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub max_length: Option<u32>,
    /// Literal server-side default (number, bool or string).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub default: Option<serde_json::Value>,
    /// Server-side `DEFAULT now()`.
    #[serde(skip_serializing_if = "crate::ir::is_false", default)]
    pub default_now: bool,
    /// Server-side default as a raw SQL expression (`gen_random_uuid()`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub default_sql: Option<String>,
    /// SQL type overriding the one derived from `type` (`citext`, `vector(3)`). `type`
    /// then only says how values convert to and from the frontend language.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub db_type: Option<String>,
    /// SQL wrapped around the column when it is read, `{}` standing for the column
    /// (`CAST({} AS text)`), for types the driver can't decode directly.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub read_sql: Option<String>,
    /// SQL wrapped around every bound value written to or compared with the column,
    /// `{}` standing for the parameter (`CAST({} AS citext)`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub write_sql: Option<String>,
    /// Column-level `CHECK` expression (raw SQL).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub check: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub renamed_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub comment: Option<String>,
    /// Extensions this column's type needs.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub requires: Vec<String>,
    /// Per-language type hints for generated code (`{"python": "list[float]"}`).
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty", default)]
    pub hints: std::collections::BTreeMap<String, String>,
}

impl FieldIr {
    /// A plain column: no key, default or SQL templates.
    pub fn plain(name: &str, ty: ColType) -> FieldIr {
        FieldIr {
            name: name.to_owned(),
            column: name.to_owned(),
            ty,
            nullable: true,
            primary_key: false,
            auto_increment: false,
            unique: false,
            index: false,
            max_length: None,
            default: None,
            default_now: false,
            default_sql: None,
            db_type: None,
            read_sql: None,
            write_sql: None,
            check: None,
            renamed_from: None,
            comment: None,
            requires: vec![],
            hints: Default::default(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Deserialize, Serialize, Debug)]
pub struct RelationIr {
    pub name: String,
    pub kind: RelKind,
    pub target: String,
    pub from: String,
    pub to: String,
    /// True on the side that owns the foreign key constraint (`from` is the FK column).
    #[serde(skip_serializing_if = "crate::ir::is_false", default)]
    pub foreign_key: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub on_delete: Option<OnDelete>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub on_update: Option<OnDelete>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
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
#[derive(Deserialize, Serialize, Debug)]
pub struct IndexColumnIr {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub opclass: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub collation: Option<String>,
    #[serde(skip_serializing_if = "crate::ir::is_false", default)]
    pub desc: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub nulls: Option<Nulls>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct IndexIr {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
    pub columns: Vec<IndexColumnIr>,
    #[serde(skip_serializing_if = "crate::ir::is_false", default)]
    pub unique: bool,
    /// Access method (`btree` when absent): `gin`, `gist`, `brin`, `hnsw`, ...
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub method: Option<String>,
    /// Partial index predicate (raw SQL).
    #[serde(skip_serializing_if = "Option::is_none", default, rename = "where")]
    pub where_: Option<String>,
    /// Non-key fields stored in the index (`INCLUDE`).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub include: Vec<String>,
    /// Storage parameters (`WITH (m = 16)`), values rendered as given.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub with: Vec<(String, String)>,
    #[serde(skip_serializing_if = "crate::ir::is_false", default)]
    pub nulls_not_distinct: bool,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub requires: Vec<String>,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConstraintIr {
    Unique {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        name: Option<String>,
        fields: Vec<String>,
        #[serde(skip_serializing_if = "crate::ir::is_false", default)]
        nulls_not_distinct: bool,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        deferrable: Option<Deferrable>,
    },
    Check {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        name: Option<String>,
        expr: String,
    },
    /// `EXCLUDE USING <method> (<element> WITH <operator>, ...)`.
    Exclude {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        method: Option<String>,
        elements: Vec<ExcludeElementIr>,
        #[serde(skip_serializing_if = "Option::is_none", default, rename = "where")]
        where_: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        deferrable: Option<Deferrable>,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        requires: Vec<String>,
    },
}

#[derive(Deserialize, Serialize, Debug)]
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
#[derive(Deserialize, Serialize, Debug)]
pub struct TriggerIr {
    pub name: String,
    pub timing: TriggerTiming,
    pub events: Vec<TriggerEvent>,
    /// `UPDATE OF <fields>`.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub update_of: Vec<String>,
    #[serde(default)]
    pub for_each: ForEach,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub when: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub function: Option<String>,
    /// Literal arguments passed to the function (`TG_ARGV`).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub language: Option<String>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct FunctionIr {
    pub name: String,
    /// Argument list as SQL (`a integer, b text`); empty for trigger functions.
    #[serde(default)]
    pub args: String,
    pub returns: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub language: Option<String>,
    pub body: String,
    /// `immutable`, `stable` or `volatile`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub volatility: Option<String>,
    #[serde(skip_serializing_if = "crate::ir::is_false", default)]
    pub security_definer: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ExtensionIr {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub schema: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub version: Option<String>,
    /// What the extension adds, so references to it pull it in automatically. Known
    /// extensions (`pg_trgm`, `vector`, ...) don't need this; see `ext.rs`.
    #[serde(skip_serializing_if = "Provides::is_empty", default)]
    pub provides: Provides,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct Provides {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub types: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub index_methods: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub opclasses: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
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

#[derive(Deserialize, Debug, Clone)]
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
    /// `EXCLUDED.<field>`: the row proposed for insertion, in an upsert's `DO UPDATE`.
    Excluded { name: String },
    /// A SQL function. Aggregates (`count`, `sum`, ...) whose arguments go through a
    /// to-many relation are computed per row in a correlated subquery; `count` with no
    /// argument (or a relation path in `rel`) counts rows.
    Func {
        name: String,
        #[serde(default)]
        args: Vec<Expr>,
        /// `count(User.posts)`: the relation whose rows are counted.
        #[serde(default)]
        rel: Option<Vec<String>>,
        #[serde(default)]
        distinct: bool,
    },
    /// `<item> [NOT] IN (SELECT <one column> ...)`.
    InSelect { item: Box<Expr>, select: Box<Select>, #[serde(default)] neg: bool },
    /// An integer written into the SQL (window function offsets, `ntile` buckets).
    Int { value: i64 },
    /// A field of an enclosing query's root model (or CTE), `depth` queries up:
    /// `outer(User.id)` in a subquery of a `User` query.
    Outer { depth: usize, name: String },
    /// A column of a CTE (`cte.c.<name>`), in a query reading that CTE, or in the
    /// recursive part of the CTE itself.
    CteCol { cte: String, name: String },
    /// `EXISTS (<select>)`.
    Exists { select: Box<Select> },
    /// `(<select>)` returning one column and at most one row.
    Subquery { select: Box<Select> },
    /// `<func> OVER (PARTITION BY ... ORDER BY ... <frame>)`.
    Window {
        func: Box<Expr>,
        #[serde(default)]
        partition_by: Vec<Expr>,
        #[serde(default)]
        order_by: Vec<Order>,
        #[serde(default)]
        frame: Option<Frame>,
    },
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    Rows,
    Range,
}

/// A window frame. Bounds: `None` is unbounded, `0` the current row, negative values
/// rows (or values) preceding, positive ones following.
#[derive(Deserialize, Clone, Copy, Debug)]
pub struct Frame {
    pub kind: FrameKind,
    #[serde(default)]
    pub start: Option<i64>,
    #[serde(default)]
    pub end: Option<i64>,
}

/// `WITH <name> AS (<query> [UNION [ALL] <recursive>])`.
#[derive(Deserialize, Debug, Clone)]
pub struct Cte {
    pub name: String,
    pub query: Select,
    /// The recursive part: may read the CTE's own columns (`CteCol`), which joins it.
    #[serde(default)]
    pub recursive: Option<Select>,
    /// `UNION` instead of `UNION ALL` between the two parts.
    #[serde(default)]
    pub distinct: bool,
    /// `MATERIALIZED` / `NOT MATERIALIZED`.
    #[serde(default)]
    pub materialized: Option<bool>,
}

/// A relation loaded with a separate `IN (...)` query once the parent rows are known.
/// `query` (on the relation's target model) filters, orders and slices the related rows
/// (a slice applies per parent row) and nests further prefetches.
#[derive(Deserialize, Debug, Clone)]
pub struct Prefetch {
    pub relation: String,
    /// Attribute the rows are stored under; the relation's name when absent.
    #[serde(default)]
    pub attr: Option<String>,
    #[serde(flatten)]
    pub query: Select,
}

/// One entry of `select(...)`: every column of the root model, or an expression.
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum SelectItem {
    Model,
    /// `name` is the column's name when the select is a CTE.
    Expr {
        expr: Expr,
        #[serde(default)]
        name: Option<String>,
    },
}

#[derive(Deserialize, Debug, Clone)]
pub struct Order {
    pub expr: Expr,
    #[serde(default)]
    pub desc: bool,
}

/// One entry per `filter()` call. Entries are AND-ed, but each is planned on its own:
/// conditions inside one entry that go through the same to-many relation must hold for
/// the same related row, while separate entries are independent (Django semantics).
#[derive(Deserialize, Debug, Clone)]
pub struct Select {
    /// The root model, or a CTE of this statement when the query reads a CTE that has no
    /// model.
    pub model: String,
    /// CTEs this statement declares (`WITH ...`). Subqueries don't declare their own:
    /// frontends hoist them to the statement.
    #[serde(default)]
    pub with: Vec<Cte>,
    /// Read the root model's rows from this CTE instead of its table.
    #[serde(default)]
    pub from: Option<String>,
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
    /// Relations of the root model loaded with one extra `IN (...)` query each, inside
    /// the same frontend call.
    #[serde(default)]
    pub prefetch: Vec<Prefetch>,
    /// Row lock on the root model's rows (`FOR UPDATE` / `FOR SHARE`). Select only.
    #[serde(default)]
    pub lock: Option<Lock>,
    /// `select(...)`: the columns to return instead of the root model's fields.
    #[serde(default)]
    pub columns: Option<Vec<SelectItem>>,
    #[serde(default)]
    pub group_by: Vec<Expr>,
    /// Conditions on groups, AND-ed.
    #[serde(default)]
    pub having: Vec<Expr>,
    /// `SELECT DISTINCT`; with `distinct_on`, `DISTINCT ON (...)`.
    #[serde(default)]
    pub distinct: bool,
    #[serde(default)]
    pub distinct_on: Vec<Expr>,
}

#[derive(Deserialize, Debug, Clone, Copy)]
pub struct Lock {
    /// `FOR UPDATE` when true, `FOR SHARE` otherwise.
    #[serde(default = "crate::ir::yes")]
    pub exclusive: bool,
    /// Fail at once instead of waiting for rows locked by others (`NOWAIT`).
    #[serde(default)]
    pub nowait: bool,
    /// Leave out rows locked by others (`SKIP LOCKED`).
    #[serde(default)]
    pub skip_locked: bool,
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
    pub with: Vec<Cte>,
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
    pub with: Vec<Cte>,
    #[serde(default)]
    pub filters: Vec<Expr>,
    /// Return the deleted rows (every column) instead of a row count.
    #[serde(default)]
    pub returning: bool,
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

impl Provides {
    pub fn is_empty(&self) -> bool {
        self.types.is_empty() && self.index_methods.is_empty() && self.opclasses.is_empty() && self.functions.is_empty()
    }
}
