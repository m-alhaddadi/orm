//! `@timestamps.updated_at`: the ORM sets the field on each update. Database mode adds a
//! row trigger so other writers set it too. Execution requires the `updated-at` capability.
use orm_contracts::{dialect::Dialect, extension::Declaration, ir::{ColType, ModelIr, SchemaIr, TriggerIr}, tracking::{Mode, UpdatedAt}};

fn error(d: &Declaration, message: impl std::fmt::Display) -> String {
    format!("{}:{}:{}: @{}: {message}", d.location.file, d.location.line, d.location.column, d.attribute)
}

fn ident(s: &str) -> String { format!("\"{}\"", s.replace('"', "\"\"")) }

/// The trigger that sets `column` on an update that leaves it unchanged, so a written value
/// wins. SQLite can't assign `NEW`, so its trigger updates the row after the update.
fn trigger(dialect: Dialect, model: &ModelIr, column: &str) -> Result<TriggerIr, String> {
    let pk = model.fields.iter().find(|f| f.primary_key).ok_or_else(|| format!("{} has no primary key", model.name))?;
    let c = ident(column);
    let value = match dialect {
        Dialect::Postgres => serde_json::json!({"timing": "before", "when": format!("NEW.{c} IS NOT DISTINCT FROM OLD.{c}"),
            "body": format!("BEGIN NEW.{c} := now(); RETURN NEW; END;")}),
        Dialect::Sqlite => serde_json::json!({"timing": "after", "when": format!("NEW.{c} IS OLD.{c}"),
            "body": format!("UPDATE {} SET {c} = CURRENT_TIMESTAMP WHERE {pk} = NEW.{pk}", ident(&model.table), pk = ident(&pk.column))}),
    };
    let mut value = value;
    value["name"] = format!("updated_at_{column}").into();
    value["events"] = serde_json::json!(["update"]);
    serde_json::from_value(value).map_err(|e| e.to_string())
}

