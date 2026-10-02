//! Syntax tree -> schema IR.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::syntax::{err, Args, Attr, Item, Member, ModelDecl, Pos, Result, TypeRef, Value};
use crate::ext::{ExtensionDef, TypeDef};
use crate::ir::{
    ColType, ConstraintIr, Deferrable, ExcludeElementIr, ExtensionIr, FieldIr, ForEach, FunctionIr, IndexColumnIr,
    IndexIr, ModelIr, Nulls, OnDelete, RelKind, RelationIr, SchemaIr, TriggerEvent, TriggerIr, TriggerTiming,
};

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

    /// A bare name or a string.
    fn name(&mut self, key: &'a str) -> Result<Option<String>> {
        match self.get(key) {
            None => Ok(None),
            Some((p, v)) => Ok(Some(name_of(p, v, &format!("{}: {key}", self.what))?)),
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
        Value::Object(entries) => serde_json::Value::Object(
            entries.iter().map(|(k, p, v)| Ok((k.clone(), json_of(*p, v)?))).collect::<Result<_>>()?,
        ),
        Value::Path(p, _) if p.len() == 1 && p[0] == "null" => serde_json::Value::Null,
        Value::Path(..) => return err(pos, "expected a literal"),
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
        "cascade" => OnDelete::Cascade,
        "set_null" => OnDelete::SetNull,
        "set_default" => OnDelete::SetDefault,
        "restrict" => OnDelete::Restrict,
        "no_action" => OnDelete::NoAction,
        _ => return err(pos, format!("unknown action {s}; use cascade, set_null, set_default, restrict or no_action")),
    })
}

fn builtin_type(name: &str) -> Option<ColType> {
    Some(match name {
        "BigInt" => ColType::BigInt,
        "Int" => ColType::Int,
        "Float" => ColType::Float,
        "Bool" => ColType::Bool,
        "String" => ColType::String,
        "Text" => ColType::Text,
        "DateTime" => ColType::DateTime,
        "Date" => ColType::Date,
        "Uuid" => ColType::Uuid,
        "Json" => ColType::Json,
        _ => return None,
    })
}

const FIELD_ATTRS: &str = "@primary, @auto, @unique, @index, @default, @check, @comment, @column, @renamed_from, @db_type";

pub struct Lowering<'l> {
    /// Reads an imported file (path as written in the schema).
    pub load: &'l dyn Fn(&str) -> std::result::Result<String, String>,
}

struct Ctx {
    types: HashMap<String, (String, TypeDef)>,
}

