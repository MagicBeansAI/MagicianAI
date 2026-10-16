//! Core attention service: the corpus-generic resurfacing and attention
//! learning engines (plan workstream 3.0,
//! `docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md`).
//!
//! These engines lived in `magician-comms/src/channel_assist/` by packaging
//! history; they are substrate-generic (memory, tasks, episodes, comms,
//! calendar) and moved lib-side behind the existing
//! [`magician_v2::resurfacing_seam`] boundary. `magician-comms` keeps re-export
//! shims at the old paths, and the comms-coupled pieces (the comms corpus
//! source adapter, the curation/action/interaction wiring that holds
//! `ChannelAssistStore`, and the mail-fed attention workers) stayed
//! satellite-side implementing these traits.

pub mod learning;
pub mod resurfacing;
