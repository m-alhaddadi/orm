//! ContentType synthesis and explicit identity generation. Ordinary compilation is read-only.
pub use orm_contracts::identity::*;
use std::{collections::{BTreeSet, HashMap}, path::{Path, PathBuf}};
use crate::ir::{EnumIr, EnumStorage, EnumValueIr, SchemaIr};

pub fn manifest_path(schema: &Path) -> PathBuf { schema.with_extension("identities.json") }

pub fn reconcile(previous: &IdentityManifest, models: &[String], renames: &[(String, String)], restores: &[String]) -> Result<IdentityManifest, String> {
    previous.validate()?;
    let names: BTreeSet<_> = models.iter().cloned().collect();
    if names.len() != models.len() { return Err("duplicate model names in application schema".into()); }
    let mut next = previous.clone();
    let mut used = BTreeSet::new();
    for (old, new) in renames {
        if old == new || names.contains(old) || !names.contains(new) || !used.insert(old) || !used.insert(new) {
            return Err(format!("invalid identity rename {old}={new}; old must be removed, new must exist, and each name may be used once"));
        }
        if next.active().any(|e| &e.model == new) { return Err(format!("{new} already has an active identity")); }
        let entry = next.entries.iter_mut().find(|e| !e.retired && &e.model == old)
            .ok_or_else(|| format!("no active identity for rename source {old}"))?;
        if !entry.previous_names.contains(old) { entry.previous_names.push(old.clone()); }
        entry.previous_names.retain(|n| n != new);
        entry.model = new.clone();
    }
    let mut restored = BTreeSet::new();
    for name in restores {
        if !names.contains(name) || !restored.insert(name) || next.active().any(|e| &e.model == name) {
            return Err(format!("invalid identity restoration {name}"));
        }
        let candidates: Vec<_> = next.entries.iter().enumerate().filter(|(_, e)| e.retired && &e.model == name).map(|(i, _)| i).collect();
        match candidates.as_slice() {
            [i] => next.entries[*i].retired = false,
            _ => return Err(format!("restoration {name} needs exactly one retired identity; select/repair identity metadata explicitly")),
        }
    }
    for entry in &mut next.entries { if !names.contains(&entry.model) { entry.retired = true; } }
    let missing: Vec<_> = names.iter().filter(|name| !next.active().any(|e| &e.model == *name)).cloned().collect();
    let mut max = next.entries.iter().map(|e| e.id).max().unwrap_or(0);
    for model in missing {
        max = max.checked_add(1).ok_or("ContentType IDs exhausted; cannot allocate above signed 32-bit maximum")?;
        next.entries.push(ModelIdentity { id: max, model, retired: false, previous_names: vec![] });
    }
    next.entries.sort_by_key(|e| e.id);
    previous.validate_successor(&next)?;
    Ok(next)
}

pub fn content_type(manifest: &IdentityManifest) -> Result<EnumIr, String> {
    manifest.validate()?;
    let mut values: Vec<_> = manifest.active().map(|e| EnumValueIr { name: e.model.clone(), value: e.id.into() }).collect();
    values.sort_by_key(|v| v.value.as_i64());
    if values.is_empty() { return Err("ContentType needs at least one concrete stored model".into()); }
    Ok(EnumIr { name: "ContentType".into(), db_name: "contenttype".into(), storage: EnumStorage::Int, values, comment: None })
}

