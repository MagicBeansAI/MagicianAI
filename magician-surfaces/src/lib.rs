//! Near-free surface modules extracted from the magician monolith:
//! thinking_map, progress_channels, evals, and counterparties — each had at
//! most one lib consumer at extraction time; the shared seams
//! (tutor_map_context, progress channel vocabulary, counterparty identity
//! types) stay lib-side.

pub mod counterparties;
pub mod evals;
pub mod thinking_map;
