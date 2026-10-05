//! The schema language (`.prisma` files): Prisma's schema syntax, plus our own
//! attributes and blocks for what Prisma lacks.
//!
//! ```text
//! import "extensions/acme.toml"          // extra extension definitions (see ext.rs)
//!
//! datasource db {
//!   provider   = "postgresql"
//!   extensions = [postgis(version: "3.4")] // pin an extension; used ones are added anyway
//! }
//!
//! model User {
//!   id         BigInt   @id @default(autoincrement())
//!   email      String   @unique @db.Citext   // extension type
//!   created_at DateTime @default(now())
//!   posts      Post[]
//!
//!   @@index([created_at(sort: Desc)], where: raw("email IS NOT NULL"))
//!   @@map("users")
//! }
//! ```
//!
//! [`compile`] turns a file into the [`SchemaIr`] every binding consumes. The full
//! reference is `docs/schema.md`; `docs/prisma-syntax.md` has the design.

mod lower;
mod syntax;

use std::path::{Path, PathBuf};

use crate::ir::SchemaIr;
use crate::schema::Schema;

pub use syntax::Pos;

/// Compiles schema source. `origin` is the file it came from: error messages name it
/// and `import` paths resolve against its directory.
pub fn compile(source: &str, origin: Option<&Path>) -> Result<SchemaIr, String> {
    compile_project(source, origin).map(|project| project.ir)
}

/// Source ownership for generated modules. Model and enum names remain global.
pub struct SchemaUnit {
    pub path: PathBuf,
    pub models: Vec<String>,
    pub enums: Vec<String>,
}

pub struct CompiledProject {
    pub ir: SchemaIr,
    pub units: Vec<SchemaUnit>,
    /// Source files and database catalog resources read by compilation.
    pub inputs: Vec<PathBuf>,
}

struct Loader {
    units: Vec<SchemaUnit>,
    seen: std::collections::HashSet<PathBuf>,
    active: Vec<PathBuf>,
    locations: Vec<(u32, u32, String)>,
    lines: u32,
}

impl Loader {
    fn located(&self, e: syntax::Error) -> String {
        let (start, _, label) = self.locations.iter().find(|(start, end, _)| e.pos.line >= *start && e.pos.line < *end)
            .unwrap_or(&self.locations[0]);
        format!("{label}:{}:{}: {}", e.pos.line.saturating_sub(*start) + 1, e.pos.col, e.msg)
    }

