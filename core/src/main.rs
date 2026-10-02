//! `orm`: the schema tool, independent of any language binding.
//!
//! ```text
//! orm check <schema.prisma>
//! orm compile <schema.prisma> [-o ir.json]
//! orm generate python <schema.prisma> [-o models.py]          (also writes models.pyi)
//! orm makemigrations <schema.prisma> [--dir migrations] [--name N] [--empty] [--check]
//! orm sqlmigrate <migration> [--dir migrations] [--down]
//! ```
//!
//! Applying migrations needs a database connection, so it lives in the bindings
//! (`python -m orm migrate`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use orm_core::{codegen, dsl, migrate};

const USAGE: &str = "usage:
  orm check <schema.prisma>
  orm compile <schema.prisma> [-o ir.json]
  orm generate python <schema.prisma> [-o models.py]
  orm makemigrations <schema.prisma> [--dir migrations] [--name N] [--empty] [--check]
  orm sqlmigrate <migration> [--dir migrations] [--down]";

struct Args {
    positional: Vec<String>,
    options: Vec<(String, Option<String>)>,
}

impl Args {
    fn parse(raw: Vec<String>) -> Result<Self, String> {
        let flags = ["--empty", "--check", "--down"];
        let (mut positional, mut options) = (vec![], vec![]);
        let mut it = raw.into_iter();
        while let Some(a) = it.next() {
            if flags.contains(&a.as_str()) {
                options.push((a, None));
            } else if a.starts_with('-') {
                let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
                options.push((a, Some(v)));
            } else {
                positional.push(a);
            }
        }
        Ok(Args { positional, options })
    }

    fn opt(&self, names: &[&str]) -> Option<&str> {
        self.options.iter().find(|(k, _)| names.contains(&k.as_str())).and_then(|(_, v)| v.as_deref())
    }

    fn flag(&self, name: &str) -> bool {
        self.options.iter().any(|(k, _)| k == name)
    }

    fn check(&self, allowed: &[&str], positional: usize) -> Result<(), String> {
        if let Some((k, _)) = self.options.iter().find(|(k, _)| !allowed.contains(&k.as_str())) {
            return Err(format!("unknown option {k}"));
        }
        if self.positional.len() != positional {
            return Err(USAGE.into());
        }
        Ok(())
    }
}

fn load(path: &str) -> Result<(orm_core::ir::SchemaIr, orm_core::schema::Schema), String> {
    dsl::check(dsl::compile_file(Path::new(path))?)
}

fn run(raw: Vec<String>) -> Result<ExitCode, String> {
    let Some((command, rest)) = raw.split_first() else { return Err(USAGE.into()) };
    let args = Args::parse(rest.to_vec())?;
    match command.as_str() {
        "check" => {
            args.check(&[], 1)?;
            let (ir, _) = load(&args.positional[0])?;
            println!("ok: {} model(s)", ir.models.len());
        }
        "compile" => {
            args.check(&["-o", "--out"], 1)?;
            let (ir, _) = load(&args.positional[0])?;
            let json = serde_json::to_string_pretty(&ir).map_err(|e| e.to_string())? + "\n";
            match args.opt(&["-o", "--out"]) {
                Some(out) => std::fs::write(out, json).map_err(|e| format!("{out}: {e}"))?,
                None => print!("{json}"),
            }
        }
        "generate" => {
            args.check(&["-o", "--out"], 2)?;
            if args.positional[0] != "python" {
                return Err(format!("unknown target {}; available: python", args.positional[0]));
            }
            let schema_path = &args.positional[1];
            let (ir, schema) = load(schema_path)?;
            let out = match args.opt(&["-o", "--out"]) {
                Some(o) => PathBuf::from(o),
                None => Path::new(schema_path).with_file_name("models.py"),
            };
            let source = Path::new(schema_path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let g = codegen::python::generate(&ir, &schema, &source)?;
            if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::write(&out, g.module).map_err(|e| format!("{}: {e}", out.display()))?;
            let stub = out.with_extension("pyi");
            std::fs::write(&stub, g.stub).map_err(|e| format!("{}: {e}", stub.display()))?;
            println!("wrote {} and {}", out.display(), stub.display());
        }
        "makemigrations" => {
            args.check(&["--dir", "--name", "--empty", "--check"], 1)?;
            let (_, schema) = load(&args.positional[0])?;
            let dir = PathBuf::from(args.opt(&["--dir"]).unwrap_or("migrations"));
            if args.flag("--check") {
                let plan = migrate::files::next(&dir, &schema)?;
                for s in &plan.up {
                    println!("  {}", s.summary);
                }
                if !plan.up.is_empty() {
                    eprintln!("the schema has changes without a migration");
                    return Ok(ExitCode::from(1));
                }
                return Ok(ExitCode::SUCCESS);
            }
            match migrate::files::make(&dir, &schema, args.opt(&["--name"]), args.flag("--empty"))? {
                None => println!("No changes."),
                Some((folder, plan)) => {
                    println!("Created {}", folder.path.display());
                    for s in &plan.up {
                        println!("  - {}", s.summary);
                        if let Some(w) = &s.warning {
                            println!("    ! {w}");
                        }
                    }
                }
            }
        }
        "sqlmigrate" => {
            args.check(&["--dir", "--down"], 1)?;
            let dir = PathBuf::from(args.opt(&["--dir"]).unwrap_or("migrations"));
            let m = migrate::files::find(&dir, &args.positional[0])?;
            let file = m.path.join(if args.flag("--down") { "down.sql" } else { "up.sql" });
            print!("{}", std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?);
        }
        "-h" | "--help" | "help" => println!("{USAGE}"),
        other => return Err(format!("unknown command {other}\n{USAGE}")),
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
