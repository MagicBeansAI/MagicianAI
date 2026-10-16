//! Near-free surface modules extracted from the magician monolith:
//! thinking_map, progress_channels, evals, and counterparties — each had at
//! most one lib consumer at extraction time; the shared seams
//! (tutor_map_context, progress channel vocabulary, counterparty identity
//! types) stay lib-side.

// Proving `Send` for eval futures that hold `AgentRuntime` walks the same deep
// runtime type graph that `magician` raises its limit for; match it.
#![recursion_limit = "256"]

pub mod counterparties;
pub mod evals;
pub mod thinking_map;
