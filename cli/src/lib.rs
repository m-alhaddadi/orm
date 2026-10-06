//! The `orm` command line. There is one implementation, this crate, and it runs as:
//!
//! * the `orm` binary (`src/main.rs`),
//! * `python -m orm`, through the Python extension (`bindings/python`),
//! * `npx orm` / `bunx orm`, through the Node addon (`bindings/node`).
//!
//! ```text
//! orm [--schema FILE] [--dir DIR] [--url URL] <command>
//!   check                                         compile the schema and report errors
//!   compile [-o ir.json]                          the compiled schema (JSON IR)
//!   generate [python|typescript] [-o FILE] [--import MODULE]
//!   makemigrations [name] [--empty] [--check]
//!   sqlmigrate <migration> [--down]
//!   migrate [target]
//!   rollback [--steps N | --to <migration>|zero]
//!   showmigrations
//! ```
//!
//! The schema file and the migrations directory come from `--schema` / `--dir`, else
//! from `[tool.orm]` in `pyproject.toml` or the `"orm"` key of `package.json`
//! (`schema`, `migrations`), else `schema.prisma` and `migrations`. The database URL
//! comes from `--url` or `ORM_DATABASE_URL`.
//!
//! Output goes to the process's stdout and stderr. Exit codes: 0 success, 1 failure
//! (an invalid schema, a migration error, a database error, `makemigrations --check`
//! finding changes), 2 bad usage.

use std::path::{Path, PathBuf};

use orm_core::{codegen, dsl, migrate as files};
use orm_engine::migrate::{self, Down};

/// Which program is running the CLI: it decides the name in the help, which
/// configuration file is read first and what `generate` writes by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// The standalone `orm` binary.
    Binary,
    /// `python -m orm`.
    Python,
    /// `npx orm`.
    Node,
}

impl Host {
    fn prog(self) -> &'static str {
        match self {
            Host::Binary => "orm",
            Host::Python => "python -m orm",
            Host::Node => "npx orm",
        }
    }

    pub fn parse(name: &str) -> Option<Host> {
        match name {
            "binary" => Some(Host::Binary),
            "python" => Some(Host::Python),
            "node" => Some(Host::Node),
            _ => None,
        }
    }
}

const COMMANDS: &str = "  check                                    compile the schema and report errors
  compile [-o ir.json]                     print or write the compiled schema (JSON IR)
  generate [python|typescript] [-o FILE] [--import MODULE]
                                           the models module from the schema
  makemigrations [name] [--empty] [--check]
                                           write the next migration from schema changes
  sqlmigrate <migration> [--down]          print a migration's SQL
  migrate [target]                         apply pending migrations
  rollback [--steps N | --to <migration>|zero]
                                           revert applied migrations (default: the last one)
  showmigrations                           list migrations and whether they are applied

The schema and migrations directory come from --schema / --dir, else from [tool.orm]
in pyproject.toml or \"orm\" in package.json, else schema.prisma and migrations/.
The database URL comes from --url or ORM_DATABASE_URL.";

fn usage(host: Host) -> String {
    format!("usage: {} [--schema FILE] [--dir DIR] [--url URL] <command>\n{COMMANDS}", host.prog())
}

enum Failure {
    /// Bad arguments: exit 2 with the usage.
    Usage(String),
    /// Exit 2 without the usage.
    Setup(String),
    /// Exit 1.
    Failed(String),
}

impl From<orm_engine::Error> for Failure {
    fn from(e: orm_engine::Error) -> Self {
        Failure::Failed(e.to_string())
    }
}

type Result<T> = std::result::Result<T, Failure>;

fn failed(e: impl std::fmt::Display) -> Failure {
    Failure::Failed(e.to_string())
}

struct Args {
    positional: Vec<String>,
    options: Vec<(String, Option<String>)>,
}

const FLAGS: [&str; 5] = ["--empty", "--check", "--down", "-h", "--help"];
const VALUED: [&str; 9] = ["--schema", "--dir", "--url", "-o", "--out", "--import", "--name", "--steps", "--to"];

