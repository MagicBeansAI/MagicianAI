//! Trusted-store integrity for the security-bearing agent-writable stores. The
//! live guarantee is an in-process, per-boot [`TrustAuthority`] (unforgeable —
//! populated only by magician, never by the shell) plus a `content_hash` verified
//! at the decision site. See docs/plans/2026-07-10-trusted-store-integrity-design.md.
//!
//! Elevation approvals remain memory-anchored and intentionally expire across a
//! restart. Execution-local LLM routing is different: ordinary pauses must resume
//! after restart, so its write-once sidecar carries a scope-keyed durable HMAC via
//! `durable_routing`. [`BootKey`] remains reserved and is not on either live path.
mod authority;
mod boot_key;
pub mod durable_attenuation;
mod durable_launch;
mod durable_routing;

pub use authority::{
    install_process_authority, process_authority, AuthorityEntry, StoreKind, TrustAuthority,
    TrustedRecordKey,
};
pub use boot_key::BootKey;
pub use durable_attenuation::{
    plane_attenuation_seal_path, seal_plane_attenuation, verify_plane_attenuation_seal,
    PlaneAttenuationIntegritySeal,
};
pub use durable_launch::{
    accepted_runtime_launch_seal_path, seal_accepted_runtime_launch,
    verify_accepted_runtime_launch_seal, AcceptedExplicitDelegation, AcceptedRuntimeLaunchIntent,
    AcceptedRuntimeLaunchIntentSeal, AcceptedRuntimeLaunchMode, AcceptedRuntimeLaunchState,
    SealedAcceptedRuntimeLaunch, ACCEPTED_RUNTIME_LAUNCH_SCHEMA_VERSION,
};
pub use durable_routing::{
    execution_routing_seal_path, seal_execution_routing, verify_execution_routing_seal,
    ExecutionRoutingIntegritySeal,
};
