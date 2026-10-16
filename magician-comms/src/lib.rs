//! The comms data plane: channel-assist (Gmail/calendar/WhatsApp/Telegram
//! ingestion, distillation, annotation), extracted from the magician monolith
//! as a satellite crate depending on the `magician` lib. The observe-config
//! substrate and the pinned LLM dispatch seam live lib-side
//! (`magician_v2::observe_connectors`, `magician_v2::llm_dispatch_seam`) and
//! are re-imported here.

pub mod channel_assist;
