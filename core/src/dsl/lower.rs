//! Syntax tree -> schema IR.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::syntax::{err, Args, Attr, EnumDecl, Item, Member, ModelDecl, Pos, Props, Result, TypeRef, Value};
use crate::ext::{ExtensionDef, TypeDef};
use crate::ir::{
    ColType, ConstraintIr, Deferrable, EnumIr, EnumStorage, EnumValueIr, ExcludeElementIr, ExtensionIr, FieldIr,
    ForEach, FunctionIr, IndexColumnIr, IndexIr, ModelIr, Nulls, OnDelete, RelKind, RelationIr, SchemaIr,
    ThroughIr, TriggerEvent, TriggerIr, TriggerTiming,
};
use crate::migrate::pg::ident;

/// Reads the named arguments of one attribute, rejecting unknown or repeated ones.
struct Named<'a> {
    what: String,
    pos: Pos,
    args: &'a Args,
    used: HashSet<&'a str>,
}

impl<'a> Named<'a> {
    fn new(what: impl Into<String>, args: &'a Args) -> Self {
        Named { what: what.into(), pos: args.pos, args, used: HashSet::new() }
    }

    fn get(&mut self, key: &'a str) -> Option<(Pos, &'a Value)> {
        self.args.named.iter().find(|(k, _, _)| k == key).map(|(k, p, v)| {
            self.used.insert(k.as_str());
            (*p, v)
        })
    }

    fn str(&mut self, key: &'a str) -> Result<Option<String>> {
        match self.get(key) {
            None => Ok(None),
            Some((_, Value::Str(s))) => Ok(Some(s.clone())),
            Some((p, _)) => err(p, format!("{}: {key} must be a string", self.what)),
        }
    }

    /// SQL text: `"..."` or Prisma's `raw("...")`.
    fn sql(&mut self, key: &'a str) -> Result<Option<String>> {
        match self.get(key) {
            None => Ok(None),
            Some((p, v)) => Ok(Some(raw_sql(p, v, &format!("{}: {key}", self.what))?)),
        }
    }

    /// A bare name or a string.
    fn name(&mut self, key: &'a str) -> Result<Option<String>> {
        match self.get(key) {
            None => Ok(None),
            Some((p, v)) => Ok(Some(name_of(p, v, &format!("{}: {key}", self.what))?)),
        }
    }

    /// The database name of an index or constraint: `name:` or Prisma's `map:`.
    fn db_name(&mut self) -> Result<Option<String>> {
        match (self.str("name")?, self.str("map")?) {
            (Some(_), Some(_)) => err(self.pos, format!("{}: give name: or map:, not both", self.what)),
            (a, b) => Ok(a.or(b)),
        }
    }

    fn bool(&mut self, key: &'a str) -> Result<bool> {
        match self.get(key) {
            None => Ok(false),
            Some((_, Value::Bool(b))) => Ok(*b),
            Some((p, _)) => err(p, format!("{}: {key} must be true or false", self.what)),
        }
    }

    fn names(&mut self, key: &'a str) -> Result<Vec<String>> {
        match self.get(key) {
            None => Ok(vec![]),
            Some((_, Value::List(items))) => items.iter().map(|(p, v)| name_of(*p, v, &self.what)).collect(),
            Some((p, v)) => Ok(vec![name_of(p, v, &self.what)?]),
        }
    }

    fn finish(self) -> Result<()> {
        if let Some((k, p, _)) = self.args.named.iter().find(|(k, _, _)| !self.used.contains(k.as_str())) {
            return err(*p, format!("{}: unknown argument `{k}`", self.what));
        }
        Ok(())
    }

    fn no_positional(&self) -> Result<()> {
        match self.args.positional.first() {
            Some((p, _)) => err(*p, format!("{} takes no positional arguments", self.what)),
            None => Ok(()),
        }
    }
}

fn name_of(pos: Pos, v: &Value, what: &str) -> Result<String> {
    match v {
        Value::Path(p, None) if p.len() == 1 => Ok(p[0].clone()),
        Value::Str(s) => Ok(s.clone()),
        _ => err(pos, format!("{what}: expected a name")),
    }
}

/// `"..."` or `raw("...")`.
fn raw_sql(pos: Pos, v: &Value, what: &str) -> Result<String> {
    match v {
        Value::Str(s) => Ok(s.clone()),
        Value::Path(p, Some(args)) if p.len() == 1 && p[0] == "raw" => match args.positional.as_slice() {
            [(_, Value::Str(s))] if args.named.is_empty() => Ok(s.clone()),
            _ => err(pos, format!("{what}: raw(\"...\") takes one string")),
        },
        _ => err(pos, format!("{what}: expected SQL as \"...\" or raw(\"...\")")),
    }
}

/// A call `name(...)` with one string argument, e.g. `dbgenerated("now()")`.
fn call_str<'v>(v: &'v Value, name: &str) -> Option<std::result::Result<&'v str, ()>> {
    match v {
        Value::Path(p, args) if p.len() == 1 && p[0] == name => Some(match args.as_ref().map(|a| (a.positional.as_slice(), a.named.len())) {
            Some(([(_, Value::Str(s))], 0)) => Ok(s.as_str()),
            _ => Err(()),
        }),
        _ => None,
    }
}

