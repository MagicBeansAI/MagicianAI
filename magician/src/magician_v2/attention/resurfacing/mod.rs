//! Proactive Resurfacing Engine (Phase 1).
//!
//! Design: `docs/plans/2026-07-07-proactive-resurfacing-design.md`;
//! implementation plan: `docs/plans/2026-07-07-proactive-resurfacing-implementation.md`.
//!
//! The engine scans durable substrate (memory, tasks, episodes, comms,
//! calendar), scores each item's salience from a small bundle of signals,
//! and resurfaces the most relevant candidates to the owner with cooldowns
//! and dismissal feedback. This module holds the provider-neutral core
//! types shared by the SQLite store, scorer, and worker.
//!
//! Plan workstream 3.0 relocated this engine lib-side (it is corpus-generic,
//! not comms-specific). The comms-coupled wiring — the worker/curator/action
//! services that hold `ChannelAssistStore`, the comms corpus source adapter,
//! and the interaction registry — remained in `magician-comms`'s
//! `channel_assist::resurfacing` module; its Phase 5 glob re-export of this
//! tree was removed (batch 5 of the 2026-08-28 removal inventory), so this
//! module is the only import root for the engine types.

pub mod centrality;
pub mod interaction;
pub mod memory_connections;
pub mod memory_context;
pub mod memory_effect_review;
pub mod memory_effects;
pub mod memory_stage2;
pub mod scorer;
pub mod scoring;
pub mod source_refs;
pub mod sources;
pub mod store;
pub mod types;
