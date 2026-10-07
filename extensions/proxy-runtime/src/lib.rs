//! Optional concrete result contracts. Preparation resolves field positions once;
//! operations check decoded physical values without any declaration lookup.
use std::collections::BTreeMap;
use orm_contracts::{extension::ProxyModel, ir::{EnumIr, FieldIr, ValueType}};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct PreparedProxy {
    pub model: String,
    pub fields: Vec<PreparedField>,
}
#[derive(Clone, Debug)]
pub struct PreparedField {
    pub position: usize,
    pub name: String,
    pub ty: ValueType,
    pub expected: ExpectedShape,
    pub allowed: Option<Vec<Value>>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ExpectedShape {
    pub non_null: bool,
    pub enum_members: Option<Vec<String>>,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Category { Null, EnumSubset }
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Warning {
    pub code: &'static str,
    pub model: String,
    pub field: String,
    pub expected_shape: ExpectedShape,
    pub category: Category,
    pub occurrence_count: usize,
}

pub fn prepare(model: &str, fields: &[FieldIr], enums: &[EnumIr], proxy: Option<&ProxyModel>) -> Result<PreparedProxy, String> {
    let mut prepared = PreparedProxy { model: model.into(), ..Default::default() };
    let Some(proxy) = proxy else { return Ok(prepared) };
    let mut seen = std::collections::BTreeSet::new();
    for contract in &proxy.fields {
        if !seen.insert(&contract.field) { return Err(format!("{model}: duplicate proxy shape field {}", contract.field)); }
        let position = fields.iter().position(|f| f.name == contract.field).ok_or_else(|| format!("{model}: unknown proxy shape field {}", contract.field))?;
        let field = &fields[position];
        let allowed = if let Some(members) = &contract.subset {
            let e = field.enum_idx.and_then(|i| enums.get(i as usize)).ok_or_else(|| format!("{model}.{}: subset requires a physical parent enum", field.name))?;
            if members.is_empty() || members.iter().collect::<std::collections::BTreeSet<_>>().len() != members.len() { return Err("proxy enum subset must be nonempty and unique".into()); }
            Some(members.iter().map(|m| e.values.iter().find(|v| v.name == *m).map(|v| v.value.clone()).ok_or_else(|| format!("{model}.{}: unknown parent enum member {m}", field.name))).collect::<Result<_, _>>()?)
        } else { None };
        prepared.fields.push(PreparedField { position, name: field.name.clone(), ty: field.value_type(),
            expected: ExpectedShape { non_null: contract.non_null, enum_members: contract.subset.clone() }, allowed });
    }
    Ok(prepared)
}

/// Borrow an ordinary decoded enum scalar, retaining its parent's representation.
pub enum EnumScalar<'a> { Text(&'a str), Int(i64) }
impl PreparedField {
    pub fn accepts_enum(&self, value: EnumScalar<'_>) -> bool {
        self.allowed.as_ref().is_none_or(|allowed| allowed.iter().any(|v| match value {
            EnumScalar::Text(s) => v.as_str() == Some(s),
            EnumScalar::Int(n) => v.as_i64() == Some(n),
        }))
    }
}

/// One accumulator per operation, shared by root/joins/prefetch/returning shapes.
/// Only metadata enters a warning. Decoded field values are never retained.
#[derive(Default)]
pub struct Warnings<'a> { entries: BTreeMap<(&'a str, &'a str, Category), Warning> }
impl<'a> Warnings<'a> {
    pub fn record(&mut self, proxy: &'a PreparedProxy, field: &'a PreparedField, category: Category) {
        let key = (proxy.model.as_str(), field.name.as_str(), category);
        self.entries.entry(key).and_modify(|w| w.occurrence_count += 1).or_insert_with(|| Warning {
            code: "orm.proxy.shape", model: proxy.model.clone(), field: field.name.clone(),
            expected_shape: field.expected.clone(), category, occurrence_count: 1,
        });
    }
    pub fn finish(self) -> Vec<Warning> { self.entries.into_values().collect() }
}