/// A type or storage-parameter argument as SQL text.
fn sql_text(pos: Pos, v: &Value) -> Result<String> {
    match v {
        Value::Num(n) => Ok(n.clone()),
        Value::Str(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Path(p, None) => Ok(p.join(".")),
        _ => err(pos, "expected a number, name or string"),
    }
}

fn json_of(pos: Pos, v: &Value) -> Result<serde_json::Value> {
    Ok(match v {
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Num(n) => serde_json::from_str(n).map_err(|e| super::syntax::Error { pos, msg: e.to_string() })?,
        Value::List(items) => serde_json::Value::Array(items.iter().map(|(p, v)| json_of(*p, v)).collect::<Result<_>>()?),
        Value::Object(_) | Value::Path(..) => return err(pos, "expected a literal"),
    })
}

fn deferrable(n: &mut Named<'_>) -> Result<Option<Deferrable>> {
    let pos = n.pos;
    Ok(match n.name("deferrable")?.as_deref() {
        None => None,
        Some("immediate") => Some(Deferrable::Immediate),
        Some("deferred") => Some(Deferrable::Deferred),
        Some(other) => return err(pos, format!("deferrable must be immediate or deferred, not {other}")),
    })
}

fn action(pos: Pos, s: &str) -> Result<OnDelete> {
    Ok(match s {
        "Cascade" => OnDelete::Cascade,
        "SetNull" => OnDelete::SetNull,
        "SetDefault" => OnDelete::SetDefault,
        "Restrict" => OnDelete::Restrict,
        "NoAction" => OnDelete::NoAction,
        _ => return err(pos, format!("unknown referential action {s}; use Cascade, SetNull, SetDefault, Restrict or NoAction")),
    })
}

/// Prisma's scalar types.
fn builtin_type(name: &str) -> Option<ColType> {
    Some(match name {
        "BigInt" => ColType::BigInt,
        "Int" => ColType::Int,
        "Float" => ColType::Float,
        "Boolean" => ColType::Bool,
        "String" => ColType::String,
        "DateTime" => ColType::DateTime,
        "Json" => ColType::Json,
        "Decimal" => ColType::Decimal,
        _ => return None,
    })
}

/// Prisma's Postgres native types (`@db.X`) as SQL, and whether `X` is the SQL type
/// the field type already maps to.
fn native_type(name: &str, ty: ColType) -> Option<(&'static str, bool)> {
    let sql = match name {
        "Text" => "text",
        "VarChar" => "varchar",
        "Char" => "char",
        "Bit" => "bit",
        "VarBit" => "varbit",
        "Uuid" => "uuid",
        "Xml" => "xml",
        "Inet" => "inet",
        "Boolean" => "boolean",
        "Integer" => "integer",
        "SmallInt" => "smallint",
        "Oid" => "oid",
        "BigInt" => "bigint",
        "DoublePrecision" => "double precision",
        "Real" => "real",
        "Decimal" => "numeric",
        "Money" => "money",
        "Timestamp" => "timestamp",
        "Timestamptz" => "timestamptz",
        "Date" => "date",
        "Time" => "time",
        "Timetz" => "timetz",
        "Json" => "json",
        "JsonB" => "jsonb",
        "ByteA" => "bytea",
        _ => return None,
    };
    let same = matches!(
        (name, ty),
        ("BigInt", ColType::BigInt)
            | ("Integer", ColType::Int)
            | ("DoublePrecision", ColType::Float)
            | ("Boolean", ColType::Bool)
            | ("Timestamptz", ColType::DateTime)
            | ("JsonB", ColType::Json)
            | ("Decimal", ColType::Decimal)
    );
    Some((sql, same))
}

const FIELD_ATTRS: &str = "@id, @unique, @default, @map, @db.*, @check, @comment, @renamed_from";

pub struct Lowering<'l> {
    /// Reads an imported file (path as written in the schema).
    pub load: &'l dyn Fn(&str) -> std::result::Result<String, String>,
}

struct Ctx {
    types: HashMap<String, (String, TypeDef)>,
    enums: HashMap<String, EnumIr>,
}

impl Lowering<'_> {
    pub fn lower(&self, items: Vec<Item>) -> Result<SchemaIr> {
        let mut ir = SchemaIr::default();
        let mut ctx = Ctx { types: HashMap::new(), enums: HashMap::new() };
        for def in ExtensionDef::builtin() {
            for (name, t) in &def.types {
                ctx.types.insert(name.clone(), (def.name.clone(), t.clone()));
            }
        }
        // imports first: their types are usable anywhere in the file
        for item in &items {
            if let Item::Import { pos, path, .. } = item {
                let src = (self.load)(path).map_err(|e| super::syntax::Error { pos: *pos, msg: format!("import {path:?}: {e}") })?;
                let def = ExtensionDef::parse(&src)
                    .map_err(|e| super::syntax::Error { pos: *pos, msg: format!("extension file {path:?}: {e}") })?;
                for (name, t) in &def.types {
                    ctx.types.insert(name.clone(), (def.name.clone(), t.clone()));
                }
                ir.catalog.push(ExtensionIr { name: def.name.clone(), schema: None, version: None, provides: def.provides() });
            }
        }
        let models: Vec<&ModelDecl> = items.iter().filter_map(|i| if let Item::Model(m) = i { Some(m) } else { None }).collect();
        let model_names: HashMap<&str, &ModelDecl> = models.iter().map(|m| (m.name.as_str(), *m)).collect();
        if model_names.len() != models.len() {
            let mut seen = HashSet::new();
            let dup = models.iter().find(|m| !seen.insert(m.name.as_str())).unwrap();
            return err(dup.pos, format!("model {} is declared twice", dup.name));
        }

        for item in &items {
            if let Item::Enum(e) = item {
                if model_names.contains_key(e.name.as_str()) || ctx.enums.contains_key(&e.name) {
                    return err(e.pos, format!("{} is declared twice", e.name));
                }
                let lowered = enum_block(e)?;
                ctx.enums.insert(e.name.clone(), lowered.clone());
                ir.enums.push(lowered);
            }
        }
        let mut db_names = HashSet::new();
        for e in ir.enums.iter().filter(|e| e.storage == EnumStorage::Native) {
            if !db_names.insert(e.db_name.as_str()) {
                return err(Pos::default(), format!("two enums use the database type {:?}", e.db_name));
            }
        }

        let mut datasource = false;
        for item in &items {
            match item {
                Item::Datasource { pos, props } => {
                    if std::mem::replace(&mut datasource, true) {
                        return err(*pos, "only one datasource block is allowed");
                    }
                    datasource_block(&mut ir, *pos, props)?;
                }
                Item::Function { pos, name, props } => ir.functions.push(function(*pos, name, props)?),
                Item::Model(_) | Item::Enum(_) | Item::Import { .. } => {}
            }
        }

        // fields first (relations need every model's primary key)
        let mut lowered: Vec<ModelIr> = vec![];
        for m in &models {
            lowered.push(model_fields(m, &model_names, &ctx)?);
        }
        let pks: HashMap<String, Option<String>> = lowered
            .iter()
            .map(|m| (m.name.clone(), m.fields.iter().find(|f| f.primary_key).map(|f| f.name.clone())))
            .collect();
        for (m, decl) in lowered.iter_mut().zip(&models) {
            if pks[&m.name].is_none() {
                return err(decl.pos, format!("model {} has no @id field", decl.name));
            }
            for member in decl.members.iter().filter(|mm| model_names.contains_key(mm.ty.name.as_str())) {
                let r = relation(decl, member, &m.fields, &model_names)?;
                m.relations.push(r);
            }
            for block in &decl.blocks {
                model_block(m, block)?;
            }
        }
        ir.models = lowered;
        Ok(ir)
    }
}