pub fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations: Vec<_> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute.starts_with("timestamps.")).cloned().collect();
    for d in &declarations {
        if d.attribute != "timestamps.updated_at" { return Err(error(d, "unknown timestamps attribute")); }
        let name = d.field.clone().ok_or_else(|| error(d, "is a field attribute"))?;
        let mode = Mode::parse(d.arguments.get("mode")).map_err(|e| error(d, e))?;
        let model = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| error(d, "unknown model"))?;
        let field = model.fields.iter().find(|f| f.name == name).ok_or_else(|| error(d, "unknown field"))?;
        if field.ty != ColType::DateTime || field.array { return Err(error(d, format!("{}.{name} must be a DateTime field", d.model))); }
        if ir.behavior.updated_at.iter().any(|u| u.model == d.model && u.field == name) { return Err(error(d, "is given twice")); }
        if mode == Mode::Database {
            let column = field.column.clone();
            let models = ir.models.iter_mut().chain(ir.behavior.storage.iter_mut().flat_map(|s| s.models.iter_mut()));
            for model in models.filter(|m| m.name == d.model) {
                let trigger = trigger(ir.dialect, model, &column)?;
                if model.triggers.iter().any(|t| t.name == trigger.name) { return Err(error(d, format!("{} already has a trigger {}", d.model, trigger.name))); }
                model.triggers.push(trigger);
            }
        }
        ir.behavior.updated_at.push(UpdatedAt { model: d.model.clone(), field: name, mode });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(dialect: &str, mode: Option<&str>, ty: &str) -> SchemaIr {
        let arguments = mode.map(|m| serde_json::json!({"mode": m})).unwrap_or_else(|| serde_json::json!({}));
        serde_json::from_value(serde_json::json!({
            "dialect": dialect,
            "models": [{"name": "Post", "table": "posts", "fields": [
                {"name": "id", "column": "id", "type": "int", "primary_key": true},
                {"name": "changed", "column": "changed_at", "type": ty, "default_now": true}
            ]}],
            "behavior": {"schema_contract": 1, "declarations": [
                {"attribute": "timestamps.updated_at", "model": "Post", "field": "changed", "positional": [], "arguments": arguments, "location": {"file": "schema.prisma", "line": 4, "column": 3}}
            ]}
        })).unwrap()
    }

    fn prepared(ir: &mut SchemaIr) -> Result<(), String> {
        orm_contracts::extension::capture_storage(ir)?;
        prepare(ir)
    }

    #[test]
    fn preparation_changes_only_its_declared_effects_once() {
        let effects = orm_contracts::extension::pass_effects(&schema("postgres", None, "date_time"), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.storage", "behavior.updated_at", "models"]);
        let effects = orm_contracts::extension::pass_effects(&schema("postgres", Some("application"), "date_time"), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.updated_at"]);
    }

    #[test]
    fn database_mode_adds_the_trigger_to_logical_and_physical_models() {
        let mut ir = schema("postgres", None, "date_time");
        prepared(&mut ir).unwrap();
        assert_eq!(ir.behavior.updated_at, [UpdatedAt { model: "Post".into(), field: "changed".into(), mode: Mode::Database }]);
        for model in [&ir.models[0], &ir.behavior.storage.as_ref().unwrap().models[0]] {
            let t = &model.triggers[0];
            assert_eq!((t.name.as_str(), t.body.as_deref()), ("updated_at_changed_at", Some("BEGIN NEW.\"changed_at\" := now(); RETURN NEW; END;")));
            assert_eq!(serde_json::to_value(t.timing).unwrap(), "before");
            assert_eq!(t.when.as_deref(), Some("NEW.\"changed_at\" IS NOT DISTINCT FROM OLD.\"changed_at\""));
        }
        let mut ir = schema("sqlite", None, "date_time");
        prepared(&mut ir).unwrap();
        let t = &ir.behavior.storage.as_ref().unwrap().models[0].triggers[0];
        assert_eq!(serde_json::to_value(t.timing).unwrap(), "after");
        assert_eq!(t.when.as_deref(), Some("NEW.\"changed_at\" IS OLD.\"changed_at\""));
        assert_eq!(t.body.as_deref(), Some("UPDATE \"posts\" SET \"changed_at\" = CURRENT_TIMESTAMP WHERE \"id\" = NEW.\"id\""));
    }

    #[test]
    fn application_mode_adds_no_ddl() {
        let mut ir = schema("sqlite", Some("application"), "date_time");
        prepared(&mut ir).unwrap();
        assert_eq!(ir.behavior.updated_at[0].mode, Mode::Application);
        assert!(ir.models[0].triggers.is_empty() && ir.behavior.storage.as_ref().unwrap().models[0].triggers.is_empty());
    }

    #[test]
    fn wrong_declarations_fail_with_their_location() {
        let err = prepared(&mut schema("postgres", None, "string")).unwrap_err();
        assert_eq!(err, "schema.prisma:4:3: @timestamps.updated_at: Post.changed must be a DateTime field");
        let err = prepared(&mut schema("postgres", Some("trigger"), "date_time")).unwrap_err();
        assert!(err.ends_with("mode must be \"database\" or \"application\""), "{err}");
    }

    /// The migration of the physical schema, as the host plans it.
    fn migration(ir: &SchemaIr) -> Vec<String> {
        let storage = serde_json::json!({"dialect": ir.dialect, "models": ir.behavior.storage.as_ref().unwrap().models});
        let schema = orm_core::schema::Schema::from_ir(serde_json::from_value(storage).unwrap()).unwrap();
        let plan = orm_core::migrate::plan(&schema, &Default::default()).unwrap();
        plan.up.into_iter().map(|s| s.sql).collect()
    }

    #[test]
    fn postgres_migration_creates_the_trigger_function_and_trigger() {
        let mut ir = schema("postgres", None, "date_time");
        prepared(&mut ir).unwrap();
        let up = migration(&ir);
        assert!(up.iter().any(|s| s.starts_with("CREATE OR REPLACE FUNCTION \"posts_updated_at_changed_at\"() RETURNS trigger")), "{up:?}");
        assert!(up.contains(&"CREATE TRIGGER \"updated_at_changed_at\" BEFORE UPDATE ON \"posts\" FOR EACH ROW WHEN (NEW.\"changed_at\" IS NOT DISTINCT FROM OLD.\"changed_at\") EXECUTE FUNCTION \"posts_updated_at_changed_at\"()".to_owned()), "{up:?}");
    }

    #[test]
    fn sqlite_trigger_sets_the_field_for_other_writers_only() {
        let mut ir = schema("sqlite", None, "date_time");
        prepared(&mut ir).unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for statement in migration(&ir) { db.execute_batch(&statement).unwrap(); }
        db.execute_batch("INSERT INTO posts (id, changed_at) VALUES (1, '2000-01-01 00:00:00')").unwrap();
        let changed = |db: &rusqlite::Connection| db.query_row("SELECT changed_at FROM posts", [], |r| r.get::<_, String>(0)).unwrap();
        db.execute_batch("UPDATE posts SET id = 1").unwrap();
        assert_ne!(changed(&db), "2000-01-01 00:00:00");
        db.execute_batch("UPDATE posts SET changed_at = '2001-01-01 00:00:00'").unwrap();
        assert_eq!(changed(&db), "2001-01-01 00:00:00");
    }
}
