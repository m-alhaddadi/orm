//! Validated, indexed form of [`SchemaIr`] used by the planner.

use std::collections::HashMap;

use crate::ir::{ColType, EnumIr, EnumStorage, ExtensionIr, FieldIr, FunctionIr, ModelIr, RelKind, RelationIr, SchemaIr};

pub type Result<T> = std::result::Result<T, String>;

pub struct Model {
    pub ir: ModelIr,
    pub pk: usize,
    /// Prepared `@client_default`s by field position.
    pub client_defaults: Vec<(usize, crate::client_default::ClientDefault)>,
    #[cfg(feature = "file-storage")]
    pub file_fields: Vec<crate::file_storage::PreparedFileField>,
    #[cfg(feature = "query-defaults")]
    pub query_defaults: crate::selection::PreparedDefaults,
    #[cfg(feature = "composition")]
    pub native: crate::behavior::NativeModel,
    #[cfg(feature = "composition")]
    pub resolved_fields: Vec<crate::behavior::ResolvedField>,
    #[cfg(feature = "composition")]
    pub owner: crate::behavior::OwnerId,
    field_index: HashMap<String, usize>,
    /// relation name -> (index into `ir.relations`, target model index)
    relation_index: HashMap<String, (usize, usize)>,
}

impl Model {
    /// The columns of a derived table (a CTE): no table of its own, no relations, no
    /// primary key (`pk` points at the first column).
    pub fn derived(name: &str, fields: Vec<FieldIr>) -> Result<Model> {
        let mut field_index = HashMap::new();
        for (i, f) in fields.iter().enumerate() {
            if field_index.insert(f.name.clone(), i).is_some() {
                return Err(format!("{name} has several columns named {:?}", f.name));
            }
        }
        let ir = ModelIr {
            name: name.to_owned(),
            table: name.to_owned(),
            fields,
            relations: vec![],
            indexes: vec![],
            constraints: vec![],
            triggers: vec![],
            renamed_from: None,
            comment: None,
        };
        Ok(Model {
            #[cfg(feature = "file-storage")] file_fields: vec![],
            ir, pk: 0, field_index, relation_index: HashMap::new(), client_defaults: vec![],
            #[cfg(feature = "query-defaults")] query_defaults: Default::default(),
            #[cfg(feature = "composition")] native: crate::behavior::NativeModel::None,
            #[cfg(feature = "composition")] owner: crate::behavior::OwnerId(0),
            #[cfg(feature = "composition")] resolved_fields: vec![] })
    }

    pub fn table(&self) -> &str {
        &self.ir.table
    }

    pub fn fields(&self) -> &[FieldIr] {
        &self.ir.fields
    }

    pub fn field(&self, name: &str) -> Result<&FieldIr> {
        self.field_index
            .get(name)
            .map(|&i| &self.ir.fields[i])
            .ok_or_else(|| format!("model {} has no field {name:?}", self.ir.name))
    }

    /// True if the selected file-storage codec owns the field.
    pub fn is_file_field(&self, name: &str) -> bool {
        #[cfg(feature = "file-storage")]
        { self.file_fields.iter().any(|file| self.fields()[file.position].name == name) }
        #[cfg(not(feature = "file-storage"))]
        { let _ = name; false }
    }

    pub fn field_pos(&self, name: &str) -> Result<usize> {
        self.field_index
            .get(name)
            .copied()
            .ok_or_else(|| format!("model {} has no field {name:?}", self.ir.name))
    }

    pub fn pk_field(&self) -> &FieldIr {
        &self.ir.fields[self.pk]
    }

    /// Returns the relation and the index of its target model.
    pub fn relation(&self, name: &str) -> Result<(&RelationIr, usize)> {
        self.relation_index
            .get(name)
            .map(|&(r, t)| (&self.ir.relations[r], t))
            .ok_or_else(|| format!("model {} has no relation {name:?}", self.ir.name))
    }
}

pub struct Schema {
    #[cfg(feature = "proxy-models")]
    pub proxy_models: Vec<crate::proxy::PreparedProxy>,
    pub dialect: crate::dialect::Dialect,
    pub models: Vec<Model>,
    pub enums: Vec<EnumIr>,
    pub identities: Option<crate::identity::IdentityManifest>,
    pub extensions: Vec<ExtensionIr>,
    pub functions: Vec<FunctionIr>,
    pub catalog: Vec<ExtensionIr>,
    model_index: HashMap<String, usize>,
    #[cfg(feature = "composition")]
    pub native_models: Vec<crate::behavior::NativeModel>,
    #[cfg(feature = "composition")]
    storage: Option<Box<Schema>>,
    #[cfg(feature = "composition")]
    pub owner_links: Vec<crate::behavior::PreparedOwnerLink>,
}

impl Schema {
    pub fn from_ir(ir: SchemaIr) -> Result<Self> { Self::from_ir_impl(ir, true) }

    fn from_ir_impl(mut ir: SchemaIr, behavioral: bool) -> Result<Self> {
        crate::identity::validate(&ir)?;
        if behavioral { crate::behavior::prepare(&mut ir, None)?; }
        #[cfg(not(feature = "model-composition"))]
        if ir.behavior.extensions.contains_key("composition") {
            return Err("model-composition is absent from this native artifact; rebuild with model-composition enabled".into());
        }
        #[cfg(feature = "composition")]
        let native_models = if behavioral { crate::behavior::bind(&mut ir)? } else { vec![crate::behavior::NativeModel::None; ir.models.len()] };
        #[cfg(feature = "composition")]
        let storage = match ir.behavior.storage.take() {
            Some(storage) => {
                let physical = serde_json::from_value(serde_json::json!({
                    "models": storage.models, "enums": ir.enums, "dialect": ir.dialect,
                    "extensions": ir.extensions, "catalog": ir.catalog, "functions": ir.functions, "identities": ir.identities,
                })).map_err(|e| e.to_string())?;
                Some(Box::new(Self::from_ir_impl(physical, false)?))
            }
            None => None,
        };
        crate::features::validate(&ir)?;
        #[cfg(feature = "composition")]
        let mut native = native_models.iter().copied();
        let model_index: HashMap<String, usize> = ir
            .models
            .iter()
            .enumerate()
            .map(|(i, m)| (m.name.clone(), i))
            .collect();
        if model_index.len() != ir.models.len() {
            return Err("duplicate model name in schema".into());
        }
        #[cfg(feature = "file-storage")]
        let mut file_fields = crate::file_storage::prepare(&ir)?.into_iter();
        let enums = ir.enums;
        let mut models = Vec::with_capacity(ir.models.len());
        for mut m in ir.models {
            for f in &mut m.fields {
                let Some(name) = &f.enum_name else { continue };
                let i = enums
                    .iter()
                    .position(|e| e.name == *name)
                    .ok_or_else(|| format!("{}.{}: unknown enum {name}", m.name, f.name))?;
                let ok = match enums[i].storage {
                    EnumStorage::Native | EnumStorage::Text => matches!(f.ty, ColType::String | ColType::Text),
                    EnumStorage::Int => matches!(f.ty, ColType::Int | ColType::BigInt),
                };
                if !ok {
                    return Err(format!("{}.{}: enum {name} can't be stored as {:?}", m.name, f.name, f.ty));
                }
                f.enum_idx = Some(i as u32);
            }
            let field_index: HashMap<String, usize> =
                m.fields.iter().enumerate().map(|(i, f)| (f.name.clone(), i)).collect();
            if field_index.len() != m.fields.len() { return Err(format!("model {} has duplicate field names", m.name)); }
            let pks: Vec<usize> = m
                .fields
                .iter()
                .enumerate()
                .filter(|(_, f)| f.primary_key)
                .map(|(i, _)| i)
                .collect();
            let pk = match pks.as_slice() {
                [pk] => *pk,
                _ => return Err(format!("model {} must have exactly one primary key", m.name)),
            };
            let mut relation_index = HashMap::new();
            for (i, r) in m.relations.iter().enumerate() {
                let target = *model_index.get(&r.target).ok_or_else(|| {
                    format!("relation {}.{} targets unknown model {}", m.name, r.name, r.target)
                })?;
                if !field_index.contains_key(&r.from) {
                    return Err(format!("relation {}.{}: no field {:?}", m.name, r.name, r.from));
                }
                if field_index.contains_key(&r.name) {
                    return Err(format!("{}.{} is both a field and a relation", m.name, r.name));
                }
                relation_index.insert(r.name.clone(), (i, target));
            }
            let client_defaults = crate::client_default::prepare(&m.name, &m.fields, &enums)?;
            models.push(Model {
                #[cfg(feature = "file-storage")] file_fields: file_fields.next().expect("prepared file model"),
                ir: m, pk, field_index, relation_index, client_defaults,
                #[cfg(feature = "query-defaults")] query_defaults: Default::default(),
                #[cfg(feature = "composition")] native: native.next().expect("prepared model"),
                #[cfg(feature = "composition")] owner: crate::behavior::OwnerId(0),
            #[cfg(feature = "composition")] resolved_fields: vec![] });
        }
        // Validate relation targets now that every model is indexed.
        for m in &models {
            for r in &m.ir.relations {
                let (_, t) = m.relation(&r.name)?;
                let what = |e: String| format!("relation {}.{}: {e}", m.ir.name, r.name);
                models[t].field(&r.to).map_err(what)?;
                if let Some(th) = &r.through {
                    if r.kind != RelKind::Many {
                        return Err(what("a relation through a join model is to-many".into()));
                    }
                    let j = *model_index.get(&th.model).ok_or_else(|| what(format!("unknown join model {}", th.model)))?;
                    models[j].field(&th.source).map_err(what)?;
                    models[j].field(&th.target).map_err(what)?;
                }
            }
        }
        #[cfg(not(feature = "query-defaults"))]
        if !ir.behavior.query_defaults.is_empty() { return Err("query defaults require an enabled query-defaults artifact; rebuild".into()); }
        #[cfg(feature = "query-defaults")]
        crate::selection::prepare(&mut models, &ir.behavior.query_defaults)?;
        #[cfg(feature = "composition")]
        let owner_links = crate::ownership::resolve(&mut models, storage.as_deref(), &ir.behavior.field_storage, &ir.behavior.owner_links)?;
        #[cfg(feature = "proxy-models")]
        let proxy_models = {
            let mut seen = std::collections::BTreeSet::new();
            for proxy in &ir.behavior.proxy_models {
                if !seen.insert(&proxy.model) || !model_index.contains_key(&proxy.model) || !model_index.contains_key(&proxy.parent) {
                    return Err(format!("{}: duplicate or unresolved proxy metadata; rebuild schema", proxy.model));
                }
                let mut parent = proxy.parent.as_str();
                let mut path = std::collections::BTreeSet::from([proxy.model.as_str()]);
                while let Some(next) = ir.behavior.proxy_models.iter().find(|p| p.model == parent) {
                    if !path.insert(parent) { return Err("cyclic proxy metadata; rebuild schema".into()); }
                    parent = &next.parent;
                }
                let model = &models[model_index[&proxy.model]];
                if proxy.storage_owner != parent { return Err(format!("{}: stale proxy storage owner; rebuild schema", proxy.model)); }
                let physical = storage.as_ref().ok_or("proxy models require explicit physical storage")?;
                if model.table() != models[model_index[parent]].table() || !physical.models.iter().any(|m| m.ir.name == parent && m.table() == model.table()) {
                    return Err(format!("{}: proxy must share its root storage owner", proxy.model));
                }
            }
            models.iter().map(|m| crate::proxy::prepare(&m.ir.name, m.fields(), &enums,
                ir.behavior.proxy_models.iter().find(|p| p.model == m.ir.name))).collect::<Result<Vec<_>>>()?
        };
        Ok(Schema {
            #[cfg(feature = "proxy-models")] proxy_models,
            dialect: ir.dialect,
            models,
            enums,
            identities: ir.identities,
            extensions: ir.extensions,
            functions: ir.functions,
            catalog: ir.catalog,
            model_index,
            #[cfg(feature = "composition")] native_models,
            #[cfg(feature = "composition")] storage,
            #[cfg(feature = "composition")] owner_links,
        })
    }

    /// Schema used by physical migrations; logical views retain their storage.
    pub fn physical(&self) -> &Schema {
        #[cfg(feature = "composition")]
        if let Some(storage) = &self.storage { return storage; }
        self
    }

    pub fn model_idx(&self, name: &str) -> Result<usize> {
        self.model_index.get(name).copied().ok_or_else(|| format!("unknown model {name:?}"))
    }

    pub fn model(&self, idx: usize) -> &Model {
        &self.models[idx]
    }

    /// Walks `path` (relation names) from `root` and returns the model reached.
    pub fn walk(&self, root: usize, path: &[String]) -> Result<usize> {
        let mut cur = root;
        for hop in path {
            cur = self.models[cur].relation(hop)?.1;
        }
        Ok(cur)
    }
}
