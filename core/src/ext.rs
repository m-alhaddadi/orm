//! Database extensions, described by TOML files.
//!
//! An extension file says what an extension adds to the schema language:
//!
//! ```toml
//! name = "vector"                       # the CREATE EXTENSION name
//! index_methods = ["hnsw", "ivfflat"]
//! opclasses = ["vector_cosine_ops"]
//! functions = ["cosine_distance"]
//!
//! [types.vector]                        # usable as a field type: `embedding: vector(384)`
//! sql = "vector({dims})"                # SQL type; {arg} placeholders come from `args`
//! args = ["dims"]
//! defaults = { dims = "3" }             # optional default per argument
//! value = "json"                        # how values travel: text, json, big_int, ...
//! read = "CAST(CAST({} AS text) AS jsonb)"            # optional; {} is the column
//! write = "CAST(CAST({} AS text) AS vector({dims}))"  # optional; {} is the bound value
//! python = "list[float]"                # optional type hints for generated code
//! typescript = "number[]"
//! ```
//!
//! The common Postgres extensions ship with the core (`extensions/postgres/*.toml`);
//! a schema imports more with `import "path/to/ext.toml"`. Referencing anything an
//! extension provides (a type, an index method, an operator class, a function called
//! in a default or predicate) makes the migration `CREATE EXTENSION` it.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::ir::{ColType, ExtensionIr, Provides};

const BUILTIN: &[(&str, &str)] = &[
    ("bloom", include_str!("../extensions/postgres/bloom.toml")),
    ("btree_gin", include_str!("../extensions/postgres/btree_gin.toml")),
    ("btree_gist", include_str!("../extensions/postgres/btree_gist.toml")),
    ("citext", include_str!("../extensions/postgres/citext.toml")),
    ("hstore", include_str!("../extensions/postgres/hstore.toml")),
    ("pg_trgm", include_str!("../extensions/postgres/pg_trgm.toml")),
    ("pgcrypto", include_str!("../extensions/postgres/pgcrypto.toml")),
    ("postgis", include_str!("../extensions/postgres/postgis.toml")),
    ("unaccent", include_str!("../extensions/postgres/unaccent.toml")),
    ("uuid-ossp", include_str!("../extensions/postgres/uuid-ossp.toml")),
    ("vector", include_str!("../extensions/postgres/vector.toml")),
];

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct ExtensionDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub types: BTreeMap<String, TypeDef>,
    #[serde(default)]
    pub index_methods: Vec<String>,
    #[serde(default)]
    pub opclasses: Vec<String>,
    #[serde(default)]
    pub functions: Vec<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct TypeDef {
    #[serde(default)]
    pub sql: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub defaults: BTreeMap<String, String>,
    pub value: ColType,
    #[serde(default)]
    pub read: Option<String>,
    #[serde(default)]
    pub write: Option<String>,
    #[serde(default)]
    pub python: Option<String>,
    #[serde(default)]
    pub typescript: Option<String>,
}

/// An extension type applied to arguments: what a field of that type becomes.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedType {
    pub extension: String,
    pub value: ColType,
    pub db_type: String,
    pub read_sql: Option<String>,
    pub write_sql: Option<String>,
    /// language -> type hint
    pub hints: BTreeMap<String, String>,
}

impl ExtensionDef {
    pub fn parse(source: &str) -> Result<Self, String> {
        let def: ExtensionDef = toml::from_str(source).map_err(|e| e.to_string())?;
        if def.name.trim().is_empty() {
            return Err("extension name is empty".into());
        }
        for (name, t) in &def.types {
            for key in t.defaults.keys() {
                if !t.args.contains(key) {
                    return Err(format!("type {name}: default for unknown argument {key:?}"));
                }
            }
        }
        Ok(def)
    }

    pub fn builtin() -> Vec<ExtensionDef> {
        BUILTIN
            .iter()
            .map(|(file, src)| Self::parse(src).unwrap_or_else(|e| panic!("built-in extension {file}: {e}")))
            .collect()
    }

    /// The names this extension provides, as carried in the schema IR.
    pub fn provides(&self) -> Provides {
        Provides {
            types: self.types.keys().cloned().collect(),
            index_methods: self.index_methods.clone(),
            opclasses: self.opclasses.clone(),
            functions: self.functions.clone(),
        }
    }
}