pub fn validate(ir: &SchemaIr) -> Result<(), String> {
    let Some(manifest) = &ir.identities else { return Ok(()); };
    manifest.validate()?;
    let proxies: BTreeSet<_> = ir.behavior.proxy_models.iter().map(|p| p.model.as_str()).chain(ir.behavior.declarations.iter().filter(|d| d.attribute == "proxy.of").map(|d| d.model.as_str())).collect();
    let actual: BTreeSet<_> = ir.models.iter().filter(|m| !proxies.contains(m.name.as_str())).map(|m| m.name.as_str()).collect();
    let expected: BTreeSet<_> = manifest.active().map(|e| e.model.as_str()).collect();
    if actual != expected { return Err("stale identity manifest: active models differ from the application schema; run orm identities".into()); }
    for proxy in &ir.behavior.proxy_models {
        let view = ir.models.iter().find(|m| m.name == proxy.model).ok_or_else(|| format!("unknown proxy identity {}", proxy.model))?;
        let owner = ir.models.iter().find(|m| m.name == proxy.storage_owner && expected.contains(m.name.as_str()))
            .ok_or_else(|| format!("proxy {} needs a resolved concrete storage identity", proxy.model))?;
        if view.table != owner.table { return Err(format!("proxy {} does not share storage identity {}", proxy.model, proxy.storage_owner)); }
    }
    let wanted = content_type(manifest)?;
    let enums: Vec<_> = ir.enums.iter().filter(|e| e.name == "ContentType").collect();
    if enums != vec![&wanted] { return Err("ContentType does not match frozen model identities; regenerate the compiled schema".into()); }
    validate_names(ir)?;
    Ok(())
}

fn validate_names(ir: &SchemaIr) -> Result<(), String> {
    let mut tables = HashMap::new();
    let mut symbols = HashMap::new();
    for reserved in ["ContentType", "Model", "QuerySet", "Registry", "define", "Decimal", "UUID", "datetime", "date", "IntEnum", "Enum", "Instance", "ModelClass", "Column", "RelationPath", "RelatedSet", "ManyRelatedSet", "Hop", "Compat", "Expression", "SchemaIR", "JsonValue", "In", "Many", "Date", "Object", "Promise", "Readonly"] {
        symbols.insert(reserved.to_string(), "generated/runtime symbol".to_string());
    }
    let proxies: BTreeSet<_> = ir.behavior.proxy_models.iter().map(|p| p.model.as_str()).chain(ir.behavior.declarations.iter().filter(|d| d.attribute == "proxy.of").map(|d| d.model.as_str())).collect();
    for model in &ir.models {
        if ["class", "def", "import", "from", "return", "for", "while", "if", "else", "try", "except", "finally", "with", "as", "pass", "lambda", "async", "await", "None", "True", "False", "global", "nonlocal", "yield", "raise", "assert", "del", "elif", "break", "continue", "and", "or", "not", "in", "is", "enum", "interface", "export", "default", "new", "const", "let", "function", "extends", "implements", "null"].contains(&model.name.as_str()) {
            return Err(format!("model {} is a reserved language name", model.name));
        }
        // PostgreSQL identifiers are quoted and truncated at a UTF-8 boundary;
        // SQLite identifiers compare ASCII case-insensitively.
        let table = if ir.dialect == crate::dialect::Dialect::Sqlite { model.table.to_ascii_lowercase() } else {
            let mut end = model.table.len().min(63);
            while !model.table.is_char_boundary(end) { end -= 1; }
            model.table[..end].to_owned()
        };
        if let Some(old) = if proxies.contains(model.name.as_str()) { None } else { tables.insert(table, &model.name) } {
            return Err(format!("models {old} and {} share a physical table identity", model.name));
        }
        for name in [model.name.clone(), format!("{}Insert", model.name), format!("{}Update", model.name), format!("{}UpdateRow", model.name), format!("{}QuerySet", model.name), format!("{}Model", model.name), format!("{}Spec", model.name), format!("{}Fields", model.name), format!("{}Path", model.name), format!("{}Data", model.name)] {
            if let Some(old) = symbols.insert(name.clone(), model.name.clone()) {
                return Err(format!("model {} generated name {name} collides with {old}", model.name));
            }
        }
        // Python Enum reserves sunder names and mro; names must work in both bindings.
        if !proxies.contains(model.name.as_str()) && (model.name.starts_with('_') || model.name == "mro") {
            return Err(format!("model {} cannot be a ContentType member", model.name));
        }
    }
    for e in ir.enums.iter().filter(|e| e.name != "ContentType") {
        if let Some(old) = symbols.insert(e.name.clone(), e.name.clone()) { return Err(format!("enum {} collides with {old}", e.name)); }
    }
    Ok(())
}

pub fn read(path: &Path) -> Result<IdentityManifest, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}; run orm identities to generate frozen IDs", path.display()))?;
    let manifest: IdentityManifest = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    manifest.validate()?;
    Ok(manifest)
}