/// `enum Name { a b @map("B") ... @@map("db_name") @@storage(text) }`
fn enum_block(e: &EnumDecl) -> Result<EnumIr> {
    let what = format!("enum {}", e.name);
    let mut out = EnumIr {
        name: e.name.clone(),
        db_name: e.name.to_lowercase(),
        storage: EnumStorage::Native,
        values: vec![],
        comment: None,
    };
    for a in &e.blocks {
        let one = || -> Result<&Value> {
            match a.args.positional.as_slice() {
                [(_, v)] if a.args.named.is_empty() => Ok(v),
                _ => err(a.pos, format!("{what}: @@{} takes one argument", a.name)),
            }
        };
        match (a.name.as_str(), one()?) {
            ("map", Value::Str(s)) => out.db_name = s.clone(),
            ("comment", Value::Str(s)) => out.comment = Some(s.clone()),
            ("storage", v) => {
                out.storage = match name_of(a.pos, v, &what)?.as_str() {
                    "native" => EnumStorage::Native,
                    "text" => EnumStorage::Text,
                    "int" => EnumStorage::Int,
                    o => return err(a.pos, format!("{what}: @@storage is native, text or int, not {o}")),
                }
            }
            ("map" | "comment", _) => return err(a.pos, format!("{what}: @@{}(\"...\") takes a string", a.name)),
            (o, _) => return err(a.pos, format!("{what}: unknown attribute @@{o}; use @@map, @@storage or @@comment")),
        }
    }
    if e.values.is_empty() {
        return err(e.pos, format!("{what} has no values"));
    }
    let mut seen_names = HashSet::new();
    let mut seen_values = HashSet::new();
    for (pos, name, attrs) in &e.values {
        if !seen_names.insert(name.as_str()) {
            return err(*pos, format!("{what}: {name} is declared twice"));
        }
        let mut label: Option<String> = None;
        let mut number: Option<i64> = None;
        for a in attrs {
            let v = match a.args.positional.as_slice() {
                [(_, v)] if a.args.named.is_empty() => v,
                _ => return err(a.pos, format!("{what}.{name}: @{} takes one argument", a.name)),
            };
            match (a.name.as_str(), v) {
                ("map", Value::Str(s)) => label = Some(s.clone()),
                ("value", Value::Num(n)) => {
                    number = Some(n.parse().map_err(|_| super::syntax::Error {
                        pos: a.pos,
                        msg: format!("{what}.{name}: @value takes an integer"),
                    })?)
                }
                ("map", _) => return err(a.pos, format!("{what}.{name}: @map takes a string")),
                ("value", _) => return err(a.pos, format!("{what}.{name}: @value takes an integer")),
                (o, _) => return err(a.pos, format!("{what}.{name}: unknown attribute @{o}; use @map or @value")),
            }
        }
        let value = match out.storage {
            EnumStorage::Int => {
                if label.is_some() {
                    return err(*pos, format!("{what}.{name}: an int enum stores @value(n), not @map"));
                }
                match number {
                    Some(n) => serde_json::Value::from(n),
                    None => return err(*pos, format!("{what}.{name}: values of an int enum need @value(n)")),
                }
            }
            _ => {
                if number.is_some() {
                    return err(*pos, format!("{what}.{name}: @value(n) is for @@storage(int); use @map(\"...\")"));
                }
                serde_json::Value::String(label.unwrap_or_else(|| name.clone()))
            }
        };
        if !seen_values.insert(value.to_string()) {
            return err(*pos, format!("{what}: two values are stored as {value}"));
        }
        out.values.push(EnumValueIr { name: name.clone(), value });
    }
    Ok(out)
}

