//! Validated, indexed form of [`SchemaIr`] used by the planner.

use std::collections::HashMap;

use crate::ir::{ColType, EnumIr, EnumStorage, ExtensionIr, FieldIr, FunctionIr, ModelIr, RelKind, RelationIr, SchemaIr};

pub type Result<T> = std::result::Result<T, String>;

pub struct Model {
    pub ir: ModelIr,
    pub pk: usize,
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
        Ok(Model { ir, pk: 0, field_index, relation_index: HashMap::new() })
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
    pub dialect: crate::dialect::Dialect,
    pub models: Vec<Model>,
    pub enums: Vec<EnumIr>,
    pub extensions: Vec<ExtensionIr>,
    pub functions: Vec<FunctionIr>,
    pub catalog: Vec<ExtensionIr>,
    model_index: HashMap<String, usize>,
}

impl Schema {
    pub fn from_ir(ir: SchemaIr) -> Result<Self> {
        crate::features::validate(&ir)?;
        let model_index: HashMap<String, usize> = ir
            .models
            .iter()
            .enumerate()
            .map(|(i, m)| (m.name.clone(), i))
            .collect();
        if model_index.len() != ir.models.len() {
            return Err("duplicate model name in schema".into());
        }
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
            models.push(Model { ir: m, pk, field_index, relation_index });
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
        Ok(Schema {
            dialect: ir.dialect,
            models,
            enums,
            extensions: ir.extensions,
            functions: ir.functions,
            catalog: ir.catalog,
            model_index,
        })
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