impl Lowering<'_> {
    pub fn lower(&self, items: Vec<Item>) -> Result<SchemaIr> {
        let mut ir = SchemaIr::default();
        let mut ctx = Ctx { types: HashMap::new() };
        for def in ExtensionDef::builtin() {
            for (name, t) in &def.types {
                ctx.types.insert(name.clone(), (def.name.clone(), t.clone()));
            }
        }
        // imports first: their types are usable anywhere in the file
        for item in &items {
            if let Item::Import { pos, path } = item {
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
            match item {
                Item::Extension { pos, name, args } => {
                    let mut n = Named::new(format!("extension {name}"), args);
                    n.no_positional()?;
                    let schema = n.str("schema")?;
                    let version = n.str("version")?;
                    n.finish()?;
                    if ir.extensions.iter().any(|e| e.name == *name) {
                        return err(*pos, format!("extension {name} is declared twice"));
                    }
                    let provides = ir.catalog.iter().find(|e| e.name == *name).map(|e| e.provides.clone()).unwrap_or_default();
                    ir.extensions.push(ExtensionIr { name: name.clone(), schema, version, provides });
                }
                Item::Function { pos, name, props } => ir.functions.push(function(*pos, name, props)?),
                Item::Model(_) | Item::Import { .. } => {}
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
            let pk = pks[&m.name].clone();
            let Some(pk) = pk else { return err(decl.pos, format!("model {} has no @primary field", decl.name)) };
            for member in decl.members.iter().filter(|mm| model_names.contains_key(mm.ty.name.as_str())) {
                let r = relation(member, &m.fields, &pk, &model_names, &pks)?;
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

fn function(pos: Pos, name: &str, props: &[(String, Pos, Value)]) -> Result<FunctionIr> {
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
    for a in &m.attrs {
        let text = |a: &Attr| -> Result<String> {
            match a.args.positional.as_slice() {
                [(_, Value::Str(s))] if a.args.named.is_empty() => Ok(s.clone()),
                _ => err(a.pos, format!("@{}(\"...\") takes one string", a.name)),
            }
        };
        match a.name.as_str() {
            "table" => ir.table = text(a)?,
            "comment" => ir.comment = Some(text(a)?),
            "renamed_from" => ir.renamed_from = Some(text(a)?),
            other => return err(a.pos, format!("unknown model attribute @{other}; use @table, @comment or @renamed_from")),
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
    if *list {
        return err(*pos, format!("{what}: `[]` is only for relations to models; array columns aren't supported yet"));
    }
    let mut f = FieldIr {
        name: member.name.clone(),
        column: member.name.clone(),
        ty: ColType::Text,
        nullable: *optional,
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
    if let Some(ty) = builtin_type(name) {
        f.ty = ty;
        match (ty, args.positional.as_slice()) {
            (_, []) => {}
            (ColType::String, [(_, Value::Num(n))]) => {
                f.max_length = Some(n.parse().map_err(|_| super::syntax::Error { pos: *pos, msg: format!("{what}: String(n) needs a positive integer") })?)
            }
            _ => return err(args.pos, format!("{what}: {name} takes no arguments")),
        }
        if !args.named.is_empty() {
            return err(args.pos, format!("{what}: {name} takes no named arguments"));
        }
    } else if let Some((ext, def)) = ctx.types.get(name) {
        let values = args.positional.iter().map(|(p, v)| sql_text(*p, v)).collect::<Result<Vec<_>>>()?;
        if !args.named.is_empty() {
            return err(args.pos, format!("{what}: extension types take positional arguments"));
        }
        let r = def.resolve(ext, name, &values).map_err(|e| super::syntax::Error { pos: *pos, msg: format!("{what}: {e}") })?;
        f.ty = r.value;
        f.db_type = Some(r.db_type);
        f.read_sql = r.read_sql;
        f.write_sql = r.write_sql;
        f.hints = r.hints;
        f.requires = vec![r.extension];
    } else {
        return err(
            *pos,
            format!(
                "{what}: unknown type {name}; expected a built-in type (BigInt, Int, Float, Bool, String, Text, \
                 DateTime, Date, Uuid, Json), a model, or a type from an extension file"
            ),
        );
    }

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
        match a.name.as_str() {
            "primary" => f.primary_key = flag(a)?,
            "auto" => f.auto_increment = flag(a)?,
            "unique" => f.unique = flag(a)?,
            "index" => f.index = flag(a)?,
            "check" => f.check = Some(text(a)?),
            "comment" => f.comment = Some(text(a)?),
            "column" => f.column = text(a)?,
            "renamed_from" => f.renamed_from = Some(text(a)?),
            "db_type" => f.db_type = Some(text(a)?),
            "default" => match one(a, &what)? {
                Value::Path(p, None) if p.len() == 1 && p[0] == "now" => f.default_now = true,
                Value::Path(p, Some(args)) if p.len() == 1 && p[0] == "sql" => match args.positional.as_slice() {
                    [(_, Value::Str(s))] if args.named.is_empty() => f.default_sql = Some(s.clone()),
                    _ => return err(a.pos, format!("{what}: sql(\"...\") takes one string")),
                },
                Value::Path(..) => {
                    return err(a.pos, format!("{what}: @default takes a literal, now or sql(\"...\")"));
                }
                v => f.default = Some(json_of(a.pos, v)?),
            },
            other => return err(a.pos, format!("{what}: unknown attribute @{other}; use {FIELD_ATTRS}")),
        }
    }
    if f.primary_key && f.nullable {
        return err(member.pos, format!("{what}: a primary key can't be optional"));
    }
    Ok(f)
}

fn relation(
    member: &Member,
    fields: &[FieldIr],
    pk: &str,
    models: &HashMap<&str, &ModelDecl>,
    pks: &HashMap<String, Option<String>>,
) -> Result<RelationIr> {
    let target = &member.ty.name;
    let what = format!("relation {}", member.name);
    let rel = member.attrs.iter().find(|a| a.name == "relation");
    if let Some(a) = member.attrs.iter().find(|a| a.name != "relation") {
        return err(a.pos, format!("{what}: relations take only @relation(...)"));
    }
    let Some(rel) = rel else {
        return err(
            member.pos,
            format!("{what}: say how it joins with @relation(via: ...): a field of this model for `{target}`, `{target}.field` for `{target}[]`"),
        );
    };
    let mut n = Named::new(what.clone(), &rel.args);
    n.no_positional()?;
    let (vpos, via) = match n.get("via") {
        Some((p, Value::Path(path, None))) => (p, path.clone()),
        Some((p, _)) => return err(p, format!("{what}: via must name a field")),
        None => return err(rel.pos, format!("{what}: @relation needs via:")),
    };
    let target_fields: Vec<String> = models[target.as_str()]
        .members
        .iter()
        .filter(|m| !models.contains_key(m.ty.name.as_str()))
        .map(|m| m.name.clone())
        .collect();
    if member.ty.list {
        if member.ty.optional {
            return err(member.ty.pos, format!("{what}: a to-many relation can't be optional"));
        }
        let to = match via.as_slice() {
            [t, f] if t == target => f.clone(),
            [f] => f.clone(),
            _ => return err(vpos, format!("{what}: via must be {target}.<field>")),
        };
        if !target_fields.contains(&to) {
            return err(vpos, format!("{what}: {target} has no field {to}"));
        }
        let from = n.name("from")?.unwrap_or_else(|| pk.to_owned());
        n.finish()?;
        return Ok(RelationIr {
            name: member.name.clone(),
            kind: RelKind::Many,
            target: target.clone(),
            from,
            to,
            foreign_key: false,
            on_delete: None,
            on_update: None,
            deferrable: None,
        });
    }
    let [from] = via.as_slice() else {
        return err(vpos, format!("{what}: via must name a field of this model"));
    };
    let Some(local) = fields.iter().find(|f| f.name == *from) else {
        return err(vpos, format!("{what}: no field {from} in this model"));
    };
    if local.nullable != member.ty.optional {
        let hint = if local.nullable { format!("{target}?") } else { target.clone() };
        return err(member.ty.pos, format!("{what}: {from} is {}nullable, so the relation type is `{hint}`", if local.nullable { "" } else { "not " }));
    }
    let to = match n.name("to")? {
        Some(t) => {
            if !target_fields.contains(&t) {
                return err(rel.pos, format!("{what}: {target} has no field {t}"));
            }
            t
        }
        None => pks[target.as_str()].clone().unwrap_or_default(),
    };
    let on_delete = match n.get("on_delete") {
        Some((p, v)) => Some(action(p, &name_of(p, v, &what)?)?),
        None => None,
    };
    let on_update = match n.get("on_update") {
        Some((p, v)) => Some(action(p, &name_of(p, v, &what)?)?),
        None => None,
    };
    let deferrable = deferrable(&mut n)?;
    n.finish()?;
    Ok(RelationIr {
        name: member.name.clone(),
        kind: RelKind::One,
        target: target.clone(),
        from: from.clone(),
        to,
        foreign_key: true,
        on_delete,
        on_update,
        deferrable,
    })
}

/// `[field, field(sort: desc, ops: x), sql("expr", ...)]` -> index keys (and the
/// exclusion operator of each, when `with_op`).
fn keys(pos: Pos, v: Option<&Value>, what: &str, with_op: bool) -> Result<Vec<(IndexColumnIr, Option<String>)>> {
    let Some(Value::List(items)) = v else {
        return err(pos, format!("{what}: the first argument is a list of keys, e.g. [author_id, created_at(sort: desc)]"));
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
            None | Some("asc") => false,
            Some("desc") => true,
            Some(o) => return err(*p, format!("sort must be asc or desc, not {o}")),
        };
        let nulls = match n.name("nulls")?.as_deref() {
            None => None,
            Some("first") => Some(Nulls::First),
            Some("last") => Some(Nulls::Last),
            Some(o) => return err(*p, format!("nulls must be first or last, not {o}")),
        };
        let opclass = n.name("ops")?;
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
        "index" => {
            extra_positional(1)?;
            let columns = keys(b.pos, first, &what, false)?.into_iter().map(|(k, _)| k).collect();
            let mut n = Named::new(what.clone(), &b.args);
            let method = match (n.name("type")?, n.name("using")?) {
                (Some(_), Some(_)) => return err(b.pos, format!("{what}: give type: or using:, not both")),
                (a, b) => a.or(b),
            };
            let with = match n.get("with") {
                None => vec![],
                Some((_, Value::Object(entries))) => {
                    entries.iter().map(|(k, p, v)| Ok((k.clone(), param(*p, v)?))).collect::<Result<_>>()?
                }
                Some((p, _)) => return err(p, format!("{what}: with: takes {{ name: value, ... }}")),
            };
            let ix = IndexIr {
                name: n.str("name")?,
                columns,
                unique: n.bool("unique")?,
                method,
                where_: n.str("where")?,
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
                name: n.str("name")?,
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
            let method = n.name("type")?.or(n.name("using")?).unwrap_or_else(|| "gist".into());
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
                where_: n.str("where")?,
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
                when: n.str("when")?,
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
                return err(b.pos, format!("{what}({}): give body: \"\"\"...\"\"\" or function: name", t.name));
            }
            m.triggers.push(t);
        }
        other => {
            return err(b.pos, format!("{}: unknown declaration @@{other}; use @@index, @@unique, @@check, @@exclude or @@trigger", m.name))
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