/// `datasource db { provider = "postgresql" extensions = [...] }`
fn datasource_block(ir: &mut SchemaIr, pos: Pos, props: &Props) -> Result<()> {
    let args = Args { pos, positional: vec![], named: props.clone() };
    let mut n = Named::new("datasource", &args);
    match n.get("provider") {
        Some((_, Value::Str(p))) if p == "postgresql" || p == "postgres" => {}
        Some((_, Value::Str(p))) if p == "sqlite" => ir.dialect = crate::dialect::Dialect::Sqlite,
        Some((p, _)) => return err(p, r#"datasource: provider must be "postgresql" or "sqlite""#),

        None => return err(pos, "datasource: provider = \"postgresql\" is missing"),
    }
    // The connection comes from ORM_DATABASE_URL / --url; Prisma's keys are accepted.
    for key in ["url", "directUrl", "shadowDatabaseUrl"] {
        n.get(key);
    }
    if let Some((p, v)) = n.get("extensions") {
        let Value::List(list) = v else { return err(p, "datasource: extensions takes a list, e.g. [postgis, citext]") };
        for (p, v) in list {
            let (ident, args) = match v {
                Value::Path(path, args) if path.len() == 1 => (&path[0], args.clone().unwrap_or(Args { pos: *p, ..Default::default() })),
                _ => return err(*p, "datasource: an extension is a name, e.g. postgis(schema: \"ext\", version: \"3.4\")"),
            };
            let mut e = Named::new(format!("extension {ident}"), &args);
            e.no_positional()?;
            let name = e.str("map")?.unwrap_or_else(|| ident.clone());
            let schema = e.str("schema")?;
            let version = e.str("version")?;
            e.finish()?;
            if ir.extensions.iter().any(|x| x.name == name) {
                return err(*p, format!("extension {name} is listed twice"));
            }
            let provides = ir.catalog.iter().find(|x| x.name == name).map(|x| x.provides.clone()).unwrap_or_default();
            ir.extensions.push(ExtensionIr { name, schema, version, provides });
        }
    }
    n.finish()
}

fn function(pos: Pos, name: &str, props: &Props) -> Result<FunctionIr> {
    let args = Args { pos, positional: vec![], named: props.to_vec() };
    let mut n = Named::new(format!("function {name}"), &args);
    let body = n.str("body")?;
    let f = FunctionIr {
        name: name.to_owned(),
        args: n.str("args")?.unwrap_or_default(),
        returns: n.name("returns")?.unwrap_or_else(|| "trigger".into()),
        language: n.name("language")?,
        body: match body {
            Some(b) => b,
            None => return err(pos, format!("function {name} needs a body")),
        },
        volatility: n.name("volatility")?,
        security_definer: n.bool("security_definer")?,
    };
    n.finish()?;
    Ok(f)
}

fn model_fields(m: &ModelDecl, models: &HashMap<&str, &ModelDecl>, ctx: &Ctx) -> Result<ModelIr> {
    let mut ir = ModelIr {
        name: m.name.clone(),
        table: m.name.to_lowercase(),
        fields: vec![],
        relations: vec![],
        indexes: vec![],
        constraints: vec![],
        triggers: vec![],
        renamed_from: None,
        comment: None,
    };
    // model-level attributes that aren't schema objects; the rest come after relations
    for a in &m.blocks {
        let text = |a: &Attr| -> Result<String> {
            match a.args.positional.as_slice() {
                [(_, Value::Str(s))] if a.args.named.is_empty() => Ok(s.clone()),
                _ => err(a.pos, format!("{}: @@{}(\"...\") takes one string", m.name, a.name)),
            }
        };
        match a.name.as_str() {
            "map" => ir.table = text(a)?,
            "comment" => ir.comment = Some(text(a)?),
            "renamed_from" => ir.renamed_from = Some(text(a)?),
            _ => {}
        }
    }
    let mut seen = HashSet::new();
    for member in &m.members {
        if !seen.insert(member.name.as_str()) {
            return err(member.pos, format!("{}.{} is declared twice", m.name, member.name));
        }
        if models.contains_key(member.ty.name.as_str()) {
            continue;
        }
        ir.fields.push(field(m, member, ctx)?);
    }
    Ok(ir)
}

fn field(m: &ModelDecl, member: &Member, ctx: &Ctx) -> Result<FieldIr> {
    let TypeRef { pos, name, args, list, optional } = &member.ty;
    let what = format!("{}.{}", m.name, member.name);
    let mut f = FieldIr {
        name: member.name.clone(),
        column: member.name.clone(),
        ty: ColType::Text,
        nullable: *optional,
        array: *list,
        enum_name: None,
        enum_idx: None,
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
        hints: BTreeMap::new(),
    };
    let resolve = |f: &mut FieldIr, ty: &str, values: &[String]| -> Result<()> {
        let Some((ext, def)) = ctx.types.get(ty) else {
            return err(*pos, format!("{what}: unknown type {ty}; extension types come from the built-in extensions or an imported file"));
        };
        let r = def.resolve(ext, ty, values).map_err(|e| super::syntax::Error { pos: *pos, msg: format!("{what}: {e}") })?;
        f.ty = r.value;
        f.db_type = Some(r.db_type);
        f.read_sql = r.read_sql;
        f.write_sql = r.write_sql;
        f.hints = r.hints;
        f.requires = vec![r.extension];
        Ok(())
    };
    let decimal = name == "Decimal";
    let enum_ir = ctx.enums.get(name.as_str());
    if let Some(e) = enum_ir {
        if !args.positional.is_empty() || !args.named.is_empty() {
            return err(args.pos, format!("{what}: {name} takes no arguments"));
        }
        f.enum_name = Some(e.name.clone());
        match e.storage {
            EnumStorage::Native => {
                f.ty = ColType::String;
                let t = ident(&e.db_name);
                f.write_sql = Some(format!("CAST({{}} AS {t}{})", if *list { "[]" } else { "" }));
                f.db_type = Some(t);
            }
            EnumStorage::Text => f.ty = ColType::Text,
            EnumStorage::Int => f.ty = ColType::Int,
        }
    } else if name == "Unsupported" {
        if *list {
            return err(*pos, format!("{what}: arrays of extension types aren't supported"));
        }
        // Unsupported("vector(384)"): a type from the extension catalog
        let sql = match args.positional.as_slice() {
            [(_, Value::Str(s))] if args.named.is_empty() => s.trim(),
            _ => return err(args.pos, format!("{what}: Unsupported takes the SQL type as a string, e.g. Unsupported(\"vector(384)\")")),
        };
        let (ty, values) = match sql.split_once('(') {
            Some((ty, rest)) => {
                let Some(inner) = rest.trim_end().strip_suffix(')') else {
                    return err(*pos, format!("{what}: unbalanced parentheses in {sql:?}"));
                };
                (ty.trim(), inner.split(',').map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()).collect())
            }
            None => (sql, vec![]),
        };
        resolve(&mut f, ty, &values)?;
    } else if let Some(ty) = builtin_type(name) {
        f.ty = ty;
        if !args.positional.is_empty() || !args.named.is_empty() {
            return err(args.pos, format!("{what}: {name} takes no arguments"));
        }
        if decimal {
            f.db_type = Some("numeric".into());
        }
    } else {
        return err(
            *pos,
            format!(
                "{what}: unknown type {name}; expected a model or BigInt, Int, Float, Boolean, String, DateTime, \
                 Json, Decimal, an enum or Unsupported(\"<extension type>\")"
            ),
        );
    }

    let mut native = false;
    for a in &member.attrs {
        fn one<'a>(a: &'a Attr, what: &str) -> Result<&'a Value> {
            match a.args.positional.as_slice() {
                [(_, v)] if a.args.named.is_empty() => Ok(v),
                _ => err(a.pos, format!("{what}: @{} takes one argument", a.name)),
            }
        }
        let text = |a: &Attr| -> Result<String> {
            match one(a, &what)? {
                Value::Str(s) => Ok(s.clone()),
                _ => err(a.pos, format!("{what}: @{}(...) takes a string", a.name)),
            }
        };
        let flag = |a: &Attr| -> Result<bool> {
            if a.args.positional.is_empty() && a.args.named.is_empty() {
                Ok(true)
            } else {
                err(a.pos, format!("{what}: @{} takes no arguments", a.name))
            }
        };
        if let Some(db) = a.name.strip_prefix("db.") {
            if std::mem::replace(&mut native, true) {
                return err(a.pos, format!("{what}: only one @db.* attribute"));
            }
            if name == "Unsupported" {
                return err(a.pos, format!("{what}: Unsupported(...) already names the SQL type"));
            }
            if enum_ir.is_some() {
                return err(a.pos, format!("{what}: an enum field's type comes from the enum (@@storage)"));
            }
            if !a.args.named.is_empty() {
                return err(a.pos, format!("{what}: @db.{db} takes positional arguments"));
            }
            let values = a.args.positional.iter().map(|(p, v)| sql_text(*p, v)).collect::<Result<Vec<_>>>()?;
            match (f.ty, db, values.as_slice()) {
                (ColType::String, "VarChar", [n]) if !decimal => {
                    f.max_length = Some(n.parse().map_err(|_| super::syntax::Error { pos: a.pos, msg: format!("{what}: @db.VarChar(n) needs a positive integer") })?)
                }
                (ColType::String, "VarChar", []) if !decimal => {}
                (ColType::String, "Text", []) if !decimal => f.ty = ColType::Text,
                (ColType::String, "Uuid", []) if !decimal => f.ty = ColType::Uuid,
                (ColType::String, "Citext", []) if !decimal && !*list => resolve(&mut f, "citext", &[])?,
                (ColType::DateTime, "Date", []) => f.ty = ColType::Date,
                (ty, _, _) => {
                    let Some((sql, same)) = native_type(db, ty) else {
                        return err(a.pos, format!("{what}: unknown native type @db.{db}"));
                    };
                    if !(same && values.is_empty()) {
                        f.db_type = Some(if values.is_empty() { sql.to_owned() } else { format!("{sql}({})", values.join(", ")) });
                    }
                }
            }
            continue;
        }
        match a.name.as_str() {
            "id" => f.primary_key = flag(a)?,
            "unique" => f.unique = flag(a)?,
            "check" => f.check = Some(text(a)?),
            "comment" => f.comment = Some(text(a)?),
            "map" => f.column = text(a)?,
            "renamed_from" => f.renamed_from = Some(text(a)?),
            "default" => {
                let v = one(a, &what)?;
                let bad_call = |c: &str| err(a.pos, format!("{what}: {c}(\"...\") takes one string"));
                match v {
                    Value::Path(p, Some(args)) if p.len() == 1 && p[0] == "autoincrement" && args.positional.is_empty() && args.named.is_empty() => {
                        f.auto_increment = true
                    }
                    Value::Path(p, Some(args)) if p.len() == 1 && p[0] == "now" && args.positional.is_empty() && args.named.is_empty() => {
                        f.default_now = true
                    }
                    // an enum value: `@default(draft)`
                    Value::Path(p, None) if p.len() == 1 && enum_ir.is_some() => {
                        let e = enum_ir.unwrap();
                        match e.values.iter().find(|x| x.name == p[0]) {
                            Some(x) => f.default = Some(x.value.clone()),
                            None => return err(a.pos, format!("{what}: {} has no value {}", e.name, p[0])),
                        }
                    }
                    Value::List(items) if *list && enum_ir.is_some() => {
                        let e = enum_ir.unwrap();
                        let mut out = vec![];
                        for (p, v) in items {
                            let n = name_of(*p, v, &what)?;
                            match e.values.iter().find(|x| x.name == n) {
                                Some(x) => out.push(x.value.clone()),
                                None => return err(*p, format!("{what}: {} has no value {n}", e.name)),
                            }
                        }
                        f.default = Some(serde_json::Value::Array(out));
                    }
                    _ => match call_str(v, "dbgenerated") {
                        Some(Ok(s)) => f.default_sql = Some(s.to_owned()),
                        Some(Err(())) => return bad_call("dbgenerated"),
                        None => match v {
                            Value::Path(p, _) => {
                                return err(
                                    a.pos,
                                    format!(
                                        "{what}: @default takes a literal, autoincrement(), now() or dbgenerated(\"...\"), not {}",
                                        p.join(".")
                                    ),
                                )
                            }
                            // a Json default is its JSON text
                            Value::Str(s) if f.ty == ColType::Json => {
                                f.default = Some(serde_json::from_str(s).map_err(|e| super::syntax::Error {
                                    pos: a.pos,
                                    msg: format!("{what}: @default on a Json field is JSON text: {e}"),
                                })?)
                            }
                            v => f.default = Some(json_of(a.pos, v)?),
                        },
                    },
                }
            }
            "relation" => return err(a.pos, format!("{what}: @relation goes on a field whose type is a model")),
            other => return err(a.pos, format!("{what}: unknown attribute @{other}; use {FIELD_ATTRS}")),
        }
    }
    // parameters of other array types are cast to the column's (`varchar(20)[]`, ...)
    if f.array && f.write_sql.is_none() && (f.db_type.is_some() || f.max_length.is_some()) {
        f.write_sql = Some(format!("CAST({{}} AS {})", crate::migrate::model::sql_type(&f)));
    }
    if f.array && (f.primary_key || f.auto_increment) {
        return err(member.pos, format!("{what}: an array can't be a primary key"));
    }
    if f.primary_key && f.nullable {
        return err(member.pos, format!("{what}: a primary key can't be optional"));
    }
    Ok(f)
}

