//! Schema defaults are resolved once, before publishing a schema.
use crate::{behavior::QueryDefaults, ir::Expr, schema::Model};

#[derive(Default)]
pub struct PreparedDefaults {
    pub filter: Option<Expr>,
    pub fields: Option<Vec<String>>,
    pub related: Vec<Vec<String>>,
}

pub fn prepare(models: &mut [Model], defaults: &[QueryDefaults]) -> Result<(), String> {
    let mut filters = Vec::with_capacity(defaults.len());
    for d in defaults {
        let model = models.iter().find(|m| m.ir.name == d.model).ok_or_else(|| format!("unknown query-default model {}", d.model))?;
        if defaults.iter().filter(|x| x.model == d.model).count() != 1 { return Err(format!("duplicate query defaults for {}", d.model)); }
        if let Some(fields) = &d.fields {
            let mut seen = std::collections::HashSet::new();
            for field in fields { model.field(field)?; if !seen.insert(field) { return Err("duplicate default selected field".into()); } }
        }
        let filter = d.filter.clone().map(serde_json::from_value::<Expr>).transpose().map_err(|e| format!("{} default filter: {e}", d.model))?;
        if let Some(filter) = &filter { validate_filter(model, filter)?; }
        filters.push(filter);
        for path in &d.related {
            let mut current = model;
            if path.is_empty() { return Err("empty default relation path".into()); }
            for hop in path {
                let (r, _) = current.relation(hop)?;
                if r.kind != crate::ir::RelKind::One { return Err("default eager loading requires to-one references".into()); }
                current = models.iter().find(|m| m.ir.name == r.target).expect("validated relation");
            }
        }
    }
    fn visit(models: &[Model], defaults: &[QueryDefaults], name: &str, stack: &mut Vec<String>) -> Result<(), String> {
        if stack.iter().any(|n| n == name) { return Err(format!("recursive default loading: {} -> {name}", stack.join(" -> "))); }
        stack.push(name.into());
        if let Some(d) = defaults.iter().find(|d| d.model == name) {
            for path in &d.related {
                let mut model = models.iter().find(|m| m.ir.name == name).expect("validated model");
                for hop in path { let (r, _) = model.relation(hop)?; model = models.iter().find(|m| m.ir.name == r.target).expect("validated target"); visit(models, defaults, &model.ir.name, stack)?; }
            }
        }
        stack.pop(); Ok(())
    }
    for d in defaults { visit(models, defaults, &d.model, &mut vec![])?; }
    for (d, filter) in defaults.iter().zip(filters) {
        let model = models.iter_mut().find(|m| m.ir.name == d.model).expect("validated model");
        model.query_defaults = PreparedDefaults { filter, fields: d.fields.clone(), related: d.related.clone() };
    }
    Ok(())
}
fn validate_filter(model: &Model, filter: &Expr) -> Result<(), String> {
    match filter {
        Expr::Col { path, name } if path.is_empty() => { model.field(name)?; }
        Expr::Const { .. } | Expr::Int { .. } | Expr::Text { .. } => {}
        Expr::Cmp { l, r, .. } => { validate_filter(model, l)?; validate_filter(model, r)?; }
        Expr::And { items } | Expr::Or { items } => for item in items { validate_filter(model, item)?; },
        Expr::Not { item } | Expr::IsNull { item, .. } => validate_filter(model, item)?,
        _ => return Err(format!("{}: default filters support root columns, literals, comparisons and boolean composition", model.ir.name)),
    }
    Ok(())
}

/// Finite eager-loading expansion, with prefixes before descendants.
pub fn expanded_related(models: &[Model], root: usize) -> Result<Vec<Vec<String>>, String> {
    fn expand(models: &[Model], model: usize, prefix: &[String], out: &mut Vec<Vec<String>>) -> Result<(), String> {
        for path in &models[model].query_defaults.related {
            let mut current = model;
            let mut full = prefix.to_vec();
            for hop in path {
                current = models[current].relation(hop)?.1;
                full.push(hop.clone());
                if !out.contains(&full) { out.push(full.clone()); }
                expand(models, current, &full, out)?;
            }
        }
        Ok(())
    }
    let mut out = vec![]; expand(models, root, &[], &mut out)?; Ok(out)
}
