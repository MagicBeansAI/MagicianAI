//! The learning plane — extracted from the magician lib.
//!
//! * `outcome_learning` — OPC outcome learning (recording, maturity policy,
//!   sweep, feeders).
//! * `data_room` — OPC deal-close container and audit log.
//! * `bots` — scoped chat-bot runtime, config store, auth HITL broker.
//! * `execution_panel` — projector, runtime store, v3 adapter; the panel's
//!   serde state vocabulary stays lib-side (`magician_v2::execution_panel`)
//!   because `realtime_events` embeds `ExecutionPanelState`.

pub mod bots;
pub mod data_room;
pub mod execution_panel;
pub mod outcome_learning;
pub mod retraction;