/// The parts of `@relation(...)` both sides of a relation need.
struct RelArgs {
    name: Option<String>,
    fields: Vec<String>,
    references: Vec<String>,
}

fn rel_args(a: &Attr) -> RelArgs {
    let names = |key: &str| match a.args.named.iter().find(|(k, _, _)| k == key) {
        Some((_, _, Value::List(items))) => items.iter().filter_map(|(p, v)| name_of(*p, v, "").ok()).collect(),
        _ => vec![],
    };
    let name = match (a.args.positional.first(), a.args.named.iter().find(|(k, _, _)| k == "name")) {
        (Some((_, Value::Str(s))), _) | (None, Some((_, _, Value::Str(s)))) => Some(s.clone()),
        _ => None,
    };
    RelArgs { name, fields: names("fields"), references: names("references") }
}

fn relation(m: &ModelDecl, member: &Member, fields: &[FieldIr], models: &HashMap<&str, &ModelDecl>) -> Result<RelationIr> {
    let target = &member.ty.name;
    let what = format!("relation {}.{}", m.name, member.name);
    if let Some(a) = member.attrs.iter().find(|a| a.name != "relation") {
        return err(a.pos, format!("{what}: relations take only @relation(...)"));
    }
    let empty = Attr { pos: member.pos, name: "relation".into(), args: Args { pos: member.pos, ..Default::default() } };
    let rel = member.attrs.iter().find(|a| a.name == "relation").unwrap_or(&empty);
    let mut n = Named::new(what.clone(), &rel.args);
    let name = match rel.args.positional.as_slice() {
        [] => n.str("name")?,
        [(_, Value::Str(s))] => Some(s.clone()),
        [(p, _), ..] => return err(*p, format!("{what}: the only positional argument is the relation name, a string")),
    };
    let target_decl = models[target.as_str()];
    let target_fields: Vec<&str> = target_decl
        .members
        .iter()
        .filter(|m| !models.contains_key(m.ty.name.as_str()))
        .map(|m| m.name.as_str())
        .collect();
    let field_list = |n: &mut Named<'_>, key: &'static str| -> Result<Option<(Pos, String)>> {
        match n.get(key) {
            None => Ok(None),
            Some((p, Value::List(items))) => match items.as_slice() {
                [(fp, v)] => Ok(Some((*fp, name_of(*fp, v, &what)?))),
                _ => err(p, format!("{what}: {key}: takes one field (composite keys aren't supported)")),
            },
            Some((p, _)) => err(p, format!("{what}: {key}: takes a list, e.g. [author_id]")),
        }
    };
    if let Some((tp, v)) = n.get("through") {
        let join = name_of(tp, v, &what)?;
        let through_fields = n.names("through_fields")?;
        n.finish()?;
        return many_to_many(m, member, &what, tp, &join, &through_fields, models);
    }
    let from = field_list(&mut n, "fields")?;
    let to = field_list(&mut n, "references")?;

    let Some((fpos, from)) = from else {
        // The back side: the other model's relation to this one holds the key.
        if to.is_some() {
            return err(rel.pos, format!("{what}: references: needs fields:"));
        }
        n.finish()?;
        let back: Vec<RelArgs> = target_decl
            .members
            .iter()
            .filter(|o| o.ty.name == m.name && !std::ptr::eq(*o, member))
            .filter_map(|o| o.attrs.iter().find(|a| a.name == "relation").map(rel_args))
            .filter(|r| !r.fields.is_empty() && r.name == name)
            .collect();
        let back = match back.as_slice() {
            [b] => b,
            [] => {
                return err(
                    member.pos,
                    format!(
                        "{what}: {target} has no relation to {} with fields: / references:{}",
                        m.name,
                        name.as_ref().map(|n| format!(" named {n:?}")).unwrap_or_default()
                    ),
                )
            }
            _ => return err(member.pos, format!("{what}: {target} has several relations to {}; name them: @relation(\"name\", ...) on both sides", m.name)),
        };
        let (Some(from), Some(to)) = (back.references.first(), back.fields.first()) else {
            return err(member.pos, format!("{what}: the relation on {target} needs references:"));
        };
        let kind = if member.ty.list {
            if member.ty.optional {
                return err(member.ty.pos, format!("{what}: a to-many relation can't be optional"));
            }
            RelKind::Many
        } else {
            // has-one: the key on the other side must be unique
            if !member.ty.optional {
                return err(member.ty.pos, format!("{what}: a one-to-one back relation is optional: {target}?"));
            }
            if !is_unique(target_decl, to) {
                return err(
                    member.pos,
                    format!("{what}: {target}.{to} must be unique for a one-to-one relation (@unique, or {target}[] for to-many)"),
                );
            }
            RelKind::One
        };
        return Ok(RelationIr {
            name: member.name.clone(),
            kind,
            target: target.clone(),
            from: from.clone(),
            to: to.clone(),
            foreign_key: false,
            on_delete: None,
            on_update: None,
            deferrable: None,
            through: None,
        });
    };
    if member.ty.list {
        return err(rel.pos, format!("{what}: fields: goes on the to-one side, not on {target}[]"));
    }
    let Some(local) = fields.iter().find(|f| f.name == from) else {
        return err(fpos, format!("{what}: no field {from} in this model"));
    };
    if local.nullable != member.ty.optional {
        let hint = if local.nullable { format!("{target}?") } else { target.clone() };
        return err(member.ty.pos, format!("{what}: {from} is {}nullable, so the relation type is `{hint}`", if local.nullable { "" } else { "not " }));
    }
    let Some((tpos, to)) = to else { return err(rel.pos, format!("{what}: fields: needs references: [<field of {target}>]")) };
    if !target_fields.contains(&to.as_str()) {
        return err(tpos, format!("{what}: {target} has no field {to}"));
    }
    let on_delete = match n.get("onDelete") {
        Some((p, v)) => Some(action(p, &name_of(p, v, &what)?)?),
        None => None,
    };
    let on_update = match n.get("onUpdate") {
        Some((p, v)) => Some(action(p, &name_of(p, v, &what)?)?),
        None => None,
    };
    let deferrable = deferrable(&mut n)?;
    n.finish()?;
    Ok(RelationIr {
        name: member.name.clone(),
        kind: RelKind::One,
        target: target.clone(),
        from,
        to,
        foreign_key: true,
        on_delete,
        on_update,
        deferrable,
        through: None,
    })
}

