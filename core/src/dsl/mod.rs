//! The schema language (`.orm` files).
//!
//! ```text
//! import "extensions/acme.toml"          // extra extension definitions (see ext.rs)
//! extension postgis(version: "3.4")      // pin an extension; used ones are added anyway
//!
//! model User @table("users") {
//!     id:         BigInt      @primary @auto
//!     email:      citext      @unique             // extension type
//!     created_at: DateTime    @default(now)
//!     posts:      Post[]      @relation(via: Post.author_id)
//!
//!     @@index([created_at(sort: desc)], where: "email IS NOT NULL")
//! }
//! ```
//!
//! [`compile`] turns a file into the [`SchemaIr`] every binding consumes. The full
//! reference is `docs/schema.md`.

mod lower;
mod syntax;

use std::path::{Path, PathBuf};

use crate::ir::SchemaIr;
use crate::schema::Schema;

pub use syntax::Pos;

/// Compiles schema source. `origin` is the file it came from: error messages name it
/// and `import` paths resolve against its directory.
pub fn compile(source: &str, origin: Option<&Path>) -> Result<SchemaIr, String> {
    let label = origin.map(|p| p.display().to_string()).unwrap_or_else(|| "<schema>".into());
    let base: PathBuf = origin.and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    let load = |path: &str| -> Result<String, String> {
        let p = base.join(path);
        std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))
    };
    let located = |e: syntax::Error| format!("{label}:{}: {}", e.pos, e.msg);
    let items = syntax::parse(source).map_err(located)?;
    let ir = lower::Lowering { load: &load }.lower(items).map_err(located)?;
    Ok(ir)
}

/// Compiles and validates a schema file.
pub fn compile_file(path: &Path) -> Result<SchemaIr, String> {
    let source = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    compile(&source, Some(path))
}

/// Compiles source and checks it the way the engine will (relations, keys, schema
/// objects), so a bad schema fails at compile time rather than at connect.
pub fn check(ir: SchemaIr) -> Result<(SchemaIr, Schema), String> {
    let json = serde_json::to_string(&ir).map_err(|e| e.to_string())?;
    let schema = Schema::from_ir(serde_json::from_str(&json).map_err(|e| e.to_string())?)?;
    crate::migrate::snapshot(&schema)?;
    Ok((ir, schema))
}

#[cfg(test)]
mod tests;