    fn expand(&mut self, source: &str, origin: &Path, prefix: &str, root: bool) -> Result<Vec<syntax::Item>, String> {
        let canonical = if origin.exists() {
            origin.canonicalize().map_err(|e| e.to_string())?
        } else {
            origin.to_path_buf()
        };
        if self.active.contains(&canonical) {
            return Err(format!("{}: cyclic schema import", origin.display()));
        }
        if !self.seen.insert(canonical.clone()) {
            return Err(format!("{}: schema imported more than once", origin.display()));
        }
        self.active.push(canonical);
        let offset = self.lines;
        self.lines += source.lines().count() as u32 + 1;
        self.locations.push((offset + 1, self.lines + 1, origin.display().to_string()));
        let items = if offset == 0 { syntax::parse(source) } else { syntax::parse_at(source, offset + 1) }
            .map_err(|e| self.located(e))?;
        let index = self.units.len();
        self.units.push(SchemaUnit { path: origin.to_path_buf(), models: vec![], enums: vec![] });
        let mut out = vec![];
        for mut item in items {
            match &mut item {
                syntax::Item::Import { pos, path, prefix: local } => {
                    let imported = origin.parent().unwrap_or(Path::new("")).join(&*path);
                    if imported.extension().is_some_and(|e| e == "prisma") {
                        let text = std::fs::read_to_string(&imported)
                            .map_err(|e| self.located(syntax::Error { pos: *pos, msg: format!("import {path:?}: {e}") }))?;
                        out.extend(self.expand(&text, &imported, &format!("{prefix}{local}"), false)?);
                        continue;
                    }
                    if !root || !local.is_empty() {
                        return Err(self.located(syntax::Error { pos: *pos,
                            msg: "extension imports belong in the main schema and cannot have a prefix".into() }));
                    }
                    *path = imported.canonicalize().map_err(|e| self.located(syntax::Error {
                        pos: *pos, msg: format!("import {path:?}: {e}")
                    }))?.to_string_lossy().into_owned();
                }
                syntax::Item::Datasource { pos, .. } if !root => {
                    return Err(self.located(syntax::Error { pos: *pos,
                        msg: "datasource settings are only allowed in the main schema".into() }));
                }
                syntax::Item::Model(m) => {
                    self.units[index].models.push(m.name.clone());
                    if !prefix.is_empty() {
                        for name in ["map", "renamed_from"] {
                            if let Some(attr) = m.blocks.iter_mut().find(|a| a.name == name) {
                                if let Some((_, syntax::Value::Str(value))) = attr.args.positional.first_mut() {
                                    *value = format!("{prefix}{value}");
                                }
                            } else if name == "map" {
                                m.blocks.push(syntax::Attr { pos: m.pos, name: "map".into(), args: syntax::Args {
                                    pos: m.pos, positional: vec![(m.pos, syntax::Value::Str(format!("{prefix}{}", m.name.to_lowercase())))],
                                    named: vec![],
                                } });
                            }
                        }
                    }
                }
                syntax::Item::Enum(e) => self.units[index].enums.push(e.name.clone()),
                _ => {}
            }
            out.push(item);
        }
        self.active.pop();
        Ok(out)
    }
}

pub fn compile_project(source: &str, origin: Option<&Path>) -> Result<CompiledProject, String> {
    compile_project_mode(source, origin, None)
}

type IdentityUpdate<'a> = (&'a [(String, String)], &'a [String]);

