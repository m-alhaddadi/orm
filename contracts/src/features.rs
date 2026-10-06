//! Schema feature support, checked before code generation, migrations or connection.
use crate::dialect::Dialect;
use crate::ir::{ColType, ConstraintIr, EnumStorage, ForEach, SchemaIr, TriggerEvent, TriggerTiming};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureSupport {
    Native,
    Emulated,
    Unsupported,
}

#[derive(Clone, Copy, Debug)]
pub enum Feature {
    NativeEnums,
    Arrays,
    Extensions,
    SqlFunctions,
    RowTriggers,
    StatementTriggers,
    ExclusionConstraints,
    CoveringIndexes,
    IndexMethods,
    DeferrableUnique,
    NullsNotDistinct,
    Comments,
    ExactDecimal,
    Json,
    Uuid,
}

impl Dialect {
    pub const fn feature_support(self, feature: Feature) -> FeatureSupport {
        use Feature::*;
        match (self, feature) {
            (Self::Postgres, _) => FeatureSupport::Native,
            (Self::Sqlite, RowTriggers) => FeatureSupport::Native,
            (Self::Sqlite, Json | Uuid) => FeatureSupport::Emulated,
            (Self::Sqlite, _) => FeatureSupport::Unsupported,
        }
    }
}

pub fn validate(ir: &SchemaIr) -> Result<(), String> {
    let require = |feature, what: &str| {
        if ir.dialect.feature_support(feature) == FeatureSupport::Unsupported {
            Err(format!("{what}: {} does not support {feature:?}", ir.dialect.name()))
        } else { Ok(()) }
    };
    for e in &ir.enums {
        if e.storage == EnumStorage::Native {
            require(Feature::NativeEnums, &format!("enum {}; choose @@storage(text) or @@storage(int)", e.name))?;
        }
        if e.comment.is_some() { require(Feature::Comments, &format!("enum {}", e.name))?; }
    }
    if !ir.extensions.is_empty() || !ir.catalog.is_empty() { require(Feature::Extensions, "schema")?; }
    if !ir.functions.is_empty() { require(Feature::SqlFunctions, "schema")?; }
    for m in &ir.models {
        if m.comment.is_some() { require(Feature::Comments, &m.name)?; }
        for f in &m.fields {
            let what = format!("{}.{}", m.name, f.name);
            if f.array { require(Feature::Arrays, &what)?; }
            if f.ty == ColType::Decimal { require(Feature::ExactDecimal, &what)?; }
            if f.comment.is_some() { require(Feature::Comments, &what)?; }
            if !f.requires.is_empty() { require(Feature::Extensions, &what)?; }
            if ir.dialect == Dialect::Sqlite {
                if f.db_type.is_some() || f.read_sql.is_some() || f.write_sql.is_some() {
                    return Err(format!("{what}: sqlite does not support PostgreSQL native types or SQL conversion templates"));
                }
                if f.auto_increment && !f.primary_key {
                    return Err(format!("{what}: sqlite autoincrement requires the integer primary key"));
                }
            }
        }
        for ix in &m.indexes {
            let what = format!("{} index {:?}", m.name, ix.name);
            if !ix.include.is_empty() { require(Feature::CoveringIndexes, &what)?; }
            if ix.nulls_not_distinct { require(Feature::NullsNotDistinct, &what)?; }
            if !ix.requires.is_empty() { require(Feature::Extensions, &what)?; }
            if ix.method.as_ref().is_some_and(|m| !m.eq_ignore_ascii_case("btree")) || !ix.with.is_empty()
                || ix.columns.iter().any(|k| k.opclass.is_some() || k.nulls.is_some()) {
                require(Feature::IndexMethods, &what)?;
            }
        }
        for c in &m.constraints {
            match c {
                ConstraintIr::Exclude { .. } => require(Feature::ExclusionConstraints, &m.name)?,
                ConstraintIr::Unique { nulls_not_distinct, deferrable, .. } => {
                    if *nulls_not_distinct { require(Feature::NullsNotDistinct, &m.name)?; }
                    if deferrable.is_some() { require(Feature::DeferrableUnique, &m.name)?; }
                }
                _ => {}
            }
        }
        for tr in &m.triggers {
            let what = format!("{} trigger {}", m.name, tr.name);
            require(if tr.for_each == ForEach::Statement { Feature::StatementTriggers } else { Feature::RowTriggers }, &what)?;
            if ir.dialect == Dialect::Sqlite && (tr.function.is_some() || tr.language.is_some() || !tr.args.is_empty()
                || tr.events.len() != 1 || tr.events.contains(&TriggerEvent::Truncate) || tr.timing == TriggerTiming::InsteadOf) {
                return Err(format!("{what}: sqlite table triggers require one insert/update/delete event and an inline SQL body"));
            }
        }
    }
    Ok(())
}
