//! A third-party logical rename. The physical column retains its identity.
use orm_contracts::ir::SchemaIr;

pub fn lower(ir: &mut SchemaIr) -> Result<(), String> {
    // A contribution independent of namespaced attributes exercises raw batches.
    for model in &mut ir.models {
        if model.name == "AutoLowered" && !ir.behavior.lowered_models.contains(&model.name) {
            model.comment = Some("prepared by example".into());
        }
    }
    for d in &ir.behavior.declarations {
        if d.attribute == "example.proxy" {
            let parent = d.arguments["parent"].as_str().ok_or("proxy parent requires a string")?;
            let parent = ir.models.iter().find(|m| m.name == parent).ok_or("unknown proxy parent")?;
            let fields = serde_json::from_value(serde_json::to_value(&parent.fields).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            let table = parent.table.clone();
            let m = ir.models.iter_mut().find(|m| m.name == d.model).ok_or("unknown proxy model")?;
            m.table = table;
            m.fields = fields;
            ir.behavior.storage.as_mut().ok_or("host must capture physical storage")?.models.retain(|m| m.name != d.model);
            continue;
        }
        if d.attribute == "example.hide" {
            let m = ir.models.iter_mut().find(|m| m.name == d.model).ok_or("unknown hide model")?;
            m.fields.retain(|f| Some(&f.name) != d.field.as_ref());
            continue;
        }
        if d.attribute != "example.rename" { continue; }
        let m = ir.models.iter_mut().find(|m| m.name == d.model).ok_or("unknown rename model")?;
        let old = d.field.as_ref().ok_or("rename requires a field")?;
        let new = d.arguments["to"].as_str().ok_or("rename requires a string")?;
        if m.fields.iter().any(|f| f.name == new) { return Err(format!("{}.{} already exists", m.name, new)); }
        if m.relations.iter().any(|r| r.from == *old || r.to == *old) { return Err("rename proof does not support relation key renames".into()); }
        let f = m.fields.iter_mut().find(|f| f.name == *old).ok_or("unknown rename field")?;
        f.name = new.into();
    }
    Ok(())
}
