//! Python module generation: `models.py` (runtime) and `models.pyi` (static types).
//!
//! The runtime module embeds the compiled schema IR and builds the model classes from
//! it with `orm.define()`, so every schema detail (indexes, triggers, extension types)
//! reaches the engine without Python code describing it. The stub gives editors and
//! type checkers the per-model classes, relation paths, insert / update shapes and
//! query sets.

use std::collections::BTreeSet;
use std::fmt::Write;

use crate::ir::{ColType, EnumStorage, FieldIr, RelKind, SchemaIr};
use crate::schema::Schema;

pub struct Generated {
    pub module: String,
    pub stub: String,
}

const WIDTH: usize = 89;

fn section(name: &str) -> String {
    let head = format!("# -- {name} ");
    format!("{head}{}", "-".repeat(WIDTH.saturating_sub(head.len())))
}

fn field_class(f: &FieldIr) -> &'static str {
    if f.array {
        return "Array";
    }
    if f.enum_name.is_some() {
        return "Enum";
    }
    if f.db_type.is_some() && !f.hints.is_empty() {
        return "Field";
    }
    match f.ty {
        ColType::BigInt => "BigInt",
        ColType::Int => "Integer",
        ColType::Float => "Float",
        ColType::Bool => "Boolean",
        ColType::String => "String",
        ColType::Text => "Text",
        ColType::DateTime => "DateTime",
        ColType::Date => "Date",
        ColType::Uuid => "Uuid",
        ColType::Json => "Json",
        ColType::Decimal => "Decimal",
    }
}

fn base_type(f: &FieldIr) -> String {
    let t = element_type(f);
    if f.array {
        format!("list[{t}]")
    } else {
        t
    }
}

fn element_type(f: &FieldIr) -> String {
    if let Some(h) = f.hints.get("python") {
        return h.clone();
    }
    if let Some(e) = &f.enum_name {
        return e.clone();
    }
    match f.ty {
        ColType::BigInt | ColType::Int => "int",
        ColType::Float => "float",
        ColType::Bool => "bool",
        ColType::String | ColType::Text => "str",
        ColType::DateTime => "datetime",
        ColType::Date => "date",
        ColType::Uuid => "UUID",
        ColType::Json => "Any",
        ColType::Decimal => "Decimal",
    }
    .to_owned()
}

fn value_type(f: &FieldIr) -> String {
    let t = base_type(f);
    if f.nullable {
        format!("{t} | None")
    } else {
        t
    }
}

fn has_server_value(f: &FieldIr) -> bool {
    f.auto_increment || f.default.is_some() || f.default_now || f.default_sql.is_some()
}