impl Args {
    fn parse(raw: &[String]) -> Result<Self> {
        let (mut positional, mut options) = (vec![], vec![]);
        let mut it = raw.iter();
        while let Some(a) = it.next() {
            let (key, inline) = match a.split_once('=') {
                Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
                _ => (a.as_str(), None),
            };
            if FLAGS.contains(&key) && inline.is_none() {
                options.push((key.to_string(), None));
            } else if VALUED.contains(&key) {
                let v = match inline {
                    Some(v) => v,
                    None => it.next().cloned().ok_or_else(|| Failure::Usage(format!("{key} needs a value")))?,
                };
                options.push((key.to_string(), Some(v)));
            } else if a.starts_with('-') && a != "-" && a.parse::<f64>().is_err() {
                return Err(Failure::Usage(format!("unknown option {a}")));
            } else {
                positional.push(a.clone());
            }
        }
        Ok(Args { positional, options })
    }

    fn opt(&self, names: &[&str]) -> Option<&str> {
        self.options.iter().rev().find(|(k, _)| names.contains(&k.as_str())).and_then(|(_, v)| v.as_deref())
    }

    fn flag(&self, name: &str) -> bool {
        self.options.iter().any(|(k, _)| k == name)
    }

    /// Only these options (besides the global ones) and at most `max` positionals
    /// after the command.
    fn allow(&self, command: &str, options: &[&str], max: usize) -> Result<()> {
        let global = ["--schema", "--dir", "--url"];
        if let Some((k, _)) = self.options.iter().find(|(k, _)| !global.contains(&k.as_str()) && !options.contains(&k.as_str())) {
            return Err(Failure::Usage(format!("{command} doesn't take {k}")));
        }
        if self.positional.len() > max + 1 {
            return Err(Failure::Usage(format!("too many arguments for {command}")));
        }
        Ok(())
    }

    fn arg(&self, i: usize) -> Option<&str> {
        self.positional.get(i + 1).map(String::as_str)
    }
}

/// `schema` and `migrations` from the project's configuration file.
#[derive(Default)]
struct Config {
    schema: Option<String>,
    migrations: Option<String>,
}

fn pyproject() -> Option<Config> {
    let text = std::fs::read_to_string("pyproject.toml").ok()?;
    let doc: toml::Table = text.parse().ok()?;
    let orm = doc.get("tool")?.get("orm")?;
    let get = |k: &str| orm.get(k).and_then(|v| v.as_str()).map(String::from);
    Some(Config { schema: get("schema"), migrations: get("migrations") })
}

fn package_json() -> Option<Config> {
    let text = std::fs::read_to_string("package.json").ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    let orm = doc.get("orm")?;
    let get = |k: &str| orm.get(k).and_then(|v| v.as_str()).map(String::from);
    Some(Config { schema: get("schema"), migrations: get("migrations") })
}

fn config(host: Host) -> Config {
    match host {
        Host::Node => package_json().or_else(pyproject),
        _ => pyproject().or_else(package_json),
    }
    .unwrap_or_default()
}

fn load(path: &Path) -> Result<(orm_core::ir::SchemaIr, orm_core::schema::Schema)> {
    dsl::compile_file(path).and_then(dsl::check).map_err(Failure::Failed)
}

fn write(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| failed(format!("{}: {e}", parent.display())))?;
    }
    std::fs::write(path, text).map_err(|e| failed(format!("{}: {e}", path.display())))
}

/// Runs the command line `argv` (without the program name) and gives the exit code.
pub async fn run(argv: &[String], host: Host) -> i32 {
    match command(argv, host).await {
        Ok(code) => code,
        Err(Failure::Usage(m)) => {
            eprintln!("error: {m}\n(see `{} --help`)", host.prog());
            2
        }
        Err(Failure::Setup(m)) => {
            eprintln!("error: {m}");
            2
        }
        Err(Failure::Failed(m)) => {
            eprintln!("error: {m}");
            1
        }
    }
}

