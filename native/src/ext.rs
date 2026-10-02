//! Database extensions.
//!
//! An extension contributes SQL types, index access methods, operator classes and
//! functions. The catalog below knows what the common Postgres extensions provide, and
//! a schema can teach it more (`ExtensionIr::provides`). When the schema references
//! one of those names (a column of type `vector(3)`, a `gin_trgm_ops` index, a
//! `uuid_generate_v4()` default) the extension is required automatically, so the
//! migration creates it before anything uses it.
//!
//! Frontends ship the ergonomic half (field classes like `Vector(3)`, index helpers);
//! this module is the source of truth about which extension a name belongs to.

use std::collections::BTreeMap;

use crate::ir::{ExtensionIr, Provides};

struct Known {
    name: &'static str,
    types: &'static [&'static str],
    index_methods: &'static [&'static str],
    opclasses: &'static [&'static str],
    functions: &'static [&'static str],
}

const KNOWN: &[Known] = &[
    Known {
        name: "citext",
        types: &["citext"],
        index_methods: &[],
        opclasses: &[],
        functions: &[],
    },
    Known {
        name: "pg_trgm",
        types: &[],
        index_methods: &[],
        opclasses: &["gin_trgm_ops", "gist_trgm_ops"],
        functions: &["similarity", "word_similarity", "strict_word_similarity", "show_trgm"],
    },
    Known {
        name: "vector",
        types: &["vector", "halfvec", "sparsevec"],
        index_methods: &["hnsw", "ivfflat"],
        opclasses: &[
            "vector_l2_ops",
            "vector_ip_ops",
            "vector_cosine_ops",
            "vector_l1_ops",
            "halfvec_l2_ops",
            "halfvec_ip_ops",
            "halfvec_cosine_ops",
            "sparsevec_l2_ops",
            "sparsevec_ip_ops",
            "sparsevec_cosine_ops",
            "bit_hamming_ops",
            "bit_jaccard_ops",
        ],
        functions: &["l2_distance", "cosine_distance", "inner_product"],
    },
    Known {
        name: "postgis",
        types: &["geometry", "geography", "box2d", "box3d"],
        index_methods: &[],
        opclasses: &["gist_geometry_ops_2d", "gist_geometry_ops_nd", "gist_geography_ops"],
        functions: &["st_makepoint", "st_setsrid", "st_geomfromtext", "st_point"],
    },
    Known {
        name: "hstore",
        types: &["hstore"],
        index_methods: &[],
        opclasses: &["gin_hstore_ops", "gist_hstore_ops"],
        functions: &[],
    },
    Known {
        name: "btree_gist",
        types: &[],
        index_methods: &[],
        opclasses: &[
            "gist_int2_ops",
            "gist_int4_ops",
            "gist_int8_ops",
            "gist_text_ops",
            "gist_uuid_ops",
            "gist_timestamptz_ops",
            "gist_date_ops",
        ],
        functions: &[],
    },
    Known {
        name: "btree_gin",
        types: &[],
        index_methods: &[],
        opclasses: &["int4_ops", "int8_ops", "text_ops", "uuid_ops"],
        functions: &[],
    },
    Known {
        name: "bloom",
        types: &[],
        index_methods: &["bloom"],
        opclasses: &[],
        functions: &[],
    },
    Known {
        name: "pgcrypto",
        types: &[],
        index_methods: &[],
        opclasses: &[],
        // gen_random_uuid() is in core Postgres since 13, so it is not listed.
        functions: &["crypt", "gen_salt", "digest", "hmac", "pgp_sym_encrypt", "pgp_sym_decrypt"],
    },
    Known {
        name: "uuid-ossp",
        types: &[],
        index_methods: &[],
        opclasses: &[],
        functions: &["uuid_generate_v1", "uuid_generate_v1mc", "uuid_generate_v4", "uuid_nil"],
    },
    Known {
        name: "unaccent",
        types: &[],
        index_methods: &[],
        opclasses: &[],
        functions: &["unaccent"],
    },
];

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
enum Kind {
    Type,
    IndexMethod,
    Opclass,
    Function,
}

/// Name -> extension lookup built from the built-in catalog and the schema's own
/// extension declarations (which win).
pub struct Catalog {
    owners: BTreeMap<(Kind, String), String>,
}

impl Catalog {
    pub fn new(declared: &[ExtensionIr]) -> Self {
        let mut owners = BTreeMap::new();
        let mut add = |kind, name: &str, ext: &str| {
            owners.insert((kind, name.to_ascii_lowercase()), ext.to_owned());
        };
        for k in KNOWN {
            k.types.iter().for_each(|n| add(Kind::Type, n, k.name));
            k.index_methods.iter().for_each(|n| add(Kind::IndexMethod, n, k.name));
            k.opclasses.iter().for_each(|n| add(Kind::Opclass, n, k.name));
            k.functions.iter().for_each(|n| add(Kind::Function, n, k.name));
        }
        for e in declared {
            let Provides { types, index_methods, opclasses, functions } = &e.provides;
            types.iter().for_each(|n| add(Kind::Type, n, &e.name));
            index_methods.iter().for_each(|n| add(Kind::IndexMethod, n, &e.name));
            opclasses.iter().for_each(|n| add(Kind::Opclass, n, &e.name));
            functions.iter().for_each(|n| add(Kind::Function, n, &e.name));
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
    fn declared_extensions_extend_the_catalog() {
        let c = Catalog::new(&[ExtensionIr {
            name: "acme".into(),
            schema: None,
            version: None,
            provides: Provides { types: vec!["acme_money".into()], ..Default::default() },
        }]);
        assert_eq!(c.for_type("acme_money"), Some("acme"));
    }
}
