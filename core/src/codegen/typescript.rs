//! TypeScript module generation: one `models.ts` with the runtime models and their
//! static types.
//!
//! The module embeds the compiled schema IR and builds the models from it with the
//! runtime's `define()`, like the Python module. Per model it declares:
//!
//! * `User` (instance type): its columns as read-only properties (camelCase), its to-many
//!   relations as query sets (`posts: RelatedSet<PostSpec, ...>`) and `update()` /
//!   `delete()` / `refresh()`. To-one relations are absent: a query that loads them
//!   (`selectRelated` / `prefetchRelated`) adds them to its rows' type;
//! * `UserInsert` / `UserUpdate` / `UserUpdateRow`: what `insert()`, `update()` and
//!   `updateMany()` take (a required foreign key is "the key or the related row");
//! * `UserFields` / `UserPath`: the columns and relation paths, so `User.posts.createdAt`
//!   is a `Column<Date, "User" | "*many">`;
//! * `UserSpec` tying them together, and the model object `User` (`UserModel`).
//!
//! Field and relation names are camelCase (`author_id` -> `authorId`), the same function
//! as the runtime's `camel()`.

use std::collections::BTreeSet;
use std::fmt::Write;

use crate::ir::{ColType, EnumStorage, FieldIr, RelKind, RelationIr, SchemaIr};
use crate::schema::{Model, Schema};

const WIDTH: usize = 89;

fn section(name: &str) -> String {
    let head = format!("// -- {name} ");
    format!("{head}{}", "-".repeat(WIDTH.saturating_sub(head.len())))
}

/// `author_id` -> `authorId`: an underscore after a letter or digit is dropped and the
/// next letter or digit upper-cased (leading and trailing underscores stay).
pub fn camel(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '_' && i > 0 && chars[i - 1].is_ascii_alphanumeric() {
            let mut j = i;
            while j < chars.len() && chars[j] == '_' {
                j += 1;
            }
            if j < chars.len() && chars[j].is_ascii_alphanumeric() {
                out.extend(chars[j].to_uppercase());
                i = j + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Names the runtime gives meaning to on models and instances.
const RESERVED: [&str; 12] =
    ["objects", "_meta", "DoesNotExist", "MultipleObjectsReturned", "pk", "update", "delete", "refresh", "toJSON", "constructor", "toString", "then"];

fn element_type(f: &FieldIr) -> String {
    if let Some(h) = f.hints.get("typescript") {
        return h.clone();
    }
    if let Some(e) = &f.enum_name {
        return e.clone();
    }
    match f.ty {
        ColType::BigInt => "bigint",
        ColType::Int | ColType::Float => "number",
        ColType::Bool => "boolean",
        ColType::String | ColType::Text | ColType::Uuid => "string",
        ColType::DateTime | ColType::Date => "Date",
        ColType::Json => "JsonValue",
        ColType::Decimal => "Decimal",
    }
    .to_owned()
}

/// The type a column reads as.
fn value_type(f: &FieldIr) -> String {
    let t = element_type(f);
    let t = if f.array && !f.hints.contains_key("typescript") { format!("{t}[]") } else { t };
    if f.nullable {
        format!("{t} | null")
    } else {
        t
    }
}

/// The type a column is written from (`bigint` columns take numbers, ...).
fn input_type(f: &FieldIr) -> String {
    format!("In<{}>", value_type(f))
}

fn has_server_value(f: &FieldIr) -> bool {
    f.auto_increment || f.default.is_some() || f.default_now || f.default_sql.is_some()
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).expect("a string serializes")
}

/// The to-one relation of `m` whose key is field `field` (a foreign key on `m`).
fn belongs_to<'a>(m: &'a Model, field: &str) -> Option<&'a RelationIr> {
    m.ir.relations.iter().find(|r| r.kind == RelKind::One && r.foreign_key && r.from == field)
}

