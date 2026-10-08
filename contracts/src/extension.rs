//! Declarations are setup data. Native execution consumes resolved host identities.
use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use crate::ir::{ColType, SchemaIr, ValueType};

pub const HOST_CONTRACT: u32 = 1;
pub const SCHEMA_CONTRACT: u32 = 1;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub attribute: String,
    pub model: String,
    pub field: Option<String>,
    /// The declared type of the field a field attribute is on, as written: `Tag[]`, `Generic?`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_type: Option<String>,
    pub arguments: BTreeMap<String, serde_json::Value>,
    pub positional: Vec<serde_json::Value>,
    pub location: SourceLocation,
    #[serde(default)]
    pub lowered: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Requirements {
    /// Same-table logical views, prepared by the selected proxy compiler.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proxy_models: Vec<ProxyModel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generic_relations: Vec<crate::generic::GenericRelation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generic_reverse: Vec<crate::generic::GenericReverse>,
    #[serde(default)]
    pub query_defaults: Vec<QueryDefaults>,
    #[serde(default)]
    pub schema_contract: u32,
    #[serde(default)]
    pub declarations: Vec<Declaration>,
    #[serde(default)]
    pub extensions: BTreeMap<String, String>,
    #[serde(default)]
    pub completed_passes: Vec<String>,
    #[serde(default)]
    pub lowered_models: Vec<String>,
    #[serde(default)]
    pub specializations: Vec<Specialization>,
    #[serde(default)]
    pub result_fields: Vec<ResultDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<PhysicalSchema>,
    #[serde(default)]
    pub methods: Vec<MethodDeclaration>,
    #[serde(default)]
    pub field_storage: Vec<FieldStorage>,
    #[serde(default)]
    pub owner_links: Vec<OwnerLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_adapters: Vec<FieldAdapter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_fields: Vec<FileField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updated_at: Vec<crate::tracking::UpdatedAt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub soft_delete: Vec<crate::tracking::SoftDelete>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<crate::tracking::VersionField>,
}
impl Requirements {
    pub fn is_empty(&self) -> bool {
        self.query_defaults.is_empty() && self.proxy_models.is_empty() && self.generic_relations.is_empty() && self.generic_reverse.is_empty() && self.declarations.is_empty() && self.extensions.is_empty() && self.specializations.is_empty()
            && self.lowered_models.is_empty() && self.completed_passes.is_empty() && self.result_fields.is_empty() && self.storage.is_none() && self.field_storage.is_empty() && self.owner_links.is_empty() && self.methods.is_empty() && self.field_adapters.is_empty() && self.file_fields.is_empty()
            && self.updated_at.is_empty() && self.soft_delete.is_empty() && self.versions.is_empty() && self.schema_contract == 0
    }
}

/// A build-selected binding codec identity, never a provider or import path.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FieldAdapter {
    pub model: String,
    pub field: String,
    pub adapter: String,
}

/// File-field configuration has durable identity only; clients remain application configuration.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileField {
    pub model: String,
    pub field: String,
    pub storage: String,
    pub reference_contract: u32,
}

pub const FILE_REFERENCE_ADAPTER: &str = "file-storage.reference.v1";

/// Names the host runtimes give meaning to on models and instances.
pub const RESERVED_MEMBERS: [&str; 12] =
    ["objects", "_meta", "DoesNotExist", "MultipleObjectsReturned", "pk", "update", "delete", "refresh", "toJSON", "constructor", "toString", "then"];

