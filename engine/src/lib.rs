//! The ORM engine shared by the language bindings: query IR → SQL (`plan`), database
//! drivers (`db`), running plans (`exec`) and migrations (`migrate`).
//!
//! Bindings send the schema once and then one IR document per operation, with its
//! literal values in a separate list in their own representation ([`Params`]). They
//! plan synchronously, run the plan on the async runtime, and build their objects from
//! the decoded rows ([`db::Cell`]) in one pass.

pub mod advisory;
pub mod db;
pub mod error;
pub mod exec;
pub mod migrate;
pub mod params;
pub mod plan;

pub use error::{Error, Result};
pub use params::{NoParams, Params};

use orm_core::ir::Operation;

/// Parses a query IR document.
pub fn parse_op(op_json: &str) -> Result<Operation> {
    serde_json::from_str(op_json).map_err(|e| Error::query(format!("invalid query IR: {e}")))
}

#[cfg(feature = "composition")]
pub mod behavior;

#[cfg(feature = "composition")]
pub mod ownership;
#[cfg(feature = "proxy-models")]
pub mod proxy;