/// The hop kind of a relation, for loaded-relation types.
fn hop_kind(m: &Model, r: &RelationIr) -> Result<&'static str, String> {
    Ok(match r.kind {
        RelKind::Many if r.through.is_some() => "m2m",
        RelKind::Many => "many",
        RelKind::One if !r.foreign_key => "opt",
        RelKind::One if m.field(&r.from)?.nullable => "opt",
        RelKind::One => "one",
    })
}

/// A to-one relation as a value: the related row (any object with its key).
fn related_ref(schema: &Schema, r: &RelationIr) -> Result<String, String> {
    let target = schema.model(schema.model_idx(&r.target)?);
    Ok(format!("{{ readonly {}: {} }}", camel(&r.to), input_type(target.field(&r.to)?)))
}

/// Generates `models.ts` for `ir` (already validated as `schema`). `source` names the
/// schema file in the header comment; `runtime` is the module the runtime is imported
/// from (`"orm"`).
pub fn generate(ir: &SchemaIr, schema: &Schema, source: &str, runtime: &str) -> Result<String, String> {
    // names: camelCase must stay unique and clear of the runtime's own
    for m in &schema.models {
        let mut seen = BTreeSet::new();
        let names = m.fields().iter().map(|f| &f.name).chain(m.ir.relations.iter().map(|r| &r.name));
        for n in names {
            let c = camel(n);
            if RESERVED.contains(&c.as_str()) {
                return Err(format!("{}.{n}: the name {c} is reserved in TypeScript models; rename it", m.ir.name));
            }
            if !seen.insert(c.clone()) {
                return Err(format!("{} has two members named {c} in TypeScript (camelCase)", m.ir.name));
            }
        }
    }
    let ir_json = super::embedded_schema_json(ir)?;
    let mut used: BTreeSet<&str> = BTreeSet::new();
    let mut body = String::new();

    for e in &schema.enums {
        writeln!(body, "{}\n", section(&e.name)).unwrap();
        writeln!(body, "export const {} = {{", e.name).unwrap();
        for v in &e.values {
            let value = match e.storage {
                EnumStorage::Int => v.value.to_string(),
                _ => quote(v.value.as_str().unwrap_or(&v.name)),
            };
            writeln!(body, "  {}: {value},", v.name).unwrap();
        }
        writeln!(body, "}} as const;").unwrap();
        writeln!(body, "export type {0} = (typeof {0})[keyof typeof {0}];\n", e.name).unwrap();
    }

    for m in &schema.models {
        let name = &m.ir.name;
        let pk = m.pk_field();
        writeln!(body, "{}\n", section(name)).unwrap();
        for f in m.fields() {
            let t = value_type(f);
            for (word, import) in [("Decimal", "Decimal"), ("JsonValue", "JsonValue")] {
                if t.split(|c: char| !c.is_alphanumeric()).any(|w| w == word) {
                    used.insert(import);
                }
            }
        }

        // the column values
        writeln!(body, "/** The column values of a {name} row. */").unwrap();
        writeln!(body, "export interface {name}Data {{").unwrap();
        for f in m.fields() {
            writeln!(body, "  readonly {}: {};", camel(&f.name), value_type(f)).unwrap();
        }
        writeln!(body, "}}\n").unwrap();

        // the instance
        writeln!(
            body,
            "/** A {name} row. To-one relations are typed on rows of queries that load them. */"
        )
        .unwrap();
        writeln!(body, "export interface {name} extends {name}Data, Instance<{name}Spec> {{").unwrap();
        for r in &m.ir.relations {
            match r.kind {
                RelKind::Many if r.through.is_some() => {
                    writeln!(body, "  readonly {}: ManyRelatedSet<{}Spec>;", camel(&r.name), r.target).unwrap()
                }
                RelKind::Many => {
                    // the key and the to-one relation back, filled in by insert()
                    let target = schema.model(schema.model_idx(&r.target)?);
                    let mut link = vec![quote(&camel(&r.to))];
                    if let Some(back) = target
                        .ir
                        .relations
                        .iter()
                        .find(|b| b.kind == RelKind::One && b.foreign_key && b.from == r.to && b.target == *name)
                    {
                        link.push(quote(&camel(&back.name)));
                    }
                    writeln!(body, "  readonly {}: RelatedSet<{}Spec, {}>;", camel(&r.name), r.target, link.join(" | ")).unwrap()
                }
                RelKind::One => {}
            }
        }
        writeln!(body, "}}\n").unwrap();

        // insert: optional when the database fills it in; a required foreign key is the
        // key or the related row, exactly one of them
        writeln!(body, "export type {name}Insert = {{").unwrap();
        let mut one_of = vec![];
        for (position, f) in m.fields().iter().enumerate() {
            #[cfg(feature = "composition")]
            if m.native.computed().contains(&position) { continue; }
            #[cfg(not(feature = "composition"))]
            let _ = position;
            let optional = f.nullable || has_server_value(f);
            match belongs_to(m, &f.name) {
                Some(r) if !optional => one_of.push((f, r)),
                Some(r) => {
                    let null = if f.nullable { " | null" } else { "" };
                    writeln!(body, "  {}?: {};", camel(&f.name), input_type(f)).unwrap();
                    writeln!(body, "  {}?: {}{null};", camel(&r.name), related_ref(schema, r)?).unwrap();
                }
                None => {
                    let q = if optional { "?" } else { "" };
                    writeln!(body, "  {}{q}: {};", camel(&f.name), input_type(f)).unwrap();
                }
            }
        }
        write!(body, "}}").unwrap();
        for (f, r) in one_of {
            let (key, rel) = (camel(&f.name), camel(&r.name));
            write!(
                body,
                " & (\n  | {{ {key}: {}; {rel}?: never }}\n  | {{ {rel}: {}; {key}?: never }}\n)",
                input_type(f),
                related_ref(schema, r)?
            )
            .unwrap();
        }
        writeln!(body, ";\n").unwrap();

        // update: plain values or expressions over the model
        writeln!(body, "export interface {name}Update {{").unwrap();
        for (position, f) in m.fields().iter().enumerate() {
            #[cfg(feature = "composition")]
            if m.native.computed().contains(&position) { continue; }
            #[cfg(not(feature = "composition"))]
            let _ = position;
            let t = value_type(f);
            #[cfg(feature = "composition")]
            let expression = !m.native.validated().contains(&position);
            #[cfg(not(feature = "composition"))]
            let expression = true;
            if expression {
                writeln!(body, "  {}?: {} | Expression<Compat<{t}>, \"{name}\" | \"~{name}\", {{}}>;", camel(&f.name), input_type(f)).unwrap();
            } else { writeln!(body, "  {}?: {};", camel(&f.name), input_type(f)).unwrap(); }
            if let Some(r) = belongs_to(m, &f.name) {
                let null = if f.nullable { " | null" } else { "" };
                writeln!(body, "  {}?: {}{null};", camel(&r.name), related_ref(schema, r)?).unwrap();
            }
        }
        writeln!(body, "}}\n").unwrap();

        // updateMany rows: the primary key plus plain values
        writeln!(body, "export interface {name}UpdateRow {{").unwrap();
        for (position, f) in m.fields().iter().enumerate() {
            #[cfg(feature = "composition")]
            if m.native.computed().contains(&position) { continue; }
            #[cfg(not(feature = "composition"))]
            let _ = position;
            let q = if f.primary_key { "" } else { "?" };
            writeln!(body, "  {}{q}: {};", camel(&f.name), input_type(f)).unwrap();
            if let Some(r) = belongs_to(m, &f.name) {
                let null = if f.nullable { " | null" } else { "" };
                writeln!(body, "  {}?: {}{null};", camel(&r.name), related_ref(schema, r)?).unwrap();
            }
        }
        writeln!(body, "}}\n").unwrap();

        writeln!(body, "export interface {name}Spec {{").unwrap();
        writeln!(body, "  readonly name: {};", quote(name)).unwrap();
        writeln!(body, "  readonly row: {name};").unwrap();
        writeln!(body, "  readonly data: {name}Data;").unwrap();
        writeln!(body, "  readonly insert: {name}Insert;").unwrap();
        writeln!(body, "  readonly update: {name}Update;").unwrap();
        writeln!(body, "  readonly updateRow: {name}UpdateRow;").unwrap();
        writeln!(body, "  readonly pk: {};", value_type(pk)).unwrap();
        writeln!(body, "}}\n").unwrap();

        // columns and relation paths: `S` the scopes a column reads (the root model, plus
        // `*many` through a to-many hop), `H` the hops so far, `O` whether a hop may be null
        writeln!(
            body,
            "export interface {name}Fields<S extends string, H extends readonly Hop[], O extends boolean> {{"
        )
        .unwrap();
        for f in m.fields() {
            writeln!(body, "  readonly {}: Column<O extends true ? {} | null : {}, S>;", camel(&f.name), value_type(f), value_type(f))
                .unwrap();
        }
        for r in &m.ir.relations {
            let kind = hop_kind(m, r)?;
            let scope = if matches!(kind, "many" | "m2m") { "S | Many" } else { "S" };
            let opt = if kind == "opt" { "true" } else { "O" };
            writeln!(
                body,
                "  readonly {}: {}Path<{scope}, [...H, Hop<{}, {}, {}Spec>], {opt}>;",
                camel(&r.name),
                r.target,
                quote(&camel(&r.name)),
                quote(kind),
                r.target
            )
            .unwrap();
        }
        writeln!(body, "}}\n").unwrap();
        writeln!(
            body,
            "export interface {name}Path<S extends string, H extends readonly Hop[], O extends boolean>\n  \
             extends RelationPath<{name}Spec, S, H>,\n    {name}Fields<S, H, O> {{}}\n"
        )
        .unwrap();
        let methods: Vec<_> = ir.behavior.methods.iter().filter(|method| method.model == *name).collect();
        if methods.is_empty() {
            writeln!(body, "export interface {name}Model extends ModelClass<{name}Spec>, {name}Fields<{}, [], false> {{}}\n", quote(name)).unwrap();
        } else {
            writeln!(body, "export interface {name}Model extends ModelClass<{name}Spec>, {name}Fields<{}, [], false> {{", quote(name)).unwrap();
            for method in methods {
                let output = if method.output.is_some() { "string" } else { "void" };
                writeln!(body, "  {}(value: string): {output};", camel(&method.name)).unwrap();
            }
            writeln!(body, "}}\n").unwrap();
        }
        writeln!(body, "export const {name} = models[{}] as unknown as {name}Model;\n", quote(name)).unwrap();
    }

    let mut out = String::new();
    writeln!(out, "// Generated from {source} by `orm generate typescript`. Do not edit.").unwrap();
    out.push_str(
        "//\n\
         // The models are built at runtime from the compiled schema below; the declarations\n\
         // give them their static types. Per model: the row type (`User`), the shapes insert /\n\
         // update / updateMany take, the columns and relation paths, and the model object.\n\n\
         /* eslint-disable */\n",
    );
    let mut types = vec![
        "Column", "Compat", "Expression", "Hop", "In", "Instance", "ManyRelatedSet", "Many", "ModelClass", "RelatedSet",
        "RelationPath", "SchemaIR",
    ];
    types.extend(used.iter().copied());
    types.sort_unstable();
    let imports = types.iter().map(|t| format!("type {t}")).collect::<Vec<_>>().join(", ");
    writeln!(out, "import {{ define, {imports} }} from {};\n", quote(runtime)).unwrap();
    writeln!(out, "const SCHEMA: SchemaIR = {ir_json};\n").unwrap();
    writeln!(out, "const models = define(SCHEMA);\n").unwrap();
    out.push_str(&body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::camel;

    #[test]
    fn camel_case_names() {
        assert_eq!(camel("author_id"), "authorId");
        assert_eq!(camel("post_tags"), "postTags");
        assert_eq!(camel("id"), "id");
        assert_eq!(camel("_private"), "_private");
        assert_eq!(camel("a__b"), "aB");
        assert_eq!(camel("trailing_"), "trailing_");
        assert_eq!(camel("v2_x"), "v2X");
        assert_eq!(camel("alreadyCamel"), "alreadyCamel");
    }
}
