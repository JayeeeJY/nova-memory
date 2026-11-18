//! N0: bounded contracts for automatic intake and compiled memory.
//!
//! These are in-memory, side-effect-free contracts, not an authenticated API,
//! model, job queue, migration or change executor. Runtime callers must obtain
//! the scope, source snapshots, protections and read set from trusted services.
//! Evidence validation checks quotations, not semantic entailment.
mod change;
mod compatibility;
mod model;
mod validate;
pub(crate) mod wire;

pub use compatibility::*;
pub use model::*;

pub const INTAKE_VERSION: &str = "nova.intake.v0.1";
pub const COMPILATION_VERSION: &str = "nova.compilation.v0.1";
pub const MAX_DOCUMENT_BYTES: usize = 512 * 1024;

/// Decode bounded untrusted JSON without silently discarding duplicate keys,
/// including keys nested inside otherwise open semantic values.
/// This is wire validation only; it grants no authority or semantic approval.
pub fn decode_untrusted_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> crate::Result<T> {
    wire::decode(bytes, "invalid_untrusted_json")
}

fn invalid(reason: &'static str) -> crate::NovaError {
    // Diagnostics contain no imported text, identifiers, paths or credentials.
    crate::NovaError::Validation(format!("compilation contract: {reason}"))
}