/// Generates `models.py` / `models.pyi` for `ir` (already validated as `schema`).
/// `source` names the schema file in the header comment.
pub fn generate(ir: &SchemaIr, schema: &Schema, source: &str) -> Result<Generated, String> {
    let ir_json = super::embedded_schema_json(ir)?;
    if ir_json.contains("\"\"\"") {
        return Err("schema text contains \"\"\" which can't be embedded in the generated module".into());
    }
    let names: Vec<&str> = schema.models.iter().map(|m| m.ir.name.as_str()).collect();
    let enums: Vec<&str> = schema.enums.iter().map(|e| e.name.as_str()).collect();
    let mut exported: Vec<String> = enums.iter().map(|n| n.to_string()).collect();
    exported.extend(names.iter().map(|n| n.to_string()));
    exported.extend(names.iter().flat_map(|n| [format!("{n}Insert"), format!("{n}Update"), format!("{n}UpdateRow")]));
    exported.extend(names.iter().map(|n| format!("{n}QuerySet")));
    let all = exported.iter().map(|n| format!("    \"{n}\",\n")).collect::<String>();

    // -- models.py -------------------------------------------------------------------
    let mut py = String::new();
    writeln!(py, "# Generated from {source} by `python -m orm generate`. Do not edit.").unwrap();
    py.push_str(
        "#\n\
         # Runtime half of the generated module: the model classes are built from the compiled\n\
         # schema below. models.pyi carries the static types (columns, relation paths, typed\n\
         # inserts / updates and query sets) for editors and type checkers.\n\n\
         from orm import QuerySet, define\n\n",
    );
    writeln!(py, "_SCHEMA = r\"\"\"\n{ir_json}\n\"\"\"\n").unwrap();
    writeln!(py, "_models = define(_SCHEMA, module=__name__)").unwrap();
    for n in enums.iter().chain(&names) {
        writeln!(py, "{n} = _models[\"{n}\"]").unwrap();
    }
    py.push_str("\n# Typed per model in models.pyi; plain aliases at runtime so the names can be imported.\n");
    let qs: Vec<String> = names.iter().map(|n| format!("{n}QuerySet")).collect();
    if !qs.is_empty() { writeln!(py, "{} = QuerySet", qs.join(" = ")).unwrap(); }
    let dicts: Vec<String> =
        names.iter().flat_map(|n| [format!("{n}Insert"), format!("{n}Update"), format!("{n}UpdateRow")]).collect();
    if !dicts.is_empty() { writeln!(py, "{} = dict\n", dicts.join(" = ")).unwrap(); }
    writeln!(py, "__all__ = [\n{all}]").unwrap();

    // -- models.pyi ------------------------------------------------------------------
    let mut used: BTreeSet<&str> = BTreeSet::new();
    for m in &schema.models {
        for f in m.fields() {
            let t = base_type(f);
            for (word, import) in
                [("datetime", "datetime"), ("date", "date"), ("UUID", "UUID"), ("Any", "Any"), ("Decimal", "Decimal")]
            {
                if t.split(|c: char| !c.is_alphanumeric() && c != '_').any(|w| w == word) {
                    used.insert(import);
                }
            }
        }
    }
    let mut body = String::new();
    for e in &schema.enums {
        writeln!(body, "{}\n", section(&e.name)).unwrap();
        let base = if e.storage == EnumStorage::Int { "IntEnum" } else { "StrEnum" };
        used.insert(if e.storage == EnumStorage::Int { "IntEnum" } else { "StrEnum" });
        writeln!(body, "class {}({base}):", e.name).unwrap();
        for v in &e.values {
            writeln!(body, "    {} = {}", v.name, v.value).unwrap();
        }
        body.push('\n');
    }
    for m in &schema.models {
        let name = &m.ir.name;
        writeln!(body, "{}\n", section(name)).unwrap();
        writeln!(body, "class {name}(Model):").unwrap();
        for method in ir.behavior.methods.iter().filter(|method| method.model == *name) {
            let output = if method.output.is_some() { "str" } else { "None" };
            writeln!(body, "    @staticmethod\n    def {}(value: str) -> {output}: ...", method.name).unwrap();
        }
        for f in m.fields() {
            writeln!(body, "    {}: f.{}[{}]", f.name, field_class(f), value_type(f)).unwrap();
        }
        if !m.ir.relations.is_empty() {
            body.push('\n');
        }
        for r in &m.ir.relations {
            match r.kind {
                RelKind::Many if r.through.is_some() => {
                    writeln!(body, "    {}: f.ManyToMany[{}, _{}Path]", r.name, r.target, r.target).unwrap()
                }
                RelKind::Many => writeln!(body, "    {}: f.HasMany[{}, _{}Path]", r.name, r.target, r.target).unwrap(),
                RelKind::One if !r.foreign_key => {
                    writeln!(body, "    {}: f.HasOne[{} | None, _{}Path]", r.name, r.target, r.target).unwrap()
                }
                RelKind::One => {
                    let nullable = m.field(&r.from)?.nullable;
                    let t = if nullable { format!("{} | None", r.target) } else { r.target.clone() };
                    writeln!(body, "    {}: f.BelongsTo[{t}, _{}Path]", r.name, r.target).unwrap();
                }
            }
        }
        writeln!(body, "\n    objects: ClassVar[{name}QuerySet]\n").unwrap();
        writeln!(
            body,
            "    async def update(self, **values: Unpack[{name}Update]) -> None: ...  # type: ignore[override]\n"
        )
        .unwrap();

        writeln!(body, "class _{name}Path(RelationPath[{name}]):").unwrap();
        for f in m.fields() {
            writeln!(body, "    {}: ColumnRef[{}]", f.name, value_type(f)).unwrap();
        }
        for r in &m.ir.relations {
            writeln!(body, "    {}: _{}Path", r.name, r.target).unwrap();
        }
        body.push('\n');

        // insert / update shapes; a to-one relation can be given instead of its key
        let belongs =
            |field: &str| m.ir.relations.iter().find(|r| r.kind == RelKind::One && r.foreign_key && r.from == field);
        writeln!(body, "class {name}Insert(TypedDict):").unwrap();
        for (position, f) in m.fields().iter().enumerate() {
            #[cfg(feature = "composition")]
            if m.native.computed().contains(&position) { continue; }
            #[cfg(not(feature = "composition"))]
            let _ = position;
            let t = value_type(f);
            match belongs(&f.name) {
                Some(r) => {
                    let target = if f.nullable { format!("{} | None", r.target) } else { r.target.clone() };
                    if !f.nullable && !has_server_value(f) && !ir.behavior.proxy_models.iter().any(|p| p.model == m.ir.name && p.defaults.contains_key(&f.name)) {
                        writeln!(body, "    # One of {} / {} is required (checked at runtime).", f.name, r.name).unwrap();
                    }
                    writeln!(body, "    {}: NotRequired[{t}]", f.name).unwrap();
                    writeln!(body, "    {}: NotRequired[{target}]", r.name).unwrap();
                }
                None if f.nullable || has_server_value(f) || ir.behavior.proxy_models.iter().any(|p| p.model == m.ir.name && p.defaults.contains_key(&f.name)) => writeln!(body, "    {}: NotRequired[{t}]", f.name).unwrap(),
                None => writeln!(body, "    {}: {t}", f.name).unwrap(),
            }
        }
        writeln!(body, "\nclass {name}Update(TypedDict, total=False):").unwrap();
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
            if expression { writeln!(body, "    {}: {t} | Expression[{t}]", f.name).unwrap(); }
            else { writeln!(body, "    {}: {t}", f.name).unwrap(); }
            if let Some(r) = belongs(&f.name) {
                let target = if f.nullable { format!("{} | None", r.target) } else { r.target.clone() };
                writeln!(body, "    {}: {target}", r.name).unwrap();
            }
        }
        // update_many rows: the primary key plus plain values (no expressions)
        writeln!(body, "\nclass {name}UpdateRow(TypedDict, total=False):").unwrap();
        for (position, f) in m.fields().iter().enumerate() {
            #[cfg(feature = "composition")]
            if m.native.computed().contains(&position) { continue; }
            #[cfg(not(feature = "composition"))]
            let _ = position;
            let t = value_type(f);
            if f.primary_key {
                writeln!(body, "    {}: Required[{t}]", f.name).unwrap();
            } else {
                writeln!(body, "    {}: {t}", f.name).unwrap();
            }
            if let Some(r) = belongs(&f.name) {
                let target = if f.nullable { format!("{} | None", r.target) } else { r.target.clone() };
                writeln!(body, "    {}: {target}", r.name).unwrap();
            }
        }
        writeln!(body, "\nclass {name}QuerySet(QuerySet[{name}]):").unwrap();
        writeln!(
            body,
            "    def insert(self, **values: Unpack[{name}Insert]) -> InsertOne[{name}]: ...  # type: ignore[override]"
        )
        .unwrap();
        writeln!(
            body,
            "    def insert_many(self, rows: Iterable[{name}Insert]) -> InsertMany[{name}]: ...  # type: ignore[override]"
        )
        .unwrap();
        writeln!(
            body,
            "    def update(self, **values: Unpack[{name}Update]) -> Update[{name}]: ...  # type: ignore[override]"
        )
        .unwrap();
        writeln!(
            body,
            "    def update_many(self, rows: Iterable[{name}UpdateRow], *, batch_size: int | None = None) \
             -> UpdateMany[{name}]: ...  # type: ignore[override]\n"
        )
        .unwrap();
    }

    let mut pyi = String::new();
    writeln!(pyi, "# Generated from {source} by `python -m orm generate`. Do not edit.").unwrap();
    pyi.push_str(
        "#\n\
         # Static half of the generated module. Each model gets:\n\
         #   * the model class: column descriptors (`User.email` is a ColumnRef[str] on the class,\n\
         #     a read-only str on an instance), relation descriptors and typed `update()`;\n\
         #   * a path class (`_UserPath`): what a relation to the model evaluates to on the class\n\
         #     side, so `User.posts.created_at` autocompletes and type-checks as ColumnRef[datetime];\n\
         #   * `UserInsert` / `UserUpdate` / `UserUpdateRow` TypedDicts: the row shapes accepted by\n\
         #     insert / update / update_many;\n\
         #   * a query set class (`UserQuerySet`): typed `insert()`, `insert_many()`, `update()`,\n\
         #     `update_many()`.\n\n\
         from collections.abc import Iterable\n",
    );
    let dt: Vec<&str> = ["date", "datetime"].into_iter().filter(|d| used.contains(d)).collect();
    if !dt.is_empty() {
        writeln!(pyi, "from datetime import {}", dt.join(", ")).unwrap();
    }
    if used.contains("Decimal") {
        pyi.push_str("from decimal import Decimal\n");
    }
    let en: Vec<&str> = ["IntEnum", "StrEnum"].into_iter().filter(|d| used.contains(d)).collect();
    if !en.is_empty() {
        writeln!(pyi, "from enum import {}", en.join(", ")).unwrap();
    }
    let typing = if used.contains("Any") {
        "Any, ClassVar, NotRequired, Required, TypedDict"
    } else {
        "ClassVar, NotRequired, Required, TypedDict"
    };
    writeln!(pyi, "from typing import {typing}").unwrap();
    if schema.models.iter().flat_map(|m| m.fields()).any(|f| f.hints.get("python").is_some_and(|h| h.starts_with("Literal["))) {
        pyi.push_str("from typing import Literal\n");
    }
    if used.contains("UUID") {
        pyi.push_str("from uuid import UUID\n");
    }

    pyi.push_str(
        "\nfrom typing_extensions import Unpack\n\n\
         from orm import ColumnRef, Expression, InsertMany, InsertOne, Model, QuerySet, RelationPath, Update, UpdateMany\n\
         from orm import fields as f\n\n",
    );
    pyi.push_str(&body);
    writeln!(pyi, "__all__ = [\n{all}]").unwrap();
    Ok(Generated { module: py, stub: pyi })
}