fn compile_project_mode(source: &str, origin: Option<&Path>, update: Option<IdentityUpdate<'_>>) -> Result<CompiledProject, String> {
    let mut loader = Loader { units: vec![], seen: Default::default(), active: vec![], locations: vec![], lines: 0 };
    let origin = origin.unwrap_or(Path::new("<schema>"));
    let mut items = loader.expand(source, origin, "", true)?;
    let mut declared = std::collections::HashMap::new();
    for item in &items {
        let (name, pos) = match item {
            syntax::Item::Model(m) => (&m.name, m.pos),
            syntax::Item::Enum(e) => (&e.name, e.pos),
            _ => continue,
        };
        if let Some(first) = declared.insert(name, pos) {
            let first = loader.located(syntax::Error { pos: first, msg: "first declaration".into() });
            return Err(loader.located(syntax::Error { pos, msg: format!("{name} is declared twice; {first}") }));
        }
    }
    let label = origin.display().to_string();
    let mut declarations = lower::behavior_declarations(&mut items, &label).map_err(|e| loader.located(e))?;
    for declaration in &mut declarations {
        let location = &mut declaration.location;
        if let Some((start, _, file)) = loader.locations.iter().find(|(start, end, _)| location.line >= *start && location.line < *end) {
            location.file = file.clone();
            location.line = location.line - start + 1;
        }
    }
    let path = crate::identity::manifest_path(origin);
    let needs_identities = update.is_some() || path.is_file() || items.iter().any(|item| {
        matches!(item, syntax::Item::Model(m) if m.members.iter().any(|f| f.ty.name == "ContentType")) && !items.iter().any(|i| matches!(i, syntax::Item::Enum(e) if e.name == "ContentType"))
    }) || declarations.iter().any(|d| d.attribute.starts_with("generic."));
    let identities = if needs_identities {
        if items.iter().any(|i| matches!(i, syntax::Item::Enum(e) if e.name == "ContentType")) {
            return Err("ContentType is generated from the identity manifest; remove the handwritten enum".into());
        }
        let manifest = if let Some((renames, restores)) = update {
            let prior = if path.exists() { crate::identity::read(&path)? } else { Default::default() };
            let names = items.iter().filter_map(|i| if let syntax::Item::Model(m) = i { if declarations.iter().any(|d| d.model == m.name && d.attribute == "proxy.of") { None } else { Some(m.name.clone()) } } else { None }).collect::<Vec<_>>();
            crate::identity::reconcile(&prior, &names, renames, restores)?
        } else { crate::identity::read(&path)? };
        let e = crate::identity::content_type(&manifest)?;
        let members = e.values.iter().map(|v| format!("{} @value({})", v.name, v.value)).collect::<Vec<_>>().join("\n");
        let generated = format!("enum ContentType {{\n{members}\n@@storage(int)\n}}");
        items.extend(syntax::parse(&generated).map_err(|e| format!("generated ContentType: {}", e.msg))?);
        // All per-source Python facades expose the same shared enum.
        for unit in &mut loader.units { unit.enums.push("ContentType".into()); }
        Some(manifest)
    } else { None };
    let catalog_inputs = std::cell::RefCell::new(Vec::new());
    let load = |path: &str| {
        catalog_inputs.borrow_mut().push(PathBuf::from(path));
        std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
    };
    let deferred_identity = declarations.iter().filter(|d| d.field.is_none()).map(|d| d.model.clone()).collect();
    let mut ir = lower::Lowering { load: &load, deferred_identity: &deferred_identity }.lower(items).map_err(|e| loader.located(e))?;
    ir.identities = identities;
    if !declarations.is_empty() {
        ir.behavior.schema_contract = crate::behavior::SCHEMA_CONTRACT;
        ir.behavior.declarations = declarations;
    }
    if update.is_none() {
        crate::behavior::prepare(&mut ir, None)?;
        for model in &ir.models {
            if !model.fields.iter().any(|field| field.primary_key) {
                return Err(format!("model {} has no @id field after extension lowering", model.name));
            }
        }
    }
    crate::identity::validate(&ir)?;
    let mut inputs: Vec<_> = loader.units.iter().map(|unit| unit.path.clone()).collect();
    inputs.extend(catalog_inputs.into_inner());
    if ir.identities.is_some() { inputs.push(path); }
    Ok(CompiledProject { ir, units: loader.units, inputs })
}

pub fn compile_project_file(path: &Path) -> Result<CompiledProject, String> {
    let source = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    compile_project(&source, Some(path))
}

/// Compiles and validates a schema file.
pub fn compile_file(path: &Path) -> Result<SchemaIr, String> {
    let source = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    compile(&source, Some(path))
}

/// Compiles source and checks it the way the engine will (relations, keys, schema
/// objects), so a bad schema fails at compile time rather than at connect.
pub fn check(ir: SchemaIr) -> Result<(SchemaIr, Schema), String> {
    let json = serde_json::to_string(&ir).map_err(|e| e.to_string())?;
    let schema = Schema::from_ir(serde_json::from_str(&json).map_err(|e| e.to_string())?)?;
    crate::migrate::snapshot(&schema)?;
    Ok((ir, schema))
}

#[cfg(test)]
mod tests;

/// Explicit allocation is separate from compilation and usable without a compiled
/// generic relation extension. Validate scalar schema shape before atomic publication.
pub fn generate_identities(path: &Path, renames: &[(String, String)], restores: &[String]) -> Result<orm_contracts::identity::IdentityManifest, String> {
    let source = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let project = compile_project_mode(&source, Some(path), Some((renames, restores)))?;
    let manifest = project.ir.identities.ok_or("missing generated identities")?;
    crate::identity::write(&crate::identity::manifest_path(path), &manifest)?;
    Ok(manifest)
}
