//! Chat Mode — conversational LLM interface layered on top of the task system.
//!
//! Ask-mode chat requires the agentic runtime and delegates actionable work
//! through it. The old chat-local tool registry path has been removed.

pub mod chat_store_sink;
pub mod chat_turn_event_sink;
pub(crate) mod decision_rail;
pub mod enrollment;
pub mod envoy;
pub mod envoy_claims;
pub mod escalation_listener;
/// The seam that joins a proved inbound identity to an open engagement's lane.
pub mod inbound_authority;
/// The chat ingress's seam into the counterparty register: who wrote to us, and
/// what that answer may be used for. Also the only automatic path that writes to
/// the register — one `observe` line per identified inbound message.
pub mod inbound_sender;
/// The server-published lane invoke-grammar catalog (plan 1.2): generated
/// from the parser constants so the wire can never drift from the parsers.
pub mod invoke_catalog;
/// The shared invoke grammar for conversational product lanes: the VibeDev
/// rail's typed/spoken invoke parsing moved here from `tutor.rs` (plan 1.2b),
/// which re-exports it so existing import paths stay valid.
pub mod invoke_grammar;
/// The conversational lane seam: product lanes register here instead of
/// interleaving their admission and hot-tool logic into the service (plan
/// 1.2a; Brainstorm is the first registrant).
pub mod lane_seam;
pub mod llm_service;
pub mod models;
pub mod planning_listener;
pub mod presentation;
pub mod public_contact_profile;
pub mod retrieval_timing;
pub mod service;

/// The one source-surface string that marks a room, re-exported so the media
/// layer that mints it and the chat layer that reads it name the same constant
/// rather than two string literals that can drift apart.
pub use service::{voice_invocation_surface, MEETING_ROOM_SOURCE_SURFACE};
pub mod storage;
pub mod tools_runtime;
pub mod turn_secrets;
pub mod voice_requests;

/// Default agent ID used across the chat subsystem.
/// Shared constant to avoid duplicating the `"personal-assistant"` literal.
pub const DEFAULT_AGENT_ID: &str = "personal-assistant";
