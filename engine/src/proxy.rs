//! Concrete optional proxy behavior, inside existing planning/materialization.
use crate::db::{Cell, DbResult, RowSet};
use orm_core::{behavior::ResultShape, ir::ValueType, proxy::{Category, EnumScalar, PreparedField, PreparedProxy, Warning, Warnings}};

/// Check public instance columns only. An absent LEFT JOIN object is not a loaded
/// null field and produces no warning. Row order/count/values remain untouched.
pub fn inspect<'a>(proxy: &'a PreparedProxy, shape: Option<&ResultShape>, start: usize,
    presence: Option<(usize, ValueType)>, rows: &dyn RowSet, warnings: &mut Warnings<'a>) -> DbResult<()> {
    for field in &proxy.fields {
        let column = match shape {
            Some(shape) => match shape.fields.iter().find(|f| f.field.position == field.position && f.public).and_then(|f| f.physical) {
                Some(column) => column,
                None => continue,
            },
            None => start + field.position,
        };
        for row in 0..rows.len() {
            if let Some((column, ty)) = presence {
                if matches!(rows.cell(row, column, ty)?, Cell::Null) { continue; }
            }
            let cell = rows.cell(row, column, field.ty)?;
            if matches!(cell, Cell::Null) {
                if field.expected.non_null { warnings.record(proxy, field, Category::Null); }
            } else if field.allowed.is_some() && !accepts(field, &cell) {
                warnings.record(proxy, field, Category::EnumSubset);
            }
        }
    }
    Ok(())
}
fn accepts(field: &PreparedField, cell: &Cell<'_>) -> bool {
    match cell {
        Cell::Null => true,
        Cell::Text(value) => field.accepts_enum(EnumScalar::Text(value)),
        Cell::Int(value) => field.accepts_enum(EnumScalar::Int(i64::from(*value))),
        Cell::BigInt(value) => field.accepts_enum(EnumScalar::Int(*value)),
        Cell::Array(values) => values.iter().all(|v| accepts(field, v)),
        _ => false,
    }
}

/// Collect once for an operation. Projection/count results never acquire proxy diagnostics.
pub fn diagnostics(proxies: &[PreparedProxy], out: &crate::exec::Outcome) -> DbResult<Vec<Warning>> {
    use crate::{exec::{Fetched, Outcome}, plan::Output};
    fn output<'a>(proxies: &'a [PreparedProxy], output: &Output, rows: &dyn RowSet,
        types: &[ValueType], warnings: &mut Warnings<'a>) -> DbResult<()> {
        if let Output::Instances { model, joins, .. } = output {
            inspect(&proxies[*model], None, 0, None, rows, warnings)?;
            for join in joins {
                let pk = join.start + join.pk_pos;
                inspect(&proxies[join.model], None, join.start, Some((pk, types[pk])), rows, warnings)?;
            }
        }
        Ok(())
    }
    fn fetched<'a>(proxies: &'a [PreparedProxy], fetches: &[Fetched], warnings: &mut Warnings<'a>) -> DbResult<()> {
        for fetch in fetches {
            output(proxies, &fetch.plan.output, fetch.rows.as_ref(), &fetch.plan.types, warnings)?;
            fetched(proxies, &fetch.children, warnings)?;
        }
        Ok(())
    }
    let mut warnings = Warnings::default();
    match out {
        Outcome::Select(selected) => {
            output(proxies, &selected.plan.output, selected.rows.as_ref(), &selected.plan.types, &mut warnings)?;
            fetched(proxies, &selected.prefetched, &mut warnings)?;
        }
        Outcome::Rows { model, rows, .. } => inspect(&proxies[*model], None, 0, None, rows.as_ref(), &mut warnings)?,
        Outcome::Prefetched { prefetched, .. } => fetched(proxies, prefetched, &mut warnings)?,
        _ => {}
    }
    Ok(warnings.finish())
}

pub fn emit(warnings: &[Warning]) {
    for warning in warnings {
        // Structured metadata only, one record per query/model/field/category.
        // Both bindings use the same native sink; stderr is capturable by hosts.
        if let Ok(record) = serde_json::to_string(warning) { eprintln!("{record}"); }
    }
}