/// Whether `field` of `model` holds unique values: `@id`, `@unique` or a one-field
/// `@@unique([field])`.
fn is_unique(model: &ModelDecl, field: &str) -> bool {
    let own = model
        .members
        .iter()
        .any(|mm| mm.name == field && mm.attrs.iter().any(|a| a.name == "unique" || a.name == "id"));
    own || model.blocks.iter().any(|b| {
        b.name == "unique"
            && matches!(b.args.positional.first(), Some((_, Value::List(items)))
                if items.len() == 1 && matches!(&items[0].1, Value::Path(p, None) if p.len() == 1 && p[0] == field))
    })
}

/// `tags Tag[] @relation(through: PostTag)`: a many-to-many relation through the
/// to-one relations of a join model, one to each side (`through_fields: [post, tag]`
/// names them when the join model has several).
fn many_to_many(
    m: &ModelDecl,
    member: &Member,
    what: &str,
    pos: Pos,
    join: &str,
    through_fields: &[String],
    models: &HashMap<&str, &ModelDecl>,
) -> Result<RelationIr> {
    let target = &member.ty.name;
    if !member.ty.list || member.ty.optional {
        return err(member.ty.pos, format!("{what}: a many-to-many relation is a list: {target}[]"));
    }
    let Some(join_decl) = models.get(join) else {
        return err(pos, format!("{what}: through: {join} is not a model"));
    };
    // the join model's to-one relations: (relation name, target, fields, references)
    let links: Vec<(&str, &str, RelArgs)> = join_decl
        .members
        .iter()
        .filter(|o| models.contains_key(o.ty.name.as_str()) && !o.ty.list)
        .filter_map(|o| o.attrs.iter().find(|a| a.name == "relation").map(|a| (o.name.as_str(), o.ty.name.as_str(), rel_args(a))))
        .filter(|(_, _, r)| !r.fields.is_empty())
        .collect();
    let pick = |rel: Option<&String>, to: &str, side: &str| -> Result<&RelArgs> {
        let found: Vec<&(&str, &str, RelArgs)> = match rel {
            Some(r) => links.iter().filter(|(n, _, _)| n == r).collect(),
            None => links.iter().filter(|(_, t, _)| *t == to).collect(),
        };
        match found.as_slice() {
            [(_, t, r)] if *t == to => Ok(r),
            [(n, t, _)] => err(pos, format!("{what}: {join}.{n} points at {t}, not {to}")),
            [] => err(pos, format!("{what}: {join} has no relation with fields: {} for the {side} side", match rel {
                Some(r) => format!("named {r}"),
                None => format!("to {to}"),
            })),
            _ => err(pos, format!("{what}: {join} has several relations to {to}; say which with through_fields: [<{side} side>, ...]")),
        }
    };
    let (source, target_link) = match through_fields {
        [] if m.name == *target => {
            return err(pos, format!("{what}: a many-to-many relation of {target} to itself needs through_fields: [<from>, <to>]"))
        }
        [] => (pick(None, &m.name, "source")?, pick(None, target, "target")?),
        [a, b] => (pick(Some(a), &m.name, "source")?, pick(Some(b), target, "target")?),
        _ => return err(pos, format!("{what}: through_fields takes two relations of {join}, e.g. [post, tag]")),
    };
    let (Some(from), Some(src)) = (source.references.first(), source.fields.first()) else {
        return err(pos, format!("{what}: the relation of {join} to {} needs fields: and references:", m.name));
    };
    let (Some(to), Some(dst)) = (target_link.references.first(), target_link.fields.first()) else {
        return err(pos, format!("{what}: the relation of {join} to {target} needs fields: and references:"));
    };
    Ok(RelationIr {
        name: member.name.clone(),
        kind: RelKind::Many,
        target: target.clone(),
        from: from.clone(),
        to: to.clone(),
        foreign_key: false,
        on_delete: None,
        on_update: None,
        deferrable: None,
        through: Some(ThroughIr { model: join.to_owned(), source: src.clone(), target: dst.clone() }),
    })
}

