//! Validated, indexed form of [`SchemaIr`] used by the planner.

use std::collections::HashMap;

use crate::ir::{ExtensionIr, FieldIr, FunctionIr, ModelIr, RelationIr, SchemaIr};

pub type Result<T> = std::result::Result<T, String>;

pub struct Model {
    pub ir: ModelIr,
    pub pk: usize,
    field_index: HashMap<String, usize>,
    /// relation name -> (index into `ir.relations`, target model index)
    relation_index: HashMap<String, (usize, usize)>,
}

impl Model {
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
    pub models: Vec<Model>,
    pub extensions: Vec<ExtensionIr>,
    pub functions: Vec<FunctionIr>,
    model_index: HashMap<String, usize>,
}

impl Schema {
    pub fn from_ir(ir: SchemaIr) -> Result<Self> {
        let model_index: HashMap<String, usize> = ir
            .models
            .iter()
            .enumerate()
            .map(|(i, m)| (m.name.clone(), i))
            .collect();
        if model_index.len() != ir.models.len() {
            return Err("duplicate model name in schema".into());
        }
        let mut models = Vec::with_capacity(ir.models.len());
        for m in ir.models {
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
                models[t].field(&r.to).map_err(|e| format!("relation {}.{}: {e}", m.ir.name, r.name))?;
            }
        }
        Ok(Schema { models, extensions: ir.extensions, functions: ir.functions, model_index })
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
