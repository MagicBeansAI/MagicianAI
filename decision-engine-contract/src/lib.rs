//! The wire between the decision engine and its hosts (plan Part IV, §30).
//!
//! Everything a host sends or receives lives here and nothing else does:
//! the typed-decision IR, the shared action requests and verdicts, and the
//! endpoint bodies. Changing this crate is the one engine change that still
//! costs a host rebuild, so it changes rarely and additively; every body
//! carries [`CONTRACT_VERSION`] and a major mismatch reads as "unbound".

pub mod action;
pub mod batch;
pub mod classification;
pub mod error;
pub mod identity;
pub mod primitives;
pub mod request;
pub mod settings;
pub mod telemetry;
pub mod wire;

#[cfg(feature = "client")]
pub mod client;

pub use error::DecisionError;
pub use identity::ModelIdentity;
pub use wire::*;
