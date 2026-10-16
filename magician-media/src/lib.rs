//! The media plane — voice orchestration, streaming STT/diarization, fluid
//! audio, runtime config, providers, and the meeting engines — extracted from
//! the magician monolith as a satellite crate. The cross-module vocabulary
//! (meeting thread identity/continuity, speech segments, host automation,
//! dev-server detection, the meeting manager) stays lib-side in
//! `magician_v2::media_seam`.

// Match the monolith's auto-trait proof depth for delegated chat futures.
// This is a compiler bound, not a runtime thread-stack or concurrency setting.
#![recursion_limit = "256"]

pub mod media_rails;
pub mod obligation_sweeps;
pub mod reply_routing;
pub mod run_inbox;
pub mod run_state;
pub mod scheduling;

/// Flat re-export so downstream crates get one media facade.
pub use media_rails::*;