/// [`run`] on a runtime of its own, for callers that aren't async.
pub fn run_blocking(argv: &[String], host: Host) -> i32 {
    match tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build() {
        Ok(rt) => rt.block_on(run(argv, host)),
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

async fn command(argv: &[String], host: Host) -> Result<i32> {
    let args = Args::parse(argv)?;
    let Some(command) = args.positional.first().map(String::as_str) else {
        if args.flag("-h") || args.flag("--help") {
            println!("{}", usage(host));
            return Ok(0);
        }
        return Err(Failure::Usage("no command".into()));
    };
    if args.flag("-h") || args.flag("--help") || command == "help" {
        println!("{}", usage(host));
        return Ok(0);
    }
    let cfg = config(host);
    let schema = PathBuf::from(args.opt(&["--schema"]).or(cfg.schema.as_deref()).unwrap_or("schema.prisma"));
    let dir = PathBuf::from(args.opt(&["--dir"]).or(cfg.migrations.as_deref()).unwrap_or("migrations"));
    match command {
        "check" => {
            args.allow(command, &[], 0)?;
            let (ir, _) = load(&schema)?;
            println!("{}: ok ({} models)", schema.display(), ir.models.len());
        }
        "compile" => {
            args.allow(command, &["-o", "--out"], 0)?;
            let (ir, _) = load(&schema)?;
            let json = serde_json::to_string_pretty(&ir).map_err(failed)? + "\n";
            match args.opt(&["-o", "--out"]) {
                Some(out) => write(Path::new(out), &json)?,
                None => print!("{json}"),
            }
        }
        "generate" => {
            args.allow(command, &["-o", "--out", "--import"], 1)?;
            generate(&args, host, &schema)?;
        }
        "makemigrations" => {
            args.allow(command, &["--name", "--empty", "--check"], 1)?;
            let (_, compiled) = load(&schema)?;
            if args.flag("--check") {
                let plan = files::files::next(&dir, &compiled).map_err(Failure::Failed)?;
                for s in &plan.up {
                    println!("  {}", s.summary);
                }
                if !plan.up.is_empty() {
                    eprintln!("the schema has changes without a migration");
                    return Ok(1);
                }
                return Ok(0);
            }
            let name = args.arg(0).or(args.opt(&["--name"]));
            match files::files::make(&dir, &compiled, name, args.flag("--empty")).map_err(Failure::Failed)? {
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
            args.allow(command, &["--down"], 1)?;
            let name = args.arg(0).ok_or_else(|| Failure::Usage("sqlmigrate needs a migration".into()))?;
            let m = migrate::find(&dir, name)?;
            print!("{}", if args.flag("--down") { m.down_sql()? } else { m.up_sql()? });
        }
        "migrate" | "rollback" | "showmigrations" => {
            match command {
                "migrate" => args.allow(command, &[], 1)?,
                "rollback" => args.allow(command, &["--steps", "--to"], 0)?,
                _ => args.allow(command, &[], 0)?,
            }
            return database(command, &args, &dir).await;
        }
        other => return Err(Failure::Usage(format!("unknown command {other}"))),
    }
    Ok(0)
}

fn generate(args: &Args, host: Host, schema: &Path) -> Result<()> {
    let out = args.opt(&["-o", "--out"]).map(PathBuf::from);
    let ext = out.as_ref().and_then(|o| o.extension()).and_then(|e| e.to_str());
    let language = match (args.arg(0), ext, host) {
        (Some(l), _, _) => l,
        (None, Some("py" | "pyi"), _) => "python",
        (None, Some("ts" | "mts"), _) => "typescript",
        (None, _, Host::Python) => "python",
        (None, _, Host::Node) => "typescript",
        (None, _, Host::Binary) => return Err(Failure::Usage("generate needs a language: python or typescript".into())),
    };
    let project = dsl::compile_project_file(schema).map_err(Failure::Failed)?;
    let (ir, compiled) = dsl::check(project.ir).map_err(Failure::Failed)?;
    let source = schema.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match language {
        #[cfg(feature = "generate-python")]
        "python" => {
            if args.opt(&["--import"]).is_some() {
                return Err(Failure::Usage("--import is for typescript".into()));
            }
            let out = out.unwrap_or_else(|| schema.with_file_name("models.py"));
            let g = codegen::python::generate(&ir, &compiled, &source).map_err(Failure::Failed)?;
            if project.units.len() == 1 {
                write(&out, &g.module)?;
                write(&out.with_extension("pyi"), &g.stub)?;
                println!("wrote {} and {}", out.display(), out.with_extension("pyi").display());
            } else {
                let shared = out.with_file_name("_orm_models.py");
                let shared_abs = absolute(&shared)?;
                let mut files = vec![(shared.clone(), g.module), (shared.with_extension("pyi"), g.stub)];
                let mut outputs = std::collections::HashSet::new();
                outputs.insert(shared_abs.clone());
                for (index, unit) in project.units.iter().enumerate() {
                    let target = if index == 0 { out.clone() } else { unit.path.with_file_name("models.py") };
                    let target_abs = absolute(&target)?;
                    if !outputs.insert(target_abs.clone()) {
                        return Err(Failure::Failed(format!("multiple schema modules would write {}; put each schema in its own directory", target.display())));
                    }
                    let (relative, import) = module_paths(target_abs.parent().unwrap(), &shared_abs)?;
                    let facade = codegen::python::facade(unit, &relative, &import, &compiled);
                    files.push((target.clone(), facade.module));
                    files.push((target.with_extension("pyi"), facade.stub));
                }
                for (path, content) in files {
                    write(&path, &content)?;
                    println!("wrote {}", path.display());
                }
            }
        }
        #[cfg(feature = "generate-typescript")]
        "typescript" => {
            let out = out.unwrap_or_else(|| schema.with_file_name("models.ts"));
            let runtime = args.opt(&["--import"]).unwrap_or("orm");
            write(&out, &codegen::typescript::generate(&ir, &compiled, &source, runtime).map_err(Failure::Failed)?)?;
            println!("wrote {}", out.display());
        }
        #[cfg(not(feature = "generate-python"))]
        "python" => return Err(Failure::Usage("python generator is not compiled into this profile".into())),
        #[cfg(not(feature = "generate-typescript"))]
        "typescript" => return Err(Failure::Usage("typescript generator is not compiled into this profile".into())),
        other => return Err(Failure::Usage(format!("unknown language {other}; known: python, typescript"))),
    }
    Ok(())
}

// Normalize output paths without requiring generated files to exist yet.
fn absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() { path.to_path_buf() } else {
        std::env::current_dir().map_err(failed)?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => { normalized.pop(); }
            std::path::Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

fn module_paths(from: &Path, shared: &Path) -> Result<(String, String)> {
    let a: Vec<_> = from.components().collect();
    let b: Vec<_> = shared.parent().unwrap().components().collect();
    let common = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let mut relative = PathBuf::new();
    for _ in common..a.len() { relative.push(".."); }
    let mut modules = vec![];
    for component in &b[common..] {
        relative.push(component.as_os_str());
        let name = component.as_os_str().to_string_lossy().into_owned();
        modules.push(name);
    }
    relative.push(shared.file_name().unwrap());
    modules.push("_orm_models".into());
    let mut package = Vec::new();
    let mut package_root = shared.parent().unwrap();
    while package_root.join("__init__.py").is_file() {
        package.push(package_root.file_name().unwrap().to_string_lossy().into_owned());
        package_root = package_root.parent().unwrap();
    }
    let import = if !package.is_empty() && !from.starts_with(package_root.join(package.last().unwrap())) {
        package.reverse();
        package.push("_orm_models".into());
        package.join(".")
    } else {
        format!("{}{}", ".".repeat(a.len() - common + 1), modules.join("."))
    };
    for name in import.split('.').filter(|name| !name.is_empty()) {
        if name.chars().enumerate().any(|(i, c)| !(c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))) {
            return Err(Failure::Failed(format!("generated Python package directory must be an identifier: {name}")));
        }
    }
    Ok((relative.to_string_lossy().into_owned(), import))
}

async fn database(command: &str, args: &Args, dir: &Path) -> Result<i32> {
    let env = std::env::var("ORM_DATABASE_URL").ok().filter(|u| !u.is_empty());
    let Some(url) = args.opt(&["--url"]).map(String::from).or(env) else {
        return Err(Failure::Setup("no database; pass --url or set ORM_DATABASE_URL".into()));
    };
    let driver = orm_engine::db::connect(&url, 1).await.map_err(failed)?;
    let result = async {
        match command {
            "migrate" => {
                let done = migrate::upgrade(&*driver, dir, args.arg(0)).await?;
                for m in &done {
                    println!("Applied {}", m.name);
                }
                if done.is_empty() {
                    println!("Nothing to apply.");
                }
            }
            "rollback" => {
                let down = match (args.opt(&["--to"]), args.opt(&["--steps"])) {
                    (Some(_), Some(_)) => return Err(Failure::Usage("pass --steps or --to, not both".into())),
                    (Some(to), None) => Down::To(to.into()),
                    (None, steps) => Down::Steps(
                        steps.map_or(Ok(1), str::parse).map_err(|_| Failure::Usage("--steps takes a number".into()))?,
                    ),
                };
                let done = migrate::downgrade(&*driver, dir, down).await?;
                for m in &done {
                    println!("Reverted {}", m.name);
                }
                if done.is_empty() {
                    println!("Nothing to revert.");
                }
            }
            _ => {
                for s in migrate::status(&*driver, dir).await? {
                    let at = s.applied_at.map(|a| format!("  ({a})")).unwrap_or_default();
                    println!("[{}] {}{at}", if s.applied { "x" } else { " " }, s.migration.name);
                }
            }
        }
        Ok(0)
    }
    .await;
    driver.close().await;
    result
}
