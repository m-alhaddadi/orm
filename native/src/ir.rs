//! The ORM intermediate representation exchanged with language frontends.
//!
//! Frontends describe the schema once (at connect time) and then send one query IR
//! document per operation. The IR speaks in models, fields and relation paths; it never
//! mentions tables, joins or SeaORM. Literal values travel out of band as a positional
//! parameter list (`Param { i }`) so the document itself stays plain JSON.

use serde::Deserialize;

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
}

#[derive(Deserialize, Debug)]
pub struct SchemaIr {
    pub models: Vec<ModelIr>,
}

#[derive(Deserialize, Debug)]
pub struct ModelIr {
    pub name: String,
    pub table: String,
    pub fields: Vec<FieldIr>,
    #[serde(default)]
    pub relations: Vec<RelationIr>,
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
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelKind {
    /// At most one target row per source row (belongs-to / has-one).
    One,
    /// Any number of target rows per source row (has-many).
    Many,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnDelete {
    Cascade,
    SetNull,
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