/// Reject a file field whose generated Python or TypeScript operations collide with a model member.
pub fn check_file_methods(ir: &SchemaIr, model: &crate::ir::ModelIr, field: &str) -> Result<(), String> {
    for method in [format!("{field}_signed_url"), format!("{field}_open"), format!("{}SignedUrl", camel(field)), format!("{}Open", camel(field))] {
        let taken = |name: &str| name == method || camel(name) == method;
        if RESERVED_MEMBERS.contains(&method.as_str())
            || model.fields.iter().any(|f| taken(&f.name))
            || model.relations.iter().any(|r| taken(&r.name))
            || ir.behavior.methods.iter().any(|m| m.model == model.name && taken(&m.name))
        {
            return Err(format!("{}.{method}: generated file method collides with a model member", model.name));
        }
    }
    Ok(())
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

/// Validate field codec shapes once, before usable models are published.
pub fn validate_field_adapters(ir: &SchemaIr) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for adapter in &ir.behavior.field_adapters {
        let model = ir.models.iter().find(|m| m.name == adapter.model)
            .ok_or_else(|| format!("unknown field adapter model {}", adapter.model))?;
        let field = model.fields.iter().find(|f| f.name == adapter.field)
            .ok_or_else(|| format!("{}.{}: unknown adapter field", adapter.model, adapter.field))?;
        if field.ty != ColType::Json || field.array || field.enum_name.is_some() {
            return Err(format!("{}.{}: field adapters require scalar Json", adapter.model, adapter.field));
        }
        if !seen.insert((&adapter.model, &adapter.field)) || adapter.adapter.is_empty() {
            return Err(format!("{}.{}: duplicate/empty field adapter", adapter.model, adapter.field));
        }
    }
    let mut files = std::collections::BTreeSet::new();
    for file in &ir.behavior.file_fields {
        if !files.insert((&file.model, &file.field)) || file.reference_contract != 1
            || file.storage.is_empty() || file.storage.contains('\0') {
            return Err(format!("{}.{}: invalid file-field contract", file.model, file.field));
        }
        if !ir.behavior.field_adapters.iter().any(|a| a.model == file.model && a.field == file.field && a.adapter == FILE_REFERENCE_ADAPTER) {
            return Err(format!("{}.{}: missing file reference adapter", file.model, file.field));
        }
    }
    for adapter in ir.behavior.field_adapters.iter().filter(|a| a.adapter == FILE_REFERENCE_ADAPTER) {
        if !files.contains(&(&adapter.model, &adapter.field)) {
            return Err(format!("{}.{}: missing file-field configuration", adapter.model, adapter.field));
        }
    }
    Ok(())
}

/// Intended shape only. Physical encoding, enum representation and constraints
/// remain on the storage owner. These contracts never imply SQL predicates.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ProxyModel {
    pub model: String,
    pub parent: String,
    /// Resolved root physical model identity, filled by lowering. Generic identity
    /// manifests exclude `model` and refer to this concrete storage identity.
    #[serde(default)]
    pub storage_owner: String,
    #[serde(default)]
    pub fields: Vec<ProxyField>,
    /// `@@proxy.fields(include:|exclude:)`: which source fields and relations the view keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ProxySelection>,
    /// Source fields and relations that are not part of the view, filled by lowering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProxySelection {
    Include(Vec<String>),
    Exclude(Vec<String>),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProxyField {
    pub field: String,
    #[serde(default)]
    pub non_null: bool,
    /// Parent enum member names, not database labels. None means the whole enum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subset: Option<Vec<String>>,
}

/// Setup contribution connecting a logical field to one physical owner.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FieldStorage {
    pub model: String,
    pub field: String,
    pub owner: String,
    pub column: String,
}
/// Shared-identity owner edge. Each child has at most one parent.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerLink {
    pub child: String,
    pub parent: String,
    pub child_key: String,
    pub parent_key: String,
}
#[derive(Clone, Copy, Debug)]
pub struct PreparedOwnerLink {
    pub child: OwnerId,
    pub parent: OwnerId,
    pub child_key: usize,
    pub parent_key: usize,
}

