//! Pure-logic foundation extracted from the `magician` monolith.
//!
//! Layer-1 extraction target: modules with no dependencies on the rest of
//! `magician_v2` move here first; the `magician` crate re-exports them so
//! existing call sites keep compiling unchanged.

pub mod attention_funnel;
pub mod attention_funnel_store;
pub mod blocking_admission;
pub mod confidence;
pub mod config_extras;
pub mod durable_io;
pub mod gws_cli;
pub mod history;
pub mod hitl;
pub mod json_traversal;
pub mod local_resource_governor;
pub mod prompts;
pub mod slot_graph;
