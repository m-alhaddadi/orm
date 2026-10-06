#![cfg(not(feature = "proxy-models"))]

#[test]
fn proxy_metadata_requires_rebuild_before_models_become_usable() {
    let ir = serde_json::from_value(serde_json::json!({
        "models": [{"name":"User","table":"users","fields":[{"name":"id","column":"id","type":"int","primary_key":true}]}],
        "behavior": {"schema_contract":1,"proxy_models":[{"model":"View","parent":"User","storage_owner":"User"}]}
    })).unwrap();
    let error = orm_core::schema::Schema::from_ir(ir).err().expect("disabled artifact must reject proxy metadata");
    assert!(error.contains("proxy-models") && error.contains("rebuild"), "{error}");
    assert!(!orm_core::behavior::artifact().capabilities.iter().any(|c| c == "proxy-models"));
}
