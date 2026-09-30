//! Proactive Resurfacing Engine (Phase 1) — comms side.
//!
//! Design: `docs/plans/2026-07-07-proactive-resurfacing-design.md`;
//! implementation plan: `docs/plans/2026-07-07-proactive-resurfacing-implementation.md`.
//!
//! Plan workstream 3.0 relocated the corpus-generic engine (types, SQLite
//! store, scorer, centrality, scoring, memory effects, and the memory /
//! task-episode corpus sources) lib-side to
//! `magician::magician_v2::attention::resurfacing`. Phase 5 (batch 5 of the
//! 2026-08-28 removal inventory) removed the glob re-export that mirrored
//! that tree under this path — import the engine modules from the lib path
//! directly. The comms-coupled wiring stays here:
//!
//! * `worker` — spawns the loops and holds `ChannelAssistStore` for
//!   the comms source and routing repair.
//! * `curator` — surfaces candidates and writes channel annotation /
//!   required-action state.
//! * `actions` / `interaction` — contextual actions and detail resolution,
//!   including the follow-up lane over `ChannelAssistStore`.
//! * `sources::comms` — the comms corpus source adapter implementing the
//!   lib-side `ResurfacingSource` trait.

pub mod actions;
pub mod curator;
pub mod interaction;
pub mod memory_connections;
pub mod source_refs;
pub mod sources;
pub mod worker;

/// End-to-end regression harness tying scorer → curator → feedback → retention
/// together (per-unit tests live beside each stage). Test-only.
#[cfg(any(test, feature = "test-fixtures"))]
mod regression;

#[cfg(test)]
mod relocation_tests {
    //! Plan workstream 3.0 moved the engine core lib-side; Phase 5 (batch 5)
    //! removed the glob mirror. What remains pinned here is the genuine seam:
    //! the comms adapter satisfies the lib-side source trait.

    use magician::magician_v2::attention::resurfacing::sources::ResurfacingSource;

    use crate::channel_assist::resurfacing::sources::comms::CommsSource;

    fn satisfies_resurfacing_source<S: ResurfacingSource>() {}

    #[test]
    fn comms_source_adapter_satisfies_lib_resurfacing_source_trait() {
        satisfies_resurfacing_source::<CommsSource>();
    }
}