/// `[field, field(sort: Desc, ops: raw("x")), sql("expr", ...)]` -> index keys (and
/// the exclusion operator of each, when `with_op`).
fn keys(pos: Pos, v: Option<&Value>, what: &str, with_op: bool) -> Result<Vec<(IndexColumnIr, Option<String>)>> {
    let Some(Value::List(items)) = v else {
        return err(pos, format!("{what}: the first argument is a list of keys, e.g. [author_id, created_at(sort: Desc)]"));
    };
    if items.is_empty() {
        return err(pos, format!("{what}: no keys"));
    }
    let mut out = vec![];
    for (p, item) in items {
        let (field, expr, args) = match item {
            Value::Path(path, args) if path.len() == 1 && path[0] == "sql" => {
                let Some(args) = args else { return err(*p, "sql(\"...\") needs an expression") };
                let Some((_, Value::Str(e))) = args.positional.first() else {
                    return err(*p, "sql(\"...\") takes the expression as a string");
                };
                (None, Some(e.clone()), args.clone())
            }
            Value::Path(path, args) if path.len() == 1 => {
                (Some(path[0].clone()), None, args.clone().unwrap_or(Args { pos: *p, ..Default::default() }))
            }
            _ => return err(*p, format!("{what}: a key is a field name or sql(\"...\")")),
        };
        if args.positional.len() > usize::from(expr.is_some()) {
            return err(*p, format!("{what}: key options are named (sort:, nulls:, ops:, collate:{})", if with_op { ", op:" } else { "" }));
        }
        let mut n = Named::new(format!("{what} key"), &args);
        let desc = match n.name("sort")?.as_deref() {
            None | Some("Asc") => false,
            Some("Desc") => true,
            Some(o) => return err(*p, format!("sort must be Asc or Desc, not {o}")),
        };
        let nulls = match n.name("nulls")?.as_deref() {
            None => None,
            Some("first") => Some(Nulls::First),
            Some("last") => Some(Nulls::Last),
            Some(o) => return err(*p, format!("nulls must be first or last, not {o}")),
        };
        let opclass = match n.get("ops") {
            None => None,
            Some((op, v @ Value::Path(path, Some(_)))) if path.len() == 1 && path[0] == "raw" => Some(raw_sql(op, v, what)?),
            Some((op, v)) => Some(name_of(op, v, &format!("{what}: ops"))?),
        };
        let collation = n.str("collate")?;
        let op = if with_op { n.str("op")? } else { None };
        n.finish()?;
        if with_op && op.is_none() {
            return err(*p, format!("{what}: each key needs op: \"=\" / \"&&\" / ..."));
        }
        out.push((IndexColumnIr { field, expr, opclass, collation, desc, nulls }, op));
    }
    Ok(out)
}

/// `type: Gin` (Prisma's names) or `type: hnsw` -> the access method.
fn method(n: &mut Named<'_>) -> Result<Option<String>> {
    Ok(n.name("type")?.map(|t| t.to_lowercase()))
}

