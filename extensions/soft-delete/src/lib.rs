//! `@soft_delete.deleted_at`: the ORM soft-deletes rows instead of deleting them.
//! Database mode adds row triggers, so a raw `DELETE` soft-deletes too and the soft delete
//! cascades to soft-delete children. Execution requires the `soft-delete` capability.
use orm_contracts::{dialect::Dialect, extension::Declaration, ir::{ColType, ModelIr, OnDelete, SchemaIr, TriggerIr}, tracking::{Mode, SoftDelete}};

fn error(d: &Declaration, message: impl std::fmt::Display) -> String {
    format!("{}:{}:{}: @{}: {message}", d.location.file, d.location.line, d.location.column, d.attribute)
}

fn ident(s: &str) -> String { format!("\"{}\"", s.replace('"', "\"\"")) }

fn column<'m>(model: &'m ModelIr, field: &str) -> Result<&'m str, String> {
    model.fields.iter().find(|f| f.name == field).map(|f| f.column.as_str()).ok_or_else(|| format!("{} has no field {field}", model.name))
}

fn trigger(name: &str, value: serde_json::Value) -> Result<TriggerIr, String> {
    let mut value = value;
    value["name"] = name.into();
    serde_json::from_value(value).map_err(|e| e.to_string())
}

/// `BEFORE DELETE` of a live row: set the field and cancel the delete. A row that is
/// already soft-deleted is deleted.
fn delete_trigger(dialect: Dialect, model: &ModelIr, field: &str) -> Result<TriggerIr, String> {
    let pk = model.fields.iter().find(|f| f.primary_key).ok_or_else(|| format!("{} has no primary key", model.name))?;
    let (c, t, pk) = (ident(column(model, field)?), ident(&model.table), ident(&pk.column));
    let body = match dialect {
        Dialect::Postgres => format!("BEGIN UPDATE {t} SET {c} = now() WHERE {pk} = OLD.{pk}; RETURN NULL; END;"),
        Dialect::Sqlite => format!("UPDATE {t} SET {c} = CURRENT_TIMESTAMP WHERE {pk} = OLD.{pk}; SELECT RAISE(IGNORE)"),
    };
    trigger("soft_delete", serde_json::json!({"timing": "before", "events": ["delete"], "when": format!("OLD.{c} IS NULL"), "body": body}))
}

/// `AFTER UPDATE OF` the field, when a row becomes soft-deleted: soft-delete the live rows
/// of each soft-delete child whose relation cascades deletes. A child is
/// `(table, soft-delete column, foreign key column, referenced parent column)`.
fn cascade_trigger(dialect: Dialect, parent: &ModelIr, field: &str, children: &[(String, String, String, String)]) -> Result<TriggerIr, String> {
    let c = ident(column(parent, field)?);
    let statements: Vec<String> = children.iter().map(|(table, child, key, to)| {
        let (table, child, key, to) = (ident(table), ident(child), ident(key), ident(to));
        format!("UPDATE {table} SET {child} = NEW.{c} WHERE {key} = NEW.{to} AND {child} IS NULL")
    }).collect();
    let body = match dialect {
        Dialect::Postgres => format!("BEGIN {}; RETURN NULL; END;", statements.join("; ")),
        Dialect::Sqlite => statements.join("; "),
    };
    trigger("soft_delete_cascade", serde_json::json!({"timing": "after", "events": ["update"], "update_of": [field],
        "when": format!("OLD.{c} IS NULL AND NEW.{c} IS NOT NULL"), "body": body}))
}

