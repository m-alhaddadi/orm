//! Revisit an older composition through one Cargo cache; stale inputs still fail.
use orm_extension_build::inputs::Inputs;
use std::{fs,path::PathBuf,process::Command};

#[test]
fn cached_build_script_reads_the_active_composition() {
    let root=std::env::temp_dir().join(format!("orm-guard-cache-{}",std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let host=PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let target=host.join("target/extension-guard-proof");
    for (name,value) in [("a",1),("b",2)] {
        let dir=root.join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("Cargo.toml"),"[package]\nname=\"orm-guard-cache-proof\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[workspace]\n[build-dependencies]\nsha2=\"0.10\"\nserde_json=\"1\"\n").unwrap();
        fs::write(dir.join("src/main.rs"),"include!(env!(\"ORM_CORE_COMPOSITION\")); fn main() { println!(\"{VALUE}\"); }").unwrap();
        fs::write(dir.join("composition.rs"),format!("const VALUE:u8={value};")).unwrap();
        fs::write(dir.join("input.txt"),name).unwrap();
        let mut inputs=Inputs::default();
        inputs.file(&dir.join("input.txt")).unwrap();
        fs::write(dir.join("composition-inputs.json"),inputs.guard_data(&dir.join("composition.rs")).unwrap()).unwrap();
        fs::write(dir.join("build.rs"),Inputs::guard_source()).unwrap();
    }
    let run=|name:&str| Command::new("cargo").args(["run","--quiet","--offline","--manifest-path"]).arg(root.join(name).join("Cargo.toml"))
        .env("CARGO_TARGET_DIR",&target).env("ORM_CORE_COMPOSITION",root.join(name).join("composition.rs")).output().unwrap();
    for (name,value) in [("a",1),("b",2),("a",1)] {
        let output=run(name);
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(),value.to_string());
    }
    fs::write(root.join("a/input.txt"),"changed").unwrap();
    let output=run("a");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("extension build inputs changed"));
    fs::remove_dir_all(root).unwrap();
}