/// A module exposing only declarations owned by one source file. All modules load
/// the same generated runtime by path, so even circular relations share classes.
pub fn facade(unit: &crate::dsl::SchemaUnit, shared_path: &str, stub_import: &str) -> Generated {
    let mut names = unit.enums.clone();
    for name in &unit.models {
        names.extend([name.clone(), format!("{name}Insert"), format!("{name}Update"), format!("{name}UpdateRow"), format!("{name}QuerySet")]);
    }
    let path = serde_json::to_string(shared_path).unwrap();
    let exports = serde_json::to_string(&names).unwrap();
    let mut module = format!(r#"# Generated schema module. Do not edit.
import importlib.util as _util
from pathlib import Path as _Path
import sys as _sys

_path = (_Path(__file__).parent / {path}).resolve()
_key = '_orm_schema:' + str(_path)
if _key not in _sys.modules:
    _spec = _util.spec_from_file_location(_key, _path)
    _module = _util.module_from_spec(_spec)
    _sys.modules[_key] = _module
    try:
        _spec.loader.exec_module(_module)
    except BaseException:
        del _sys.modules[_key]
        raise
_shared = _sys.modules[_key]

__all__ = {exports}
"#);
    let mut stub = String::from("# Generated schema module. Do not edit.\n");
    for name in &names {
        writeln!(module, "{name} = _shared.{name}").unwrap();
        writeln!(stub, "from {stub_import} import {name} as {name}").unwrap();
    }
    writeln!(stub, "\n__all__ = {exports}").unwrap();
    Generated { module, stub }
}