/// A generated ordinary model method backed by one compiled native export.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MethodDeclaration {
    pub model: String,
    pub name: String,
    pub native_function: String,
    pub input: ColType,
    pub output: Option<ColType>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResultDeclaration {
    pub model: String,
    pub field: String,
    pub dependencies: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Specialization {
    pub model: String,
    pub fingerprint: String,
    pub exports: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub host_contract: u32,
    pub schema_contract: u32,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub languages: Vec<String>,
    pub databases: Vec<String>,
    #[serde(default)]
    pub attributes: Vec<Attribute>,
    #[serde(default)]
    pub passes: Vec<Pass>,
    #[serde(default)]
    pub exports: Vec<Export>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Attribute {
    pub name: String,
    pub target: AttributeTarget,
    #[serde(default)]
    pub arguments: BTreeMap<String, Argument>,
    #[serde(default)]
    pub positional: Vec<Argument>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Argument {
    pub kind: ArgumentKind,
    #[serde(default)]
    pub required: bool,
    /// The last positional argument only: it takes any number of values of `kind`.
    #[serde(default, skip_serializing_if = "crate::ir::is_false")]
    pub variadic: bool,
}
impl Attribute {
    /// The positional argument that describes value `i`.
    pub fn positional_at(&self, i: usize) -> Option<&Argument> {
        self.positional.get(i).or_else(|| self.positional.last().filter(|a| a.variadic))
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentKind { String, Integer, Boolean, List, Value }
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttributeTarget { Model, Field }
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Phase { Declaration, Logical, Storage, Behavior, Validation, Generation }
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pass {
    pub id: String,
    pub phase: Phase,
    /// Compiler entry point: `fn(&mut SchemaIr) -> Result<(), String>`.
    pub rust: String,
    #[serde(default)]
    pub after: Vec<String>,
    /// Exclusive effects, e.g. `model.User.field.name.type`.
    #[serde(default)]
    pub effects: Vec<String>,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExportKind { StringValidator, StringTransform, StringComputed, StringRecordValidator }
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub id: String,
    pub rust: String,
    pub kind: ExportKind,
    pub input: ColType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<ColType>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub host_contract: u32,
    pub schema_contract: u32,
    #[serde(default)]
    pub passes: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub extensions: BTreeMap<String, String>,
    pub exports: Vec<String>,
    pub specializations: Vec<Specialization>,
}
impl Default for Artifact {
    fn default() -> Self {
        Self { host_contract: HOST_CONTRACT, schema_contract: SCHEMA_CONTRACT,
            passes: vec![], capabilities: vec![], extensions: BTreeMap::new(), exports: vec![], specializations: vec![] }
    }
}

/// Stable resolved positions within an immutable schema snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModelId(pub usize);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FieldId { pub model: ModelId, pub position: usize }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StorageId { pub owner: OwnerId, pub column: usize }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OwnerId(pub usize);
#[derive(Clone, Debug)]
pub struct ResolvedField {
    pub logical: FieldId,
    pub storage: StorageId,
    pub logical_type: ValueType,
    pub physical_type: ValueType,
}
#[derive(Debug)]
pub struct ResultField {
    pub field: FieldId,
    pub physical: Option<usize>,
    pub public: bool,
    pub dependencies: Vec<FieldId>,
}
#[derive(Debug)]
pub struct ResultShape { pub model: ModelId, pub fields: Vec<ResultField> }
#[derive(Debug)]
pub struct QueryContract<'a> {
    pub expression: &'a crate::ir::Expr,
    pub owner: OwnerId,
    pub shape: &'a ResultShape,
}

pub fn validate_declarations(ir: &SchemaIr, manifests: &[Manifest], language: Option<&str>) -> Result<(), String> {
    let fail = |d: &Declaration, msg: String| format!("{}:{}:{}: @{}: {msg}",
        d.location.file, d.location.line, d.location.column, d.attribute);
    for d in &ir.behavior.declarations {
        // One name can have a model entry (`@@x`) and a field entry (`@x`).
        let named = || manifests.iter().flat_map(|m| m.attributes.iter().map(move |a| (m, a))).filter(|(_, a)| a.name == d.attribute);
        let found = named().find(|(_, a)| (a.target == AttributeTarget::Field) == d.field.is_some()).or_else(|| named().next());
        let (m, a) = found.ok_or_else(|| fail(d, "not compiled into this artifact; rebuild with its extension".into()))?;
        if m.host_contract != HOST_CONTRACT || m.schema_contract != SCHEMA_CONTRACT {
            return Err(fail(d, format!("{} requires incompatible host/schema contracts; rebuild", m.id)));
        }
        if !m.databases.iter().any(|s| s == ir.dialect.name()) {
            return Err(fail(d, format!("{} does not support {}", m.id, ir.dialect.name())));
        }
        if language.is_some_and(|l| !m.languages.iter().any(|s| s == l)) {
            return Err(fail(d, format!("{} does not support this language binding", m.id)));
        }
        if (d.field.is_some()) != (a.target == AttributeTarget::Field) {
            let place = if a.target == AttributeTarget::Field { format!("a field attribute; write @{} on a field", d.attribute) } else { format!("a model attribute; write @@{} in the model block", d.attribute) };
            return Err(fail(d, format!("invalid declaration target: it is {place}")));
        }
        // Only `value` arguments take null and objects, also inside a list.
        fn literal(v: &serde_json::Value) -> bool {
            match v { serde_json::Value::Null | serde_json::Value::Object(_) => false, serde_json::Value::Array(items) => items.iter().all(literal), _ => true }
        }
        let matches = |v: &serde_json::Value, arg: &Argument| match arg.kind {
            ArgumentKind::String => v.is_string(), ArgumentKind::Integer => v.is_i64(),
            ArgumentKind::Boolean => v.is_boolean(), ArgumentKind::List => v.is_array() && literal(v),
            ArgumentKind::Value => true,
        };
        for (name, value) in &d.arguments {
            let arg = a.arguments.get(name).ok_or_else(|| fail(d, format!("unknown argument {name}")))?;
            if !matches(value, arg) { return Err(fail(d, format!("wrong type for {name}"))); }
        }
        for (name, arg) in &a.arguments {
            if arg.required && !d.arguments.contains_key(name) { return Err(fail(d, format!("missing argument {name}"))); }
        }
        if a.positional.iter().rev().skip(1).any(|arg| arg.variadic) {
            return Err(fail(d, "only the last positional argument can be variadic".into()));
        }
        if d.positional.len() > a.positional.len() && !a.positional.last().is_some_and(|arg| arg.variadic) {
            return Err(fail(d, "too many positional arguments".into()));
        }
        for (i, arg) in a.positional.iter().enumerate() {
            if d.positional.get(i).is_none() && arg.required { return Err(fail(d, format!("missing positional argument {i}"))); }
        }
        for (i, v) in d.positional.iter().enumerate() {
            if !a.positional_at(i).is_some_and(|arg| matches(v, arg)) { return Err(fail(d, format!("wrong type for positional argument {i}"))); }
        }
    }
    Ok(())
}

/// Contract compatibility is checked once at definition, never at materialization.
pub fn check_requirements(ir: &SchemaIr, artifact: &Artifact) -> Result<(), String> {
    validate_field_adapters(ir)?;
    let r = &ir.behavior;
    for adapter in &r.field_adapters {
        if adapter.adapter == FILE_REFERENCE_ADAPTER && !artifact.capabilities.iter().any(|c| c == "file-storage") {
            return Err("file-storage adapter is not compiled into this artifact; rebuild".into());
        }
    }
    if (!r.generic_relations.is_empty() || !r.generic_reverse.is_empty()) && !artifact.capabilities.iter().any(|c| c == "generic-relations") {
        return Err("generic relations are not compiled into this artifact; rebuild with generic-relations".into());
    }
    if !r.proxy_models.is_empty() && !artifact.capabilities.iter().any(|c| c == "proxy-models") {
        return Err("proxy models require the compiled proxy-models capability; rebuild native artifact".into());
    }
    if !r.is_empty() && r.schema_contract != artifact.schema_contract {
        return Err(format!("schema contract {} unavailable (artifact {}); rebuild native artifact", r.schema_contract, artifact.schema_contract));
    }
    if r.storage.is_some() && !artifact.capabilities.iter().any(|c| c == "physical-schema") {
        return Err(format!("physical schema contributions are not compiled into this artifact (capabilities {:?}); rebuild", artifact.capabilities));
    }
    for pass in &r.completed_passes {
        if !artifact.passes.contains(pass) { return Err(format!("lowering pass {pass} unavailable (artifact has {:?}); rebuild native artifact", artifact.passes)); }
    }
    for (id, version) in &r.extensions {
        if artifact.extensions.get(id) != Some(version) {
            return Err(format!("extension {id}@{version} unavailable; artifact has {:?}; rebuild", artifact.extensions.get(id)));
        }
    }
    for method in &r.methods {
        if !r.specializations.iter().any(|s| s.model == method.model && artifact.specializations.contains(s)) {
            return Err(format!("{}.{}: native method requires a compiled specialization; rebuild", method.model, method.name));
        }
    }
    for result in &r.result_fields {
        if !r.specializations.iter().any(|s| s.model == result.model && artifact.specializations.contains(s)) {
            return Err(format!("{}.{}: native result requires a compiled specialization; rebuild", result.model, result.field));
        }
    }
    for specialization in &r.specializations {
        for export in &specialization.exports {
            if !artifact.exports.contains(export) {
                return Err(format!("{} requires missing export {export} (artifact has {:?}); rebuild native artifact", specialization.model, artifact.exports));
            }
        }
        if !artifact.specializations.contains(specialization) {
            return Err(format!("{}: stale or missing specialization {}; rebuild native artifact", specialization.model, specialization.fingerprint));
        }
    }
    Ok(())
}

/// Validate active compiled contributions even when a normalized schema carries no
/// source declarations (e.g. generated code or a matching native specialization).
pub fn validate_requirements(ir: &SchemaIr, manifests: &[Manifest], language: Option<&str>) -> Result<(), String> {
    for (id, version) in &ir.behavior.extensions {
        let manifest = manifests.iter().find(|m| m.id == *id && m.version == *version)
            .ok_or_else(|| format!("missing extension {id}@{version}; rebuild native artifact"))?;
        if !manifest.databases.iter().any(|d| d == ir.dialect.name()) {
            return Err(format!("{id}@{version} does not support {}; rebuild an appropriate profile", ir.dialect.name()));
        }
        if language.is_some_and(|language| !manifest.languages.iter().any(|l| l == language)) {
            return Err(format!("{id}@{version} does not support this binding; rebuild an appropriate profile"));
        }
    }
    Ok(())
}

/// Physical schema state is explicit. Removing or renaming a logical field does not
/// alter this snapshot; a compiler pass must contribute physical changes here.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalSchema { pub models: Vec<crate::ir::ModelIr> }

/// A later batch (for example a second class `prepare()`) adds the owners of its new
/// tables; the snapshot of earlier batches stays unchanged.
/// Test support for pass authors. Captures storage and runs `pass` on a copy of `ir`, as
/// the generated `prepare` does, and returns the IR sections that the pass changed
/// (`models`, `enums`, `behavior.<key>`, ...) to compare with its declared effects.
/// Then marks the batch lowered and fails if a second run changes anything.
pub fn pass_effects(ir: &SchemaIr, pass: impl Fn(&mut SchemaIr) -> Result<(), String>) -> Result<std::collections::BTreeSet<String>, String> {
    fn sections(value: &serde_json::Value, prefix: &str, out: &mut BTreeMap<String, serde_json::Value>) {
        for (key, value) in value.as_object().into_iter().flatten() {
            if prefix.is_empty() && key == "behavior" { sections(value, "behavior.", out); }
            else { out.insert(format!("{prefix}{key}"), value.clone()); }
        }
    }
    let snapshot = |ir: &SchemaIr| -> Result<BTreeMap<String, serde_json::Value>, String> {
        let mut out = BTreeMap::new();
        sections(&serde_json::to_value(ir).map_err(|e| e.to_string())?, "", &mut out);
        Ok(out)
    };
    let changed = |a: &BTreeMap<String, serde_json::Value>, b: &BTreeMap<String, serde_json::Value>| -> std::collections::BTreeSet<String> {
        a.keys().chain(b.keys()).filter(|k| a.get(*k) != b.get(*k)).cloned().collect()
    };
    let mut lowered: SchemaIr = serde_json::from_value(serde_json::to_value(ir).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    capture_storage(&mut lowered)?;
    let before = snapshot(&lowered)?;
    pass(&mut lowered)?;
    let effects = changed(&before, &snapshot(&lowered)?);
    for declaration in &mut lowered.behavior.declarations { declaration.lowered = true; }
    lowered.behavior.lowered_models = lowered.models.iter().map(|m| m.name.clone()).collect();
    capture_storage(&mut lowered)?;
    let once = snapshot(&lowered)?;
    pass(&mut lowered)?;
    let again = changed(&once, &snapshot(&lowered)?);
    if !again.is_empty() { return Err(format!("the pass lowered an already lowered batch again; it changed {again:?}")); }
    Ok(effects)
}

pub fn capture_storage(ir: &mut SchemaIr) -> Result<(), String> {
    let mut models = physical_models(ir)?;
    match &mut ir.behavior.storage {
        None => ir.behavior.storage = Some(PhysicalSchema { models }),
        Some(storage) => {
            models.retain(|m| !ir.behavior.lowered_models.contains(&m.name)
                && !storage.models.iter().any(|s| s.name == m.name || s.table == m.table));
            storage.models.extend(models);
        }
    }
    Ok(())
}

/// The capability, and Cargo feature, of compiled query defaults.
pub const QUERY_DEFAULTS: &str = "query-defaults";
/// Capabilities, and Cargo features, of the column roles in [`crate::tracking`].
pub const UPDATED_AT: &str = "updated-at";
pub const SOFT_DELETE: &str = "soft-delete";
pub const OPTIMISTIC_LOCKING: &str = "optimistic-locking";

pub const HOST_CAPABILITIES: &[&str] = &[
    "schema-transformations", "physical-schema", "native-string-values",
    "native-string-records", "native-string-results", "file-storage", QUERY_DEFAULTS,
    "model-composition", "proxy-models", "generic-relations", UPDATED_AT, SOFT_DELETE, OPTIMISTIC_LOCKING,
];

/// Capabilities that a build selects as the host Cargo feature of the same name.
pub const HOST_FEATURE_CAPABILITIES: &[&str] = &["file-storage", QUERY_DEFAULTS, "model-composition", "proxy-models", "generic-relations", UPDATED_AT, SOFT_DELETE, OPTIMISTIC_LOCKING];

/// Combine one new declaration batch with an immutable definition context. Lowered
/// declarations retain their phase state; new declarations are the only pass inputs.
pub fn merge_definition(mut context: SchemaIr, mut incoming: SchemaIr) -> Result<SchemaIr, String> {
    if context.dialect != incoming.dialect { return Err("schemas in one registry must target the same database".into()); }
    match (&context.identities, incoming.identities.take()) {
        (Some(old), Some(new)) if old != &new => return Err("different identity manifests in one registry; define the complete application schema together".into()),
        (None, new) => context.identities = new,
        _ => {}
    }
    for model in &incoming.models {
        if context.models.iter().any(|m| m.name == model.name) {
            return Err(format!("a model named {} is already registered", model.name));
        }
    }
    for e in incoming.enums.drain(..) {
        if let Some(known) = context.enums.iter().find(|k| k.name == e.name) {
            if known != &e { return Err(format!("an enum named {} is already registered", e.name)); }
        } else { context.enums.push(e); }
    }
    fn append<T: Serialize>(target: &mut Vec<T>, incoming: Vec<T>) -> Result<(), String> {
        for value in incoming {
            let encoded = serde_json::to_value(&value).map_err(|e| e.to_string())?;
            if !target.iter().any(|v| serde_json::to_value(v).ok().as_ref() == Some(&encoded)) { target.push(value); }
        }
        Ok(())
    }
    append(&mut context.extensions, std::mem::take(&mut incoming.extensions))?;
    append(&mut context.functions, std::mem::take(&mut incoming.functions))?;
    append(&mut context.catalog, std::mem::take(&mut incoming.catalog))?;
    let prior_physical = context.behavior.storage.take();
    let new_physical = incoming.behavior.storage.take();
    if prior_physical.is_some() || new_physical.is_some() {
        let prior = match prior_physical {
            Some(p) => p.models,
            None => physical_models(&context)?,
        };
        let new = match new_physical {
            Some(p) => p.models,
            None => physical_models(&incoming)?,
        };
        let mut models = prior;
        for model in new {
            if models.iter().any(|m| m.name == model.name) { return Err(format!("duplicate physical owner {}", model.name)); }
            models.push(model);
        }
        context.behavior.storage = Some(PhysicalSchema { models });
    }
    context.models.extend(incoming.models);
    let c = &mut context.behavior;
    let n = incoming.behavior;
    if c.schema_contract != 0 && n.schema_contract != 0 && c.schema_contract != n.schema_contract {
        return Err("incompatible schema contracts; rebuild dependent schemas together".into());
    }
    c.schema_contract = c.schema_contract.max(n.schema_contract);
    if !c.completed_passes.is_empty() && !n.completed_passes.is_empty() && c.completed_passes != n.completed_passes {
        return Err("incompatible lowering passes; rebuild dependent schemas together".into());
    }
    if c.completed_passes.is_empty() { c.completed_passes = n.completed_passes; }
    c.lowered_models.extend(n.lowered_models);
    c.lowered_models.sort(); c.lowered_models.dedup();
    c.declarations.extend(n.declarations);
    c.proxy_models.extend(n.proxy_models);
    c.generic_relations.extend(n.generic_relations);
    c.generic_reverse.extend(n.generic_reverse);
    c.query_defaults.extend(n.query_defaults);
    c.specializations.extend(n.specializations);
    c.result_fields.extend(n.result_fields);
    c.methods.extend(n.methods);
    c.field_storage.extend(n.field_storage);
    c.owner_links.extend(n.owner_links);
    c.field_adapters.extend(n.field_adapters);
    c.file_fields.extend(n.file_fields);
    c.updated_at.extend(n.updated_at);
    c.soft_delete.extend(n.soft_delete);
    c.versions.extend(n.versions);
    for (id, version) in n.extensions {
        if c.extensions.get(&id).is_some_and(|v| v != &version) { return Err(format!("incompatible extension {id}; rebuild dependent schemas together")); }
        c.extensions.insert(id, version);
    }
    Ok(context)
}
fn physical_models(ir: &SchemaIr) -> Result<Vec<crate::ir::ModelIr>, String> {
    let mut models: Vec<crate::ir::ModelIr> = serde_json::from_value(serde_json::to_value(&ir.models).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    for model in &mut models {
        model.fields.retain(|f| !ir.behavior.result_fields.iter().any(|r| r.model == model.name && r.field == f.name));
    }
    Ok(models)
}

/// Physical assumptions relevant to one specialization, including inherited owners.
pub fn specialization_storage(model: &crate::ir::ModelIr, requirements: &Requirements, computed: &[String]) -> Result<Vec<crate::ir::ModelIr>, String> {
    if let Some(storage) = &requirements.storage {
        let mut names: std::collections::BTreeSet<String> = requirements.field_storage.iter().filter(|f| f.model == model.name).map(|f| f.owner.clone()).collect();
        if let Some(local) = storage.models.iter().find(|m| m.table == model.table) { names.insert(local.name.clone()); }
        loop {
            let count = names.len();
            for link in &requirements.owner_links { if names.contains(&link.child) { names.insert(link.parent.clone()); } }
            if names.len() == count { break; }
        }
        return serde_json::from_value(serde_json::to_value(storage.models.iter().filter(|m| names.contains(&m.name)).collect::<Vec<_>>()).map_err(|e| e.to_string())?).map_err(|e| e.to_string());
    }
    let mut physical: crate::ir::ModelIr = serde_json::from_value(serde_json::to_value(model).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    physical.fields.retain(|f| !computed.contains(&f.name));
    Ok(vec![physical])
}

/// Resolved schema policies. `filter` is query expression JSON, never SQL text.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QueryDefaults {
    pub model: String,
    #[serde(default)] pub parent: Option<String>,
    #[serde(default)] pub filter: Option<serde_json::Value>,
    #[serde(default)] pub fields: Option<Vec<String>>,
    #[serde(default)] pub related: Vec<Vec<String>>,
    /// The order of reads that give no `order_by()`; empty is none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub order: Vec<OrderKey>,
}

/// One column of a default order.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OrderKey {
    pub field: String,
    #[serde(default, skip_serializing_if = "crate::ir::is_false")] pub desc: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub nulls: Option<crate::ir::Nulls>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(languages: &[&str], databases: &[&str]) -> Manifest {
        serde_json::from_value(serde_json::json!({
            "id": "app", "version": "1.0.0", "host_contract": HOST_CONTRACT, "schema_contract": SCHEMA_CONTRACT,
            "languages": languages, "databases": databases,
            "attributes": [{"name": "app.trim", "target": "field"}],
        })).unwrap()
    }
    fn schema(dialect: crate::dialect::Dialect) -> SchemaIr {
        let mut ir = SchemaIr { dialect, ..SchemaIr::default() };
        ir.behavior.declarations.push(Declaration {
            attribute: "app.trim".into(), model: "User".into(), field: Some("name".into()), field_type: None,
            arguments: BTreeMap::new(), positional: vec![],
            location: SourceLocation { file: "schema.prisma".into(), line: 3, column: 7 }, lowered: false,
        });
        ir
    }

    #[test]
    fn unsupported_database_names_the_declaration() {
        let error = validate_declarations(&schema(crate::dialect::Dialect::Sqlite), &[manifest(&["python"], &["postgres"])], None).unwrap_err();
        assert!(error.starts_with("schema.prisma:3:7: @app.trim:") && error.contains("does not support sqlite"), "{error}");
    }

    #[test]
    fn only_value_arguments_take_null_and_objects() {
        let mut manifest = manifest(&["python"], &["sqlite"]);
        manifest.attributes[0].arguments = serde_json::from_value(serde_json::json!({"tags": {"kind": "list"}, "payload": {"kind": "value"}})).unwrap();
        let check = |name: &str, value: serde_json::Value| {
            let mut ir = schema(crate::dialect::Dialect::Sqlite);
            ir.behavior.declarations[0].arguments.insert(name.into(), value);
            validate_declarations(&ir, std::slice::from_ref(&manifest), None)
        };
        assert!(check("tags", serde_json::json!(["a", [1]])).is_ok());
        assert!(check("tags", serde_json::json!(["a", null])).unwrap_err().contains("wrong type for tags"));
        assert!(check("tags", serde_json::json!([{"k": 1}])).is_err());
        assert!(check("payload", serde_json::json!({"labels": [null]})).is_ok());
    }

    #[test]
    fn a_variadic_last_positional_takes_any_number_of_values() {
        let mut manifest = manifest(&["python"], &["sqlite"]);
        manifest.attributes[0].positional = serde_json::from_value(serde_json::json!([{"kind": "string", "variadic": true}])).unwrap();
        let check = |manifest: &Manifest, values: serde_json::Value| {
            let mut ir = schema(crate::dialect::Dialect::Sqlite);
            ir.behavior.declarations[0].positional = serde_json::from_value(values).unwrap();
            validate_declarations(&ir, std::slice::from_ref(manifest), None)
        };
        assert!(check(&manifest, serde_json::json!([])).is_ok());
        assert!(check(&manifest, serde_json::json!(["-a", "b", "c nulls last"])).is_ok());
        assert!(check(&manifest, serde_json::json!(["a", 1])).unwrap_err().contains("wrong type for positional argument 1"));
        manifest.attributes[0].positional[0].variadic = false;
        assert!(check(&manifest, serde_json::json!(["a", "b"])).unwrap_err().contains("too many positional arguments"));
        manifest.attributes[0].positional = serde_json::from_value(serde_json::json!([{"kind": "string", "variadic": true}, {"kind": "string"}])).unwrap();
        assert!(check(&manifest, serde_json::json!(["a"])).unwrap_err().contains("only the last positional argument"));
    }

    #[test]
    fn one_name_has_a_model_entry_and_a_field_entry() {
        let mut manifest = manifest(&["python"], &["sqlite"]);
        manifest.attributes.push(serde_json::from_value(serde_json::json!({"name": "app.trim", "target": "model", "arguments": {"all": {"kind": "boolean", "required": true}}})).unwrap());
        let mut ir = schema(crate::dialect::Dialect::Sqlite);
        assert!(validate_declarations(&ir, std::slice::from_ref(&manifest), None).is_ok());
        let field = ir.behavior.declarations[0].field.take();
        assert!(validate_declarations(&ir, std::slice::from_ref(&manifest), None).unwrap_err().contains("missing argument all"));
        manifest.attributes.pop();
        let error = validate_declarations(&ir, std::slice::from_ref(&manifest), None).unwrap_err();
        assert!(error.contains("invalid declaration target: it is a field attribute; write @app.trim on a field"), "{error}");
        manifest.attributes[0].target = AttributeTarget::Model;
        ir.behavior.declarations[0].field = field;
        let error = validate_declarations(&ir, std::slice::from_ref(&manifest), None).unwrap_err();
        assert!(error.contains("invalid declaration target: it is a model attribute; write @@app.trim in the model block"), "{error}");
    }

    #[test]
    fn unsupported_language_fails() {
        let error = validate_declarations(&schema(crate::dialect::Dialect::Sqlite), &[manifest(&["python"], &["sqlite"])], Some("typescript")).unwrap_err();
        assert!(error.contains("does not support this language binding"), "{error}");
        assert!(validate_declarations(&schema(crate::dialect::Dialect::Sqlite), &[manifest(&["python"], &["sqlite"])], Some("python")).is_ok());
    }

    #[test]
    fn unknown_metadata_and_wrong_export_types_fail() {
        let mut value = serde_json::to_value(manifest(&["python"], &["sqlite"])).unwrap();
        value["unknown"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Manifest>(value).unwrap_err().to_string().contains("unknown field"));
        let mut export = serde_json::json!({"id": "app.x", "rust": "crate::x", "kind": "string_validator", "input": "string"});
        assert!(serde_json::from_value::<Export>(export.clone()).is_ok());
        export["kind"] = "integer_validator".into();
        assert!(serde_json::from_value::<Export>(export).unwrap_err().to_string().contains("unknown variant"));
    }

    #[test]
    fn requirement_errors_name_the_model_requirement_and_artifact_contents() {
        let mut ir = SchemaIr::default();
        ir.behavior.schema_contract = SCHEMA_CONTRACT;
        ir.behavior.completed_passes = vec!["app.lower".into()];
        let artifact = Artifact { passes: vec!["other.lower".into()], ..Artifact::default() };
        let error = check_requirements(&ir, &artifact).unwrap_err();
        assert!(error.contains("app.lower") && error.contains("other.lower") && error.contains("rebuild"), "{error}");
        ir.behavior.completed_passes.clear();
        ir.behavior.specializations = vec![Specialization { model: "User".into(), fingerprint: "f".into(), exports: vec!["app.trim".into()] }];
        let artifact = Artifact { exports: vec!["app.other".into()], ..Artifact::default() };
        let error = check_requirements(&ir, &artifact).unwrap_err();
        assert!(error.starts_with("User requires missing export app.trim") && error.contains("app.other") && error.contains("rebuild"), "{error}");
        let artifact = Artifact { exports: vec!["app.trim".into()], ..Artifact::default() };
        let error = check_requirements(&ir, &artifact).unwrap_err();
        assert!(error.starts_with("User: stale or missing specialization f") && error.contains("rebuild"), "{error}");
    }

    #[test]
    fn pass_effects_reports_sections_and_rejects_a_second_lowering() {
        let ir = schema(crate::dialect::Dialect::Sqlite);
        let once = |ir: &mut SchemaIr| -> Result<(), String> {
            if ir.behavior.declarations.iter().any(|d| !d.lowered) { ir.behavior.completed_passes.push("app.lower".into()); }
            Ok(())
        };
        assert_eq!(pass_effects(&ir, once).unwrap().into_iter().collect::<Vec<_>>(), ["behavior.completed_passes"]);
        let always = |ir: &mut SchemaIr| -> Result<(), String> { ir.behavior.lowered_models.push("again".into()); Ok(()) };
        assert!(pass_effects(&ir, always).unwrap_err().contains("behavior.lowered_models"));
    }

    #[test]
    fn a_later_batch_adds_the_owners_of_its_new_tables() {
        let model = |name: &str, table: &str| -> crate::ir::ModelIr {
            serde_json::from_value(serde_json::json!({"name": name, "table": table, "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": true}]})).unwrap()
        };
        let mut ir = SchemaIr { models: vec![model("User", "user")], ..SchemaIr::default() };
        capture_storage(&mut ir).unwrap();
        ir.behavior.lowered_models = vec!["User".into()];
        ir.models[0].table = "renamed".into();
        ir.models.push(model("Other", "other"));
        ir.models.push(model("ActiveUser", "user"));
        capture_storage(&mut ir).unwrap();
        let owners: Vec<_> = ir.behavior.storage.unwrap().models.into_iter().map(|m| (m.name, m.table)).collect();
        assert_eq!(owners, [("User".to_string(), "user".to_string()), ("Other".into(), "other".into())]);
    }
}