pub fn prepare(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations: Vec<_> = ir.behavior.declarations.iter().filter(|d| !d.lowered && d.attribute.starts_with("soft_delete.")).cloned().collect();
    let mut added = vec![];
    for d in &declarations {
        if d.attribute != "soft_delete.deleted_at" { return Err(error(d, "unknown soft_delete attribute")); }
        let name = d.field.clone().ok_or_else(|| error(d, "is a field attribute"))?;
        let mode = Mode::parse(d.arguments.get("mode")).map_err(|e| error(d, e))?;
        let model = ir.models.iter().find(|m| m.name == d.model).ok_or_else(|| error(d, "unknown model"))?;
        let field = model.fields.iter().find(|f| f.name == name).ok_or_else(|| error(d, "unknown field"))?;
        if field.ty != ColType::DateTime || field.array || !field.nullable { return Err(error(d, format!("{}.{name} must be a nullable DateTime field", d.model))); }
        if ir.behavior.soft_delete.iter().any(|s| s.model == d.model) { return Err(error(d, format!("{} has more than one soft-delete field", d.model))); }
        ir.behavior.soft_delete.push(SoftDelete { model: d.model.clone(), field: name, mode });
        added.push(d.model.clone());
    }
    let dialect = ir.dialect;
    for s in ir.behavior.soft_delete.clone().iter().filter(|s| s.mode == Mode::Database && added.contains(&s.model)) {
        // Children: soft-delete models with a cascading foreign key to this model.
        let mut children = vec![];
        for child in &ir.models {
            let Some(child_field) = ir.behavior.soft_delete.iter().find(|c| c.model == child.name) else { continue };
            for r in child.relations.iter().filter(|r| r.foreign_key && r.target == s.model && r.on_delete == Some(OnDelete::Cascade)) {
                children.push((child.table.clone(), column(child, &child_field.field)?.to_owned(), column(child, &r.from)?.to_owned(), r.to.clone()));
            }
        }
        let models = ir.models.iter_mut().chain(ir.behavior.storage.iter_mut().flat_map(|st| st.models.iter_mut()));
        for model in models.filter(|m| m.name == s.model) {
            let mut triggers = vec![delete_trigger(dialect, model, &s.field)?];
            if !children.is_empty() {
                let resolved = children.iter().map(|(t, c, k, to)| Ok((t.clone(), c.clone(), k.clone(), column(model, to)?.to_owned()))).collect::<Result<Vec<_>, String>>()?;
                triggers.push(cascade_trigger(dialect, model, &s.field, &resolved)?);
            }
            for t in triggers {
                if model.triggers.iter().any(|x| x.name == t.name) { return Err(format!("{} already has a trigger {}", model.name, t.name)); }
                model.triggers.push(t);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(dialect: &str, mode: &str, nullable: bool) -> SchemaIr {
        let at = |model: &str, line: u32| serde_json::json!({"attribute": "soft_delete.deleted_at", "model": model, "field": "deleted", "positional": [], "arguments": {"mode": mode}, "location": {"file": "schema.prisma", "line": line, "column": 3}});
        serde_json::from_value(serde_json::json!({
            "dialect": dialect,
            "models": [
                {"name": "Author", "table": "authors", "fields": [
                    {"name": "id", "column": "id", "type": "int", "primary_key": true},
                    {"name": "deleted", "column": "deleted_at", "type": "date_time", "nullable": nullable}
                ]},
                {"name": "Book", "table": "books", "fields": [
                    {"name": "id", "column": "id", "type": "int", "primary_key": true},
                    {"name": "author_id", "column": "writer", "type": "int"},
                    {"name": "deleted", "column": "removed_at", "type": "date_time", "nullable": true}
                ], "relations": [{"name": "author", "kind": "one", "target": "Author", "from": "author_id", "to": "id", "foreign_key": true, "on_delete": "cascade"}]},
                {"name": "Note", "table": "notes", "fields": [
                    {"name": "id", "column": "id", "type": "int", "primary_key": true},
                    {"name": "author_id", "column": "author_id", "type": "int"}
                ], "relations": [{"name": "author", "kind": "one", "target": "Author", "from": "author_id", "to": "id", "foreign_key": true, "on_delete": "cascade"}]}
            ],
            "behavior": {"schema_contract": 1, "declarations": [at("Author", 3), at("Book", 9)]}
        })).unwrap()
    }

    fn prepared(ir: &mut SchemaIr) -> Result<(), String> {
        orm_contracts::extension::capture_storage(ir)?;
        prepare(ir)
    }

    fn migration(ir: &SchemaIr) -> Vec<String> {
        let storage = serde_json::json!({"dialect": ir.dialect, "models": ir.behavior.storage.as_ref().unwrap().models});
        let schema = orm_core::schema::Schema::from_ir(serde_json::from_value(storage).unwrap()).unwrap();
        orm_core::migrate::plan(&schema, &Default::default()).unwrap().up.into_iter().map(|s| s.sql).collect()
    }

    #[test]
    fn preparation_changes_only_its_declared_effects_once() {
        let effects = orm_contracts::extension::pass_effects(&schema("postgres", "database", true), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.soft_delete", "behavior.storage", "models"]);
        let effects = orm_contracts::extension::pass_effects(&schema("postgres", "application", true), prepare).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.soft_delete"]);
    }

    #[test]
    fn database_mode_adds_delete_and_cascade_triggers() {
        let mut ir = schema("postgres", "database", true);
        prepared(&mut ir).unwrap();
        assert_eq!(ir.behavior.soft_delete[1], SoftDelete { model: "Book".into(), field: "deleted".into(), mode: Mode::Database });
        for author in [&ir.models[0], &ir.behavior.storage.as_ref().unwrap().models[0]] {
            let names: Vec<_> = author.triggers.iter().map(|t| t.name.as_str()).collect();
            assert_eq!(names, ["soft_delete", "soft_delete_cascade"]);
            assert_eq!(author.triggers[0].when.as_deref(), Some("OLD.\"deleted_at\" IS NULL"));
            assert_eq!(author.triggers[0].body.as_deref(), Some("BEGIN UPDATE \"authors\" SET \"deleted_at\" = now() WHERE \"id\" = OLD.\"id\"; RETURN NULL; END;"));
            // Note has no soft delete, so only Book is soft-deleted with its author.
            assert_eq!(author.triggers[1].body.as_deref(), Some("BEGIN UPDATE \"books\" SET \"removed_at\" = NEW.\"deleted_at\" WHERE \"writer\" = NEW.\"id\" AND \"removed_at\" IS NULL; RETURN NULL; END;"));
        }
        assert_eq!(ir.models[1].triggers.len(), 1);
        let up = migration(&ir);
        assert!(up.contains(&"CREATE TRIGGER \"soft_delete\" BEFORE DELETE ON \"authors\" FOR EACH ROW WHEN (OLD.\"deleted_at\" IS NULL) EXECUTE FUNCTION \"authors_soft_delete\"()".to_owned()), "{up:?}");
        assert!(up.contains(&"CREATE TRIGGER \"soft_delete_cascade\" AFTER UPDATE OF \"deleted_at\" ON \"authors\" FOR EACH ROW WHEN (OLD.\"deleted_at\" IS NULL AND NEW.\"deleted_at\" IS NOT NULL) EXECUTE FUNCTION \"authors_soft_delete_cascade\"()".to_owned()), "{up:?}");
    }

    #[test]
    fn application_mode_adds_no_ddl() {
        let mut ir = schema("sqlite", "application", true);
        prepared(&mut ir).unwrap();
        assert_eq!(ir.behavior.soft_delete.len(), 2);
        assert!(ir.behavior.storage.as_ref().unwrap().models.iter().all(|m| m.triggers.is_empty()));
    }

    #[test]
    fn wrong_declarations_fail_with_their_location() {
        let err = prepared(&mut schema("postgres", "database", false)).unwrap_err();
        assert_eq!(err, "schema.prisma:3:3: @soft_delete.deleted_at: Author.deleted must be a nullable DateTime field");
        let err = prepared(&mut schema("postgres", "later", true)).unwrap_err();
        assert!(err.ends_with("mode must be \"database\" or \"application\""), "{err}");
    }

    #[test]
    fn sqlite_delete_soft_deletes_cascades_and_purges_a_deleted_row() {
        let mut ir = schema("sqlite", "database", true);
        prepared(&mut ir).unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for statement in migration(&ir) { db.execute_batch(&statement).unwrap(); }
        db.execute_batch("INSERT INTO authors (id) VALUES (1), (2); INSERT INTO books (id, writer) VALUES (1, 1), (2, 2); INSERT INTO notes (id, author_id) VALUES (1, 1)").unwrap();
        let deleted = |db: &rusqlite::Connection, sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(db.execute("DELETE FROM authors WHERE id = 1", []).unwrap(), 0);
        assert_eq!(deleted(&db, "SELECT count(*) FROM authors WHERE deleted_at IS NOT NULL"), 1);
        assert_eq!(deleted(&db, "SELECT count(*) FROM books WHERE removed_at IS NOT NULL AND writer = 1"), 1);
        assert_eq!(deleted(&db, "SELECT count(*) FROM books WHERE removed_at IS NULL"), 1);
        assert_eq!(deleted(&db, "SELECT count(*) FROM notes"), 1);
        db.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        assert_eq!(db.execute("DELETE FROM authors WHERE id = 1", []).unwrap(), 1);
        assert_eq!(deleted(&db, "SELECT count(*) FROM authors"), 1);
    }
}
