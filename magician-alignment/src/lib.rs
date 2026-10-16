//! Preference-alignment core, kept apart from the `magician` crate so the
//! rules that decide how an agent intervenes build and replay in seconds.
//!
//! - [`intention`]: the owner intentions a check is asked about.
//! - [`contract`]: `alignment_input.v1`, `alignment_decision.v2` and the host
//!   validator every judge's answer goes through.
//! - [`composer`]: the host composer for typed decision models (pack
//!   `action_alignment/1.1.0`).
//! - [`args`]: the canonical form of an act's arguments.
//! - [`eval_case`]: the portable eval case (`preference_alignment_case.v1`)
//!   as host input.
//! - [`replay`]: recompose recorded typed-model answers into predictions
//!   (`alignment-replay`).
//!
//! `magician` re-exports these modules under
//! `magician_v2::preference_alignment`, so host code keeps its paths.

pub mod args;
pub mod composer;
pub mod contract;
pub mod eval_case;
pub mod intention;
pub mod replay;
