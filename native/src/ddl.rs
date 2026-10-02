//! `CREATE TABLE` / `DROP TABLE` straight from the schema. A development convenience
//! until the migration engine exists.

use sea_orm::sea_query::{
    Alias, ColumnDef, Expr as SExpr, ForeignKey, ForeignKeyAction, Index, IndexCreateStatement,
    Table, TableCreateStatement, TableDropStatement,
};

use crate::ir::{ColType, OnDelete};
use crate::schema::{Result, Schema};

/// Models ordered so every foreign key target is created before the table using it.
fn creation_order(schema: &Schema) -> Result<Vec<usize>> {
    let n = schema.models.len();
    let (mut order, mut state) = (Vec::with_capacity(n), vec![0u8; n]); // 0 new, 1 visiting, 2 done
    fn visit(s: &Schema, i: usize, state: &mut [u8], order: &mut Vec<usize>) -> Result<()> {
        match state[i] {
            2 => return Ok(()),
            1 => return Err(format!("foreign key cycle through model {}", s.models[i].ir.name)),
            _ => {}
        }
        state[i] = 1;
        for r in s.models[i].ir.relations.iter().filter(|r| r.foreign_key) {
            let t = s.model_idx(&r.target)?;
            if t != i {
                visit(s, t, state, order)?;
            }
        }
        state[i] = 2;
        order.push(i);
        Ok(())
    }
    for i in 0..n {
        visit(schema, i, &mut state, &mut order)?;
    }
    Ok(order)
}

fn default_expr(v: &serde_json::Value) -> Result<SExpr> {
    Ok(match v {
        serde_json::Value::Bool(b) => SExpr::val(*b),
        serde_json::Value::String(s) => SExpr::val(s.clone()),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => SExpr::val(i),
            None => SExpr::val(n.as_f64().ok_or("bad numeric default")?),
        },
        other => return Err(format!("unsupported default {other}")),
    })
}

pub fn create_statements(schema: &Schema) -> Result<(Vec<TableCreateStatement>, Vec<IndexCreateStatement>)> {
    let (mut tables, mut indexes) = (vec![], vec![]);
    for i in creation_order(schema)? {
        let m = &schema.models[i];
        let mut t = Table::create();
        t.table(Alias::new(m.table())).if_not_exists();
        for f in m.fields() {
            let mut c = ColumnDef::new(Alias::new(&f.column));
            match f.ty {
                ColType::BigInt => c.big_integer(),
                ColType::Int => c.integer(),
                ColType::Float => c.double(),
                ColType::Bool => c.boolean(),
                ColType::String => match f.max_length {
                    Some(n) => c.string_len(n),
                    None => c.string(),
                },
                ColType::Text => c.text(),
                ColType::DateTime => c.timestamp_with_time_zone(),
                ColType::Date => c.date(),
            };
            if !f.nullable {
                c.not_null();
            }
            if f.primary_key {
                c.primary_key();
            }
            if f.auto_increment {
                c.auto_increment();
            }
            if f.unique {
                c.unique_key();
            }
            if f.default_now {
                c.default(SExpr::cust("now()"));
            } else if let Some(v) = &f.default {
                c.default(default_expr(v)?);
            }
            t.col(c);
            if f.index && !f.unique && !f.primary_key {
                indexes.push(
                    Index::create()
                        .if_not_exists()
                        .name(format!("ix_{}_{}", m.table(), f.column))
                        .table(Alias::new(m.table()))
                        .col(Alias::new(&f.column))
                        .to_owned(),
                );
            }
        }
        for r in m.ir.relations.iter().filter(|r| r.foreign_key) {
            let target = schema.model(schema.model_idx(&r.target)?);
            let from = &m.field(&r.from)?.column;
            let mut fk = ForeignKey::create();
            fk.name(format!("fk_{}_{}", m.table(), from))
                .from(Alias::new(m.table()), Alias::new(from))
                .to(Alias::new(target.table()), Alias::new(&target.field(&r.to)?.column));
            if let Some(action) = r.on_delete {
                fk.on_delete(match action {
                    OnDelete::Cascade => ForeignKeyAction::Cascade,
                    OnDelete::SetNull => ForeignKeyAction::SetNull,
                    OnDelete::Restrict => ForeignKeyAction::Restrict,
                    OnDelete::NoAction => ForeignKeyAction::NoAction,
                });
            }
            t.foreign_key(&mut fk);
        }
        tables.push(t);
    }
    Ok((tables, indexes))
}

pub fn drop_statements(schema: &Schema) -> Result<Vec<TableDropStatement>> {
    Ok(creation_order(schema)?
        .into_iter()
        .rev()
        .map(|i| Table::drop().table(Alias::new(schema.models[i].table())).if_exists().cascade().to_owned())
        .collect())
}
