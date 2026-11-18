//! Nova Memory's local trust core.
//!
//! The crate deliberately keeps connectors and models outside the trusted core.
//! The legacy Vault requires explicit confirmation to create canonical memory.
//! `compilation` defines the separate automatic-ingestion contract; it does not
//! change legacy persistence, confirmation, permissions or runtime routes.

mod collections;
pub mod compilation;
pub mod intake;
mod crypto;
pub mod engine;
mod error;
mod model;
pub mod portable;
mod query;
mod vault;

pub use collections::*;
pub use crypto::VaultKey;
pub use error::{NovaError, Result};
pub use model::*;
pub use query::*;
pub use vault::Vault;
pub use vault::migration_types::*;

pub const NMDS_VERSION: &str = "0.2.0";
pub const POLICY_VERSION: &str = "p0.1";

pub mod structure;
pub use structure::*;
