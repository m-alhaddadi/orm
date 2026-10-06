//! Emit a selected file-field module for isolated binding/type verification.
#[cfg(feature = "file-storage")]
fn main() {
    use orm_core::{behavior::{FieldAdapter, FileField, FILE_REFERENCE_ADAPTER, SCHEMA_CONTRACT}, schema::Schema};
    let destination = std::env::args().nth(1).expect("output directory");
    let mut ir = orm_core::dsl::compile("model Report {\n id Int @id\n file Json?\n}", None).unwrap();
    ir.behavior.schema_contract = SCHEMA_CONTRACT;
    ir.behavior.field_adapters.push(FieldAdapter { model: "Report".into(), field: "file".into(), adapter: FILE_REFERENCE_ADAPTER.into() });
    ir.behavior.file_fields.push(FileField { model: "Report".into(), field: "file".into(), storage: "reports".into(), reference_contract: 1 });
    let schema = Schema::from_ir(serde_json::from_value(serde_json::to_value(&ir).unwrap()).unwrap()).unwrap();
    let python = orm_core::codegen::python::generate(&ir, &schema, "report.prisma").unwrap();
    let typescript = orm_core::codegen::typescript::generate(&ir, &schema, "report.prisma", "../../js/src/index.js").unwrap();
    std::fs::create_dir_all(&destination).unwrap();
    for (name, source) in [("models.py", python.module), ("models.pyi", python.stub), ("models.ts", typescript)] {
        std::fs::write(std::path::Path::new(&destination).join(name), source).unwrap();
    }
}
#[cfg(not(feature = "file-storage"))]
fn main() { panic!("select --features file-storage"); }
