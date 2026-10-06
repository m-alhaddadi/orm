use orm_contracts::ir::SchemaIr;
use serde_json::{json, Value};
fn definition() -> Value {
    json!({"models":[
        {"name":"Manager","table":"manager","fields":[{"name":"level","column":"level","type":"int"}]},
        {"name":"Person","table":"person","fields":[{"name":"id","column":"id","type":"int","primary_key":true,"auto_increment":true},{"name":"name","column":"name","type":"string"}]},
        {"name":"Employee","table":"employee","fields":[{"name":"salary","column":"salary","type":"int"}]},
        {"name":"Customer","table":"customer","fields":[{"name":"points","column":"points","type":"int"}]}
    ],"behavior":{"declarations":[
        {"attribute":"composition.model","model":"Manager","field":null,"arguments":{"parent":"Employee","parentRef":"employee","childRef":"manager"},"positional":[],"location":{"file":"schema","line":1,"column":1}},
        {"attribute":"composition.model","model":"Customer","field":null,"arguments":{"parent":"Person","parentRef":"person","childRef":"customer"},"positional":[],"location":{"file":"schema","line":1,"column":1}},
        {"attribute":"composition.model","model":"Employee","field":null,"arguments":{"parent":"Person","parentRef":"person","childRef":"employee"},"positional":[],"location":{"file":"schema","line":1,"column":1}}
    ]}})
}
#[test]
fn chains_and_siblings_keep_physical_storage_separate() {
    let mut ir: SchemaIr = serde_json::from_value(definition()).unwrap();
    orm_model_composition::lower(&mut ir).unwrap();
    assert_eq!(
        ir.models[0]
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "level", "salary", "name"]
    );
    let physical = ir.behavior.storage.as_ref().unwrap();
    assert_eq!(physical.models[0].fields.len(), 2);
    assert_eq!(
        ir.models[0].fields[0]
            .hints
            .get("composition.key-default")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        ir.models[0].fields[1]
            .hints
            .get("composition.local")
            .map(String::as_str),
        Some("true")
    );
    assert!(!ir.models[0].fields[2]
        .hints
        .contains_key("composition.local"));
    assert!(!physical.models[0].fields[0]
        .hints
        .contains_key("composition.key-default"));
    assert_eq!(physical.models[2].fields.len(), 2);
    assert!(physical.models[1].fields[0].auto_increment);
    assert!(!physical.models[0].fields[0].auto_increment);
    assert_eq!(ir.behavior.owner_links.len(), 3);
    assert_eq!(
        ir.behavior
            .field_storage
            .iter()
            .find(|m| m.model == "Manager" && m.field == "name")
            .unwrap()
            .owner,
        "Person"
    );
    assert_eq!(
        ir.behavior
            .field_storage
            .iter()
            .find(|m| m.model == "Manager" && m.field == "salary")
            .unwrap()
            .owner,
        "Employee"
    );
    assert!(ir.models[1].relations.iter().any(|r| r.name == "employee"));
    assert!(ir.models[1].relations.iter().any(|r| r.name == "customer"));
    assert!(physical.models[2].relations[0].foreign_key);
    for d in &mut ir.behavior.declarations {
        d.lowered = true;
    }
    let before = serde_json::to_value(&ir).unwrap();
    orm_model_composition::lower(&mut ir).unwrap();
    assert_eq!(serde_json::to_value(ir).unwrap(), before);
}
#[test]
fn rejected_candidates_are_unchanged() {
    for mutation in 0..8 {
        let mut v = definition();
        match mutation {
            0 => v["behavior"]["declarations"][0]["arguments"]["parent"] = json!("Manager"),
            1 => v["behavior"]["declarations"][0]["arguments"]["parent"] = json!("Missing"),
            2 => v["models"][2]["fields"].as_array_mut().unwrap().push(json!({"name":"name","column":"other","type":"string"})),
            3 => v["models"][2]["fields"].as_array_mut().unwrap().push(json!({"name":"id","column":"id","type":"int","primary_key":true,"auto_increment":true})),
            4 => v["behavior"]["declarations"][2]["arguments"]["parentRef"] = json!("salary"),
            6 => v["models"][2]["fields"].as_array_mut().unwrap().push(json!({"name":"id","column":"id","type":"string","primary_key":true})),
            7 => v["models"][2]["fields"].as_array_mut().unwrap().push(json!({"name":"id","column":"id","type":"int","primary_key":true,"enum":"Role"})),
            _ => v["behavior"]["declarations"][2]["arguments"]["childRef"] = json!("name"),
        }
        let mut ir: SchemaIr = serde_json::from_value(v).unwrap();
        let before = serde_json::to_value(&ir).unwrap();
        assert!(
            orm_model_composition::lower(&mut ir).is_err(),
            "mutation {mutation}"
        );
        assert_eq!(serde_json::to_value(ir).unwrap(), before);
    }
}