impl TypeDef {
    /// `name(args...)` -> SQL type and conversion templates.
    pub fn resolve(&self, extension: &str, name: &str, args: &[String]) -> Result<ResolvedType, String> {
        if args.len() > self.args.len() {
            return Err(format!(
                "type {name} takes {} argument(s) ({}), got {}",
                self.args.len(),
                self.args.join(", "),
                args.len()
            ));
        }
        let mut values = BTreeMap::new();
        for (i, arg) in self.args.iter().enumerate() {
            let v = match args.get(i) {
                Some(v) => v.clone(),
                None => self
                    .defaults
                    .get(arg)
                    .cloned()
                    .ok_or_else(|| format!("type {name} needs argument {arg:?}: {name}({})", self.args.join(", ")))?,
            };
            values.insert(arg.clone(), v);
        }
        let fill = |template: &str| {
            values.iter().fold(template.to_owned(), |t, (k, v)| t.replace(&format!("{{{k}}}"), v))
        };
        let mut hints = BTreeMap::new();
        if let Some(h) = &self.python {
            hints.insert("python".to_owned(), h.clone());
        }
        if let Some(h) = &self.typescript {
            hints.insert("typescript".to_owned(), h.clone());
        }
        Ok(ResolvedType {
            extension: extension.to_owned(),
            value: self.value,
            db_type: fill(self.sql.as_deref().unwrap_or(name)),
            read_sql: self.read.as_deref().map(fill),
            write_sql: self.write.as_deref().map(fill),
            hints,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
enum Kind {
    Type,
    IndexMethod,
    Opclass,
    Function,
}

/// Name -> extension lookup over the built-in extensions plus the ones a schema
/// declares or imports (which win).
pub struct Catalog {
    owners: BTreeMap<(Kind, String), String>,
}

impl Catalog {
    pub fn new(extra: &[ExtensionIr]) -> Self {
        let mut owners = BTreeMap::new();
        let mut add = |ext: &str, p: &Provides| {
            let mut put = |kind, name: &str| {
                owners.insert((kind, name.to_ascii_lowercase()), ext.to_owned());
            };
            p.types.iter().for_each(|n| put(Kind::Type, n));
            p.index_methods.iter().for_each(|n| put(Kind::IndexMethod, n));
            p.opclasses.iter().for_each(|n| put(Kind::Opclass, n));
            p.functions.iter().for_each(|n| put(Kind::Function, n));
        };
        for def in ExtensionDef::builtin() {
            add(&def.name, &def.provides());
        }
        for e in extra {
            add(&e.name, &e.provides);
        }
        Catalog { owners }
    }

    fn get(&self, kind: Kind, name: &str) -> Option<&str> {
        self.owners.get(&(kind, unqualified(name).to_ascii_lowercase())).map(String::as_str)
    }

    /// Extension providing a SQL type such as `vector(3)` or `geometry(Point, 4326)[]`.
    pub fn for_type(&self, sql_type: &str) -> Option<&str> {
        let base = sql_type.split(['(', '[']).next().unwrap_or("").trim();
        self.get(Kind::Type, base)
    }

    pub fn for_index_method(&self, method: &str) -> Option<&str> {
        self.get(Kind::IndexMethod, method)
    }

    pub fn for_opclass(&self, opclass: &str) -> Option<&str> {
        self.get(Kind::Opclass, opclass)
    }

    /// Extensions whose functions are called in a SQL expression (`name(` tokens).
    pub fn for_expr<'a>(&'a self, sql: &str) -> Vec<&'a str> {
        let mut out = vec![];
        for name in called_functions(sql) {
            if let Some(ext) = self.get(Kind::Function, &name) {
                if !out.contains(&ext) {
                    out.push(ext);
                }
            }
        }
        out
    }
}

fn unqualified(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name).trim_matches('"')
}

/// Identifiers directly followed by `(` outside string literals: the functions `sql`
/// calls. Good enough to spot extension functions in defaults and predicates.
fn called_functions(sql: &str) -> Vec<String> {
    let (mut out, mut ident, mut in_str) = (vec![], String::new(), false);
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            in_str = c != '\'';
            continue;
        }
        if c == '\'' {
            in_str = true;
            ident.clear();
        } else if c.is_alphanumeric() || c == '_' || c == '.' {
            ident.push(c);
        } else {
            if c == '(' && !ident.is_empty() {
                out.push(ident.clone());
            }
            if !c.is_whitespace() || chars.peek() != Some(&'(') {
                ident.clear();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_files_parse() {
        let defs = ExtensionDef::builtin();
        assert!(defs.iter().any(|d| d.name == "vector" && d.types.contains_key("vector")));
    }

    #[test]
    fn finds_extension_functions() {
        let c = Catalog::new(&[]);
        assert_eq!(c.for_expr("uuid_generate_v4()"), vec!["uuid-ossp"]);
        assert_eq!(c.for_expr("similarity(title, 'uuid_generate_v4()') > 0.3"), vec!["pg_trgm"]);
        assert!(c.for_expr("now()").is_empty());
        assert_eq!(c.for_type("vector(3)"), Some("vector"));
        assert_eq!(c.for_type("public.citext"), Some("citext"));
        assert_eq!(c.for_opclass("gin_trgm_ops"), Some("pg_trgm"));
    }

    #[test]
    fn types_resolve_with_arguments_and_defaults() {
        let defs = ExtensionDef::builtin();
        let vector = &defs.iter().find(|d| d.name == "vector").unwrap().types["vector"];
        let t = vector.resolve("vector", "vector", &["384".into()]).unwrap();
        assert_eq!(t.db_type, "vector(384)");
        assert_eq!(t.write_sql.as_deref(), Some("CAST(CAST({} AS text) AS vector(384))"));
        assert!(vector.resolve("vector", "vector", &[]).unwrap_err().contains("dims"));
        let geo = &defs.iter().find(|d| d.name == "postgis").unwrap().types["geometry"];
        assert_eq!(geo.resolve("postgis", "geometry", &["Point".into()]).unwrap().db_type, "geometry(Point, 4326)");
    }

    #[test]
    fn extension_files_are_checked() {
        assert!(ExtensionDef::parse("name = \"x\"\nbogus = 1").is_err());
        assert!(ExtensionDef::parse("name = \"x\"\n[types.t]\nvalue = \"nope\"").is_err());
        let acme = ExtensionDef::parse("name = \"acme\"\n[types.acme_money]\nsql = \"numeric(12, 2)\"\nvalue = \"text\"").unwrap();
        let c = Catalog::new(&[ExtensionIr {
            name: acme.name.clone(),
            schema: None,
            version: None,
            provides: acme.provides(),
        }]);
        assert_eq!(c.for_type("acme_money"), Some("acme"));
    }
}