/// Atomic replacement avoids partial manifests. No ordinary compile call uses it.
pub fn write(path: &Path, manifest: &IdentityManifest) -> Result<(), String> {
    use std::io::Write;
    manifest.validate()?;
    let text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())? + "\n";
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    let temporary = path.with_extension(format!("identities.tmp.{}.{stamp}", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temporary); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn names(values: &[&str]) -> Vec<String> { values.iter().map(|s| (*s).into()).collect() }
    #[test]
    fn allocation_retirement_reuse_and_restore() {
        let first = reconcile(&Default::default(), &names(&["Post", "Photo"]), &[], &[]).unwrap();
        assert_eq!(first.active().map(|e| (&*e.model, e.id)).collect::<Vec<_>>(), vec![("Photo", 1), ("Post", 2)]);
        let added = reconcile(&first, &names(&["Post", "Photo", "Account"]), &[], &[]).unwrap();
        assert_eq!(added.active().find(|e| e.model == "Account").unwrap().id, 3);
        let removed = reconcile(&added, &names(&["Photo", "Account"]), &[], &[]).unwrap();
        assert!(removed.entries.iter().find(|e| e.id == 2).unwrap().retired);
        let reused = reconcile(&removed, &names(&["Photo", "Account", "Post"]), &[], &[]).unwrap();
        assert_eq!(reused.active().find(|e| e.model == "Post").unwrap().id, 4);
        let restored = reconcile(&removed, &names(&["Photo", "Account", "Post"]), &[], &names(&["Post"])).unwrap();
        assert_eq!(restored.active().find(|e| e.model == "Post").unwrap().id, 2);
    }
    #[test]
    fn explicit_rename_keeps_identity() {
        let first = reconcile(&Default::default(), &names(&["Post"]), &[], &[]).unwrap();
        let renamed = reconcile(&first, &names(&["Article"]), &[("Post".into(), "Article".into())], &[]).unwrap();
        assert_eq!(renamed.entries[0].id, 1);
        assert_eq!(renamed.entries[0].previous_names, names(&["Post"]));
        first.validate_successor(&renamed).unwrap();
        let mut forged = renamed.clone(); forged.entries[0].previous_names.clear();
        assert!(first.validate_successor(&forged).unwrap_err().contains("reassigned"));
    }
    #[test]
    fn rename_chain_can_return_to_original_name() {
        let original = reconcile(&Default::default(), &names(&["Post"]), &[], &[]).unwrap();
        let article = reconcile(&original, &names(&["Article"]), &[("Post".into(), "Article".into())], &[]).unwrap();
        let entry = reconcile(&article, &names(&["Entry"]), &[("Article".into(), "Entry".into())], &[]).unwrap();
        let restored_name = reconcile(&entry, &names(&["Post"]), &[("Entry".into(), "Post".into())], &[]).unwrap();

        assert_eq!(restored_name.entries[0].id, original.entries[0].id);
        assert_eq!(restored_name.entries[0].model, "Post");
        assert_eq!(restored_name.entries[0].previous_names, names(&["Article", "Entry"]));
        for previous in [&original, &article, &entry] {
            previous.validate_successor(&restored_name).unwrap();
        }
    }
    #[test]
    fn rejects_conflicts_and_exhaustion() {
        let mut manifest = reconcile(&Default::default(), &names(&["Post"]), &[], &[]).unwrap();
        manifest.entries.push(manifest.entries[0].clone());
        assert!(manifest.validate().is_err());
        manifest.entries.pop(); manifest.entries[0].id = 0;
        assert!(manifest.validate().is_err());
        manifest.entries[0].id = i32::MAX;
        assert!(reconcile(&manifest, &names(&["Post", "Photo"]), &[], &[]).unwrap_err().contains("exhausted"));
        assert!(manifest.validate_successor(&Default::default()).is_err());
    }
    #[test]
    fn generation_and_split_compile_are_read_only() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.test-tmp").join(format!("orm-identities-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("schema.prisma");
        let post = "model Post {\nid Int @id\n@@map(\"posts\")\n}";
        let tag = "model Tag {\nid Int @id\ncontent_type ContentType\nobject_id Int\n}";
        std::fs::write(&path, format!("{post}\n{tag}")).unwrap();
        assert!(crate::dsl::compile_file(&path).unwrap_err().contains("orm identities"));
        let manifest = crate::dsl::generate_identities(&path, &[], &[]).unwrap();
        let before = std::fs::read(manifest_path(&path)).unwrap();
        let one = crate::dsl::compile_file(&path).unwrap();
        crate::dsl::check(one).unwrap();
        std::fs::write(root.join("post.prisma"), post).unwrap();
        std::fs::write(&path, format!("import \"post.prisma\"\n{tag}")).unwrap();
        let split = crate::dsl::compile_project_file(&path).unwrap();
        assert_eq!(split.ir.identities.as_ref(), Some(&manifest));
        assert!(split.inputs.contains(&manifest_path(&path)));
        assert!(split.units.iter().all(|u| u.enums.contains(&"ContentType".into())));
        let (_, schema) = crate::dsl::check(split.ir).unwrap();
        assert_eq!(crate::migrate::snapshot(&schema).unwrap().identities, Some(manifest));
        assert_eq!(std::fs::read(manifest_path(&path)).unwrap(), before);
        std::fs::write(&path, format!("{tag}\nmodel Account {{\nid Int @id\n}}\n{post}")).unwrap();
        assert!(crate::dsl::compile_file(&path).unwrap_err().contains("stale"));
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn ordinary_enum_named_content_type_does_not_activate_extension() {
        crate::dsl::check(crate::dsl::compile("enum ContentType {\nPost @value(7)\n@@storage(int)\n}\nmodel Tag {\nid Int @id\nt ContentType\n}", None).unwrap()).unwrap();
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    fn ir(dialect: &str) -> SchemaIr {
        let manifest = reconcile(&Default::default(), &["Post".into(), "Tag".into()], &[], &[]).unwrap();
        let generated = content_type(&manifest).unwrap();
        serde_json::from_value(serde_json::json!({"dialect":dialect,"identities":manifest,"enums":[generated],"models":[
            {"name":"Post","table":"posts","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]},
            {"name":"Tag","table":"tags","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"kind","column":"kind","type":"int","enum":"ContentType"}]}
        ]})).unwrap()
    }
    #[test]
    fn normalized_proxy_is_not_a_concrete_identity() {
        let mut ir = ir("sqlite");
        let mut view = serde_json::to_value(&ir.models[0]).unwrap(); view["name"] = "PublicPost".into();
        ir.models.push(serde_json::from_value(view).unwrap());
        ir.behavior.proxy_models.push(orm_contracts::extension::ProxyModel {
            model: "PublicPost".into(), parent: "Post".into(), storage_owner: "Post".into(), fields: vec![], defaults: Default::default(),
        });
        validate(&ir).unwrap();
        ir.behavior.proxy_models.clear();
        assert!(validate(&ir).unwrap_err().contains("stale"));
    }
    #[test]
    fn dialect_integer_ddl_and_snapshot_identity_reassignment() {
        for dialect in ["postgres", "sqlite"] {
            let (_, schema) = crate::dsl::check(ir(dialect)).unwrap();
            let ddl = crate::migrate::create_all(&schema).unwrap().join("\n");
            assert!(ddl.contains("IN (1, 2)"), "{ddl}");
            assert!(!ddl.contains("CREATE TYPE"));
            let previous = crate::migrate::snapshot(&schema).unwrap();
            let mut renamed = ir(dialect);
            renamed.models[0].name = "Article".into();
            renamed.identities = Some(reconcile(previous.identities.as_ref().unwrap(), &["Article".into(), "Tag".into()], &[("Post".into(), "Article".into())], &[]).unwrap());
            renamed.enums = vec![content_type(renamed.identities.as_ref().unwrap()).unwrap()];
            let (_, schema) = crate::dsl::check(renamed).unwrap();
            let plan = crate::migrate::plan(&schema, &previous).unwrap();
            assert!(plan.up.iter().any(|s| s.summary.contains("identities")));
            let mut forged = previous;
            forged.identities.as_mut().unwrap().entries[0].model = "WrongModel".into();
            assert!(crate::migrate::plan(&schema, &forged).unwrap_err().contains("reassigned"));
        }
    }
}
