//! The standalone `orm` binary. The commands themselves are tested through
//! `python -m orm` (tests/test_migrations.py) and `npx orm` (js/test/migrations.test.ts),
//! which run the same code; this checks what is specific to the binary.

use std::path::Path;
use std::process::{Command, Output};

const SCHEMA: &str = "model Book {\n  id    BigInt @id @default(autoincrement())\n  title String\n}\n";

fn orm(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orm")).args(args).current_dir(dir).env_remove("ORM_DATABASE_URL").output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn binary() {
    let dir = std::env::temp_dir().join(format!("orm-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("db")).unwrap();
    std::fs::write(dir.join("db/schema.prisma"), SCHEMA).unwrap();

    let help = orm(&dir, &["--help"]);
    assert!(help.status.success());
    assert!(text(&help.stdout).starts_with("usage: orm "));
    assert_eq!(orm(&dir, &[]).status.code(), Some(2));

    // no schema.prisma in the working directory
    assert_eq!(orm(&dir, &["check"]).status.code(), Some(1));
    // configuration: pyproject.toml
    std::fs::write(dir.join("pyproject.toml"), "[tool.orm]\nschema = \"db/schema.prisma\"\nmigrations = \"db/migrations\"\n").unwrap();
    let check = orm(&dir, &["check"]);
    assert_eq!(text(&check.stdout), "db/schema.prisma: ok (1 models)\n");

    // the binary has no default language: it needs one, or an output file to tell
    assert_eq!(orm(&dir, &["generate"]).status.code(), Some(2));
    assert!(orm(&dir, &["generate", "-o", "out/models.ts"]).status.success());
    assert!(dir.join("out/models.ts").is_file());
    assert!(orm(&dir, &["generate", "python"]).status.success());
    assert!(dir.join("db/models.py").is_file() && dir.join("db/models.pyi").is_file());

    assert!(orm(&dir, &["makemigrations", "--name", "start"]).status.success());
    assert!(dir.join("db/migrations/0001_start/up.sql").is_file());
    let sql = orm(&dir, &["sqlmigrate", "1"]);
    assert!(text(&sql.stdout).contains("CREATE TABLE \"book\""), "{}", text(&sql.stdout));

    let no_db = orm(&dir, &["migrate"]);
    assert_eq!(no_db.status.code(), Some(2));
    assert!(text(&no_db.stderr).contains("ORM_DATABASE_URL"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn schema_module_output_collisions_fail_before_writing() {
    let dir = std::env::temp_dir().join(format!("orm-cli-import-collision-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("schema.prisma"), "import \"library.prisma\"\n").unwrap();
    std::fs::write(dir.join("library.prisma"), SCHEMA).unwrap();
    let result = orm(&dir, &["generate", "python"]);
    assert!(!result.status.success());
    assert!(text(&result.stderr).contains("each schema in its own directory"));
    assert!(!dir.join("models.py").exists());
    assert!(!dir.join("_orm_models.py").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn identity_cli_allocates_explicitly_and_compile_never_mutates() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.test-tmp").join(format!("orm-cli-identities-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("schema.prisma");
    std::fs::write(&path, "model Post {\nid Int @id\n}\nmodel Tag {\nid Int @id\nkind ContentType\nobject_id Int\n}").unwrap();
    assert!(!orm(&dir, &["compile"]).status.success());
    let generated = orm(&dir, &["identities"]);
    assert!(generated.status.success(), "{}", text(&generated.stderr));
    assert!(text(&generated.stdout).contains("added Post = 1\nadded Tag = 2\n"), "{}", text(&generated.stdout));
    let before = std::fs::read(dir.join("schema.identities.json")).unwrap();
    for args in [&["compile"][..], &["check"][..], &["generate", "python"][..], &["generate", "typescript"][..]] {
        let out = orm(&dir, args); assert!(out.status.success(), "{}", text(&out.stderr));
    }
    assert_eq!(std::fs::read(dir.join("schema.identities.json")).unwrap(), before);
    let py = std::fs::read_to_string(dir.join("models.pyi")).unwrap();
    let ts = std::fs::read_to_string(dir.join("models.ts")).unwrap();
    assert!(py.contains("class ContentType(IntEnum)"));
    assert!(ts.contains("Post: 1"), "{ts}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn pull_baseline_and_drift_on_sqlite() {
    let dir = std::env::temp_dir().join(format!("orm-cli-pull-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let url = format!("sqlite://{}", dir.join("live.db").display());
    let source = format!("datasource db {{\n  provider = \"sqlite\"\n}}\n\n{SCHEMA}");
    std::fs::write(dir.join("made.prisma"), &source).unwrap();
    let made = ["--url", url.as_str(), "--schema", "made.prisma", "--dir", "made"];
    assert!(orm(&dir, &[&made[..], &["makemigrations"]].concat()).status.success());
    assert!(orm(&dir, &[&made[..], &["migrate"]].concat()).status.success());

    // pull writes the configured schema file, and refuses to overwrite one
    let pulled = orm(&dir, &["--url", &url, "pull"]);
    assert!(pulled.status.success(), "{}", text(&pulled.stderr));
    assert!(text(&pulled.stdout).contains("The schema reproduces the database."), "{}", text(&pulled.stdout));
    assert!(std::fs::read_to_string(dir.join("schema.prisma")).unwrap().contains("model Book {"));
    assert_eq!(orm(&dir, &["--url", &url, "pull"]).status.code(), Some(2));

    // baseline refuses a database that has applied migrations, before writing one
    let refused = orm(&dir, &["--url", &url, "baseline"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(text(&refused.stderr).contains("already has applied migrations"), "{}", text(&refused.stderr));
    assert!(!dir.join("migrations").exists());

    let drift = orm(&dir, &[&made[..], &["drift"]].concat());
    assert_eq!(drift.status.code(), Some(0), "{}", text(&drift.stdout));
    assert!(text(&drift.stdout).contains("No drift from 0001_initial."));
    // a newer snapshot than the database: drift exits 1 and names the change
    std::fs::write(dir.join("made.prisma"), source.replace("title String", "title String\n  pages Int @default(0)")).unwrap();
    assert!(orm(&dir, &[&made[..], &["makemigrations"]].concat()).status.success());
    let drift = orm(&dir, &[&made[..], &["drift"]].concat());
    assert_eq!(drift.status.code(), Some(1));
    assert!(text(&drift.stdout).contains("add column book.pages"), "{}", text(&drift.stdout));
    let _ = std::fs::remove_dir_all(&dir);
}