fn model_block(m: &mut ModelIr, b: &Attr) -> Result<()> {
    let what = format!("{}: @@{}", m.name, b.name);
    let first = b.args.positional.first().map(|(_, v)| v);
    let extra_positional = |max: usize| -> Result<()> {
        match b.args.positional.get(max) {
            Some((p, _)) => err(*p, format!("{what}: too many positional arguments")),
            None => Ok(()),
        }
    };
    match b.name.as_str() {
        "map" | "comment" | "renamed_from" => {}
        "index" => {
            extra_positional(1)?;
            let columns: Vec<IndexColumnIr> = keys(b.pos, first, &what, false)?.into_iter().map(|(k, _)| k).collect();
            // `@@index([col])` is the column's own index
            if let ([c], []) = (columns.as_slice(), b.args.named.as_slice()) {
                if c.expr.is_none() && c.opclass.is_none() && c.collation.is_none() && !c.desc && c.nulls.is_none() {
                    if let Some(f) = m.fields.iter_mut().find(|f| Some(&f.name) == c.field.as_ref()) {
                        f.index = true;
                        return Ok(());
                    }
                }
            }
            let mut n = Named::new(what.clone(), &b.args);
            let method = method(&mut n)?;
            let with = match n.get("with") {
                None => vec![],
                Some((_, Value::Object(entries))) => {
                    entries.iter().map(|(k, p, v)| Ok((k.clone(), param(*p, v)?))).collect::<Result<_>>()?
                }
                Some((p, _)) => return err(p, format!("{what}: with: takes {{ name: value, ... }}")),
            };
            let ix = IndexIr {
                name: n.db_name()?,
                columns,
                unique: n.bool("unique")?,
                method,
                where_: n.sql("where")?,
                include: n.names("include")?,
                with,
                nulls_not_distinct: n.bool("nulls_not_distinct")?,
                requires: vec![],
            };
            n.finish()?;
            m.indexes.push(ix);
        }
        "unique" => {
            extra_positional(1)?;
            let fields = match first {
                Some(Value::List(items)) => items.iter().map(|(p, v)| name_of(*p, v, &what)).collect::<Result<Vec<_>>>()?,
                _ => return err(b.pos, format!("{what}: takes a list of fields, e.g. @@unique([author_id, slug])")),
            };
            let mut n = Named::new(what.clone(), &b.args);
            let c = ConstraintIr::Unique {
                name: n.db_name()?,
                fields,
                nulls_not_distinct: n.bool("nulls_not_distinct")?,
                deferrable: deferrable(&mut n)?,
            };
            n.finish()?;
            m.constraints.push(c);
        }
        "check" => {
            extra_positional(1)?;
            let Some(Value::Str(expr)) = first else {
                return err(b.pos, format!("{what}: takes the SQL expression as a string"));
            };
            let mut n = Named::new(what.clone(), &b.args);
            let c = ConstraintIr::Check { name: n.str("name")?, expr: expr.clone() };
            n.finish()?;
            m.constraints.push(c);
        }
        "exclude" => {
            extra_positional(1)?;
            let ks = keys(b.pos, first, &what, true)?;
            let mut n = Named::new(what.clone(), &b.args);
            let method = method(&mut n)?.unwrap_or_else(|| "gist".into());
            // `=` on a plain column inside GiST needs btree_gist.
            let requires = if method == "gist" && ks.iter().any(|(k, op)| k.field.is_some() && op.as_deref() == Some("=")) {
                vec!["btree_gist".to_owned()]
            } else {
                vec![]
            };
            let c = ConstraintIr::Exclude {
                name: n.str("name")?,
                method: Some(method),
                elements: ks
                    .into_iter()
                    .map(|(column, op)| ExcludeElementIr { column, operator: op.unwrap_or_default() })
                    .collect(),
                where_: n.sql("where")?,
                deferrable: deferrable(&mut n)?,
                requires,
            };
            n.finish()?;
            m.constraints.push(c);
        }
        "trigger" => {
            extra_positional(1)?;
            let name = match b.args.positional.first() {
                Some((p, v)) => name_of(*p, v, &what)?,
                None => return err(b.pos, format!("{what}: the first argument is the trigger name")),
            };
            let mut n = Named::new(format!("{what}({name})"), &b.args);
            let mut timing = None;
            for (key, t) in [("before", TriggerTiming::Before), ("after", TriggerTiming::After), ("instead_of", TriggerTiming::InsteadOf)] {
                let events = n.names(key)?;
                if !events.is_empty() {
                    if timing.is_some() {
                        return err(b.pos, format!("{what}({name}): give one of before: / after: / instead_of:"));
                    }
                    let events = events
                        .iter()
                        .map(|e| match e.as_str() {
                            "insert" => Ok(TriggerEvent::Insert),
                            "update" => Ok(TriggerEvent::Update),
                            "delete" => Ok(TriggerEvent::Delete),
                            "truncate" => Ok(TriggerEvent::Truncate),
                            o => err(b.pos, format!("unknown trigger event {o}")),
                        })
                        .collect::<Result<Vec<_>>>()?;
                    timing = Some((t, events));
                }
            }
            let Some((timing, events)) = timing else {
                return err(b.pos, format!("{what}({name}): say when it fires: before: [update], after: [insert, delete], ..."));
            };
            let for_each = match n.name("for_each")?.as_deref() {
                None | Some("row") => ForEach::Row,
                Some("statement") => ForEach::Statement,
                Some(o) => return err(b.pos, format!("for_each must be row or statement, not {o}")),
            };
            let t = TriggerIr {
                name,
                timing,
                events,
                update_of: n.names("update_of")?,
                for_each,
                when: n.sql("when")?,
                function: n.name("function")?,
                args: match n.get("args") {
                    None => vec![],
                    Some((_, Value::List(items))) => items.iter().map(|(p, v)| sql_text(*p, v)).collect::<Result<_>>()?,
                    Some((p, _)) => return err(p, "args: takes a list"),
                },
                body: n.str("body")?,
                language: n.name("language")?,
            };
            n.finish()?;
            if t.body.is_some() == t.function.is_some() {
                return err(b.pos, format!("{what}({}): give body: \"...\" or function: name (a `function` block)", t.name));
            }
            m.triggers.push(t);
        }
        "id" => return err(b.pos, format!("{what}: composite primary keys aren't supported yet")),
        other => {
            return err(
                b.pos,
                format!(
                    "{}: unknown attribute @@{other}; use @@map, @@index, @@unique, @@check, @@exclude, @@trigger, \
                     @@comment or @@renamed_from",
                    m.name
                ),
            )
        }
    }
    Ok(())
}

/// A storage parameter value as SQL.
fn param(pos: Pos, v: &Value) -> Result<String> {
    Ok(match v {
        Value::Str(s) => format!("'{}'", s.replace('\'', "''")),
        other => sql_text(pos, other)?,
    })
}
