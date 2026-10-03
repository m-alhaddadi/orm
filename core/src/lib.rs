//! Language-independent ORM core.
//!
//! ```text
//! schema.prisma ──dsl──▶ SchemaIr ──schema──▶ validated Schema ──migrate──▶ snapshots, DDL
//!                           │                        └──codegen──▶ models for each language
//!                           └──(JSON)──▶ language bindings (Python today, JS next)
//! ```
//!
//! Nothing here knows about Python, JS or a database driver: bindings and the `orm`
//! CLI are thin layers over these modules.

pub mod codegen;
pub mod dialect;
pub mod dsl;
pub mod ext;
pub mod ir;
pub mod migrate;
pub mod schema;

pub mod features;
