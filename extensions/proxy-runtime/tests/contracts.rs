use orm_contracts::{extension::{ProxyField, ProxyModel}, ir::{ColType, EnumIr, EnumStorage, EnumValueIr, FieldIr}};
use orm_proxy_runtime::{prepare, Category, EnumScalar, Warnings};
use serde_json::json;

#[test]
fn physical_enum_values_remain_representable_and_warnings_aggregate_without_values() {
    let mut field = FieldIr::plain("status", ColType::String);
    field.enum_idx = Some(0); field.enum_name = Some("Status".into());
    let enumeration = EnumIr { name:"Status".into(),db_name:"status".into(), storage:EnumStorage::Text,
        values:vec![EnumValueIr {name:"ACTIVE".into(),value:json!("active")},EnumValueIr {name:"OLD".into(),value:json!("old-private-value")}],comment:None };
    let spec = ProxyModel { model:"Active".into(),parent:"User".into(),storage_owner:"User".into(),
        fields:vec![ProxyField {field:"status".into(),non_null:true,subset:Some(vec!["ACTIVE".into()])}], ..Default::default() };
    let prepared = prepare("Active", &[field], &[enumeration], Some(&spec)).unwrap();
    let contract = &prepared.fields[0];
    assert!(contract.accepts_enum(EnumScalar::Text("active")));
    assert!(!contract.accepts_enum(EnumScalar::Text("old-private-value")));
    let mut warnings = Warnings::default();
    for _ in 0..100 { warnings.record(&prepared, contract, Category::EnumSubset); }
    warnings.record(&prepared, contract, Category::Null);
    let warnings = warnings.finish();
    assert_eq!(warnings.len(), 2);
    assert_eq!(warnings.iter().find(|w| w.category == Category::EnumSubset).unwrap().occurrence_count, 100);
    let json = serde_json::to_string(&warnings).unwrap();
    assert!(!json.contains("old-private-value"));
    assert!(json.contains("expected_shape"));
    assert!(json.contains("Active"));
}
