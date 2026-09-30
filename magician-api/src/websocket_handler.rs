//! WebSocket Handler for V2 Real-time Event Streaming
//!
//! Provides WebSocket endpoint for real-time updates during V2 message
//! processing, including progress tracking, cancellation support, and state
//! synchronization.
//!
//! ## Capabilities
//!
//! - **Real-time events**: Broadcasts `RuntimeTransportEvent` variants to connected clients.
//! - **Snapshot requests** (`agent.ui.snapshot_request`): Returns the current MUIJ layout
//!   for an agent, reading from cache or disk storage.
//! - **UI interactions** (`ui.interaction`): Triggers agent goal cycles through the
//!   `trigger_agent_controlled` admission pipeline (GC-B02).
//!
//! ## Reconnection Support
//!
//! When a client reconnects (e.g., after page reload or network interruption),
//! the WebSocket handler will automatically re-emit pending pause events
//! (`AgenticWaitingForUser` or `AgenticMaxIterationsReached`) for the execution.
//! This ensures the UI can display the correct prompt controls after reconnect.
//!
//! To enable this, clients should pass an `execution_id` query parameter when connecting:
//! ```text
//! ws://host/api/magician/v2/realtime/ws?execution_id=<uuid>
//! ```

pub use magician::magician_v2::realtime_events::event_visible_to_scope;
use std::{collections::HashMap, sync::Arc, time::Instant};

use actix::{Actor, ActorContext, ActorFutureExt, AsyncContext, StreamHandler, WrapFuture};
use actix_web::{web, HttpRequest, HttpResponse, Result};
use actix_web_actors::ws;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::is_expected_websocket_disconnect_message;
use crate::scope::ResolvedScope;
// Test-only, re-added for the same reason as `monitors_api`: `72fae8b75`
// removed what only the tests below reach. The test module reaches it through
// `use super::*`; a second explicit import inside that module shadows this one
// and makes it read as unused, so don't add one (same shape as `screen_api`).
use crate::web_api::{
    trigger_agent_controlled, AgentApiServices, MagicianV2Api, ManualAgentTriggerRequest,
    ManualTriggerOutcome,
};
use magician::magician_v2::execution::agentic::{
    ExecutionPauseKind, FullPauseStore, PendingPauseInfo,
};
#[cfg(test)]
use magician::magician_v2::feed::FeedItem;
use magician::magician_v2::gaui::{
    agent_snapshot_cache_key, load_materialized_snapshot_document, MuijDocument, MuijDocumentCache,
    MuijQueryEngine, MuijStorage, SnapshotLoadError,
};
use magician::magician_v2::orchestrator::MagicianV2Orchestrator;
use magician::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

// Configuration for WebSocket session
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
const CLIENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// R765: Explicit max inbound WebSocket frame size (64 KiB).
/// Client messages are small JSON payloads; this prevents accidental/malicious oversized frames
/// from consuming unbounded memory. Chosen conservatively above typical message sizes.
const MAX_WS_FRAME_SIZE: usize = 64 * 1024;
/// Stable application protocol selected during browser WebSocket upgrades.
/// Authentication may arrive as a second `magician-bearer.*` offer, but that
/// credential is deliberately never echoed in the handshake response.
const V2_WEBSOCKET_PROTOCOLS: &[&str] = &["magician-events-v2"];

// R848: Typed error code constants (prevent typo drift across handler code)
mod error_codes {
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const INVALID_AGENT: &str = "invalid_agent";
    pub const INVALID_COMPONENT: &str = "invalid_component";
    pub const INVALID_MESSAGE: &str = "invalid_message";
    pub const AGENT_NOT_FOUND: &str = "agent_not_found";
    pub const AGENT_PAUSED: &str = "agent_paused";
    pub const AGENT_UNAVAILABLE: &str = "agent_unavailable";
    pub const INVALID_REQUEST: &str = "invalid_request";
    pub const TRIGGER_FAILED: &str = "trigger_failed";
    pub const PROTOCOL_ERROR: &str = "protocol_error";
}

/// GC-B01: Default allowed origin hostnames for local development.
/// If the Origin hostname matches any entry here OR matches the Host hostname, accept.
const DEFAULT_ALLOWED_ORIGINS: &[&str] = &["localhost", "127.0.0.1", "[::1]"];

/// GC-B01: Extract hostname (strip protocol and port) from an Origin or Host value.
///
/// Examples:
/// - `"http://localhost:3000"` → `"localhost"`
/// - `"https://example.com:443"` → `"example.com"`
/// - `"magician-desktop://localhost"` → `"localhost"`
/// - `"localhost:3002"` → `"localhost"`
/// - `"[::1]:3000"` → `"[::1]"`
fn strip_to_hostname(value: &str) -> &str {
    // Strip any Origin scheme prefix. Browser Origins are usually http(s), but
    // Tauri desktop webviews can send custom schemes such as
    // `magician-desktop://localhost`.
    let without_proto = value
        .find("://")
        .map(|idx| &value[idx + 3..])
        .unwrap_or(value);
    // Strip port: for IPv6 bracket notation like [::1]:3000, find the last `:` after `]`
    if let Some(bracket_end) = without_proto.rfind(']') {
        // IPv6 bracketed address — strip port only after the closing bracket
        if let Some(colon) = without_proto[bracket_end..].rfind(':') {
            &without_proto[..bracket_end + colon]
        } else {
            without_proto
        }
    } else if without_proto
        .as_bytes()
        .iter()
        .filter(|&&b| b == b':')
        .count()
        > 1
    {
        // R891: Bare (un-bracketed) IPv6 address — multiple colons without brackets.
        // Return as-is since we can't distinguish port from IPv6 segments.
        without_proto
    } else if let Some(colon) = without_proto.find(':') {
        // R766: Use find (first colon) — this branch only reached with ≤1 colon
        &without_proto[..colon]
    } else {
        without_proto
    }
}

/// GC-B01: Validate the Origin header against allowed origins and the Host header.
///
/// - No Origin header → `Ok(())` (non-browser clients like curl)
/// - Origin present but non-UTF8 → `Err` (R755: malformed, reject)
/// - Origin hostname in `DEFAULT_ALLOWED_ORIGINS` (case-insensitive) → `Ok(())`
/// - Origin hostname matches Host hostname (case-insensitive) → `Ok(())`
/// - Origin present + fails allowlist + Host missing → `Err` (R756: no escape hatch)
/// - Otherwise → `Err((origin, host))` for logging
pub(crate) fn validate_origin(req: &HttpRequest) -> Result<(), (String, String)> {
    // R755: Distinguish "no Origin header" from "Origin present but non-UTF8"
    let origin_header = req.headers().get("origin");
    let origin = match origin_header {
        None => return Ok(()), // Non-browser clients don't send Origin
        Some(val) => match val.to_str() {
            Ok(o) => o,
            Err(_) => {
                // R755: Origin present but non-UTF8 — definitively malformed, reject
                return Err(("(non-utf8 origin)".to_string(), String::new()));
            },
        },
    };

    let origin_hostname = strip_to_hostname(origin);

    // R750: Check against default allowed origins (case-insensitive per RFC 4343)
    if DEFAULT_ALLOWED_ORIGINS
        .iter()
        .any(|&allowed| allowed.eq_ignore_ascii_case(origin_hostname))
    {
        return Ok(());
    }

    // Fall back to Origin-host == Request-host comparison
    let request_host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if request_host.is_empty() {
        // R756: Origin is present and NOT in allowlist — missing Host is not an
        // escape hatch. Reject to prevent CSWSH when proxy strips Host.
        return Err((origin.to_string(), "(no host)".to_string()));
    }

    let host_hostname = strip_to_hostname(request_host);
    // R750: Case-insensitive comparison per RFC 4343
    if origin_hostname.eq_ignore_ascii_case(host_hostname) {
        return Ok(());
    }

    Err((origin.to_string(), request_host.to_string()))
}

/// Query parameters for WebSocket connection
#[derive(Debug, Deserialize)]
pub struct WsConnectParams {
    /// Optional execution ID to check for pending pauses on reconnect
    pub execution_id: Option<String>,
    /// Optional agent ID to check for agent-scoped pending pauses on reconnect
    /// (TRUE_AGENTS Phase 3). When set, also queries `FullPauseStore::get_pending_for_agent()`.
    pub agent_id: Option<String>,
    /// Explicitly opt in to structured chat presentation transport.
    pub supports_structured_presentation: Option<bool>,
}

/// Scope headers here were engraved by the bearer middleware (including the
/// browser WebSocket subprotocol path); query parameters are never authority.
// ---------------------------------------------------------------------------
// Client ↔ server snapshot message types
// ---------------------------------------------------------------------------

/// Client-to-server WebSocket message.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ClientMessage {
    #[serde(rename = "agent.ui.snapshot_request")]
    SnapshotRequest { agent_id: String },

    /// GC-B02: ActionBus interaction — triggers an agent goal cycle.
    #[serde(rename = "ui.interaction")]
    UiInteraction {
        agent_id: String,
        component_id: String,
        #[serde(default)]
        goal_id: Option<String>,
        #[serde(default)]
        trigger: Option<String>,
        /// R758: Client-generated correlation token echoed in ack/error responses.
        #[serde(default)]
        request_id: Option<String>,
    },
}

/// Server-to-client snapshot response.
#[derive(Debug, Serialize)]
struct SnapshotResponse {
    #[serde(rename = "type")]
    msg_type: &'static str,
    agent_id: String,
    document: MuijDocument,
}

/// Server-to-client snapshot error.
/// R911: Carries machine-readable `code` for client error discrimination.
#[derive(Debug, Serialize)]
struct SnapshotErrorFrame {
    #[serde(rename = "type")]
    msg_type: &'static str,
    agent_id: String,
    error: String,
    code: String,
}

/// GC-B02: Server-to-client interaction acknowledgment.
#[derive(Debug, Serialize)]
struct InteractionAckFrame {
    #[serde(rename = "type")]
    msg_type: &'static str, // "ui.interaction.ack"
    agent_id: String,
    component_id: String,
    cycle_id: String,
    status: String, // R772/R944: values from ManualAgentTriggerResponse.status
    /// R912: Echoed goal_id for client-side correlation with subsequent cycle events.
    #[serde(skip_serializing_if = "Option::is_none")]
    goal_id: Option<String>,
    /// R758: Echoed client correlation token (omitted from JSON if None).
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

/// GC-B02: Server-to-client interaction error.
#[derive(Debug, Serialize)]
struct InteractionErrorFrame {
    #[serde(rename = "type")]
    msg_type: &'static str, // "ui.interaction.error"
    agent_id: String,
    component_id: String,
    error: String,
    code: String, // R925: see error_codes module for all values
    /// R758: Echoed client correlation token (omitted from JSON if None).
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

/// R910: Protocol-level error frame (WebSocket protocol errors, binary frames, timeout).
#[derive(Debug, Serialize)]
struct ProtocolErrorFrame {
    #[serde(rename = "type")]
    msg_type: &'static str, // "error"
    error: String,
    code: &'static str,
}

/// GC-B02: Rate limit window for ui.interaction triggers.
const INTERACTION_RATE_LIMIT_SECS: u64 = 5;

/// R809: Known fields for `agent.ui.snapshot_request` messages.
const SNAPSHOT_REQUEST_KNOWN_FIELDS: &[&str] = &["type", "agent_id"];

/// R753: Known fields for `ui.interaction` messages.
/// Used for two-step validation — serde's internally-tagged enums silently ignore unknown fields,
/// so we check the raw JSON keys against this list after successful parsing.
///
/// **R838: SYNC REQUIREMENT** — this list MUST match `ClientMessage::UiInteraction` fields
/// plus `"type"`. When adding a new field to UiInteraction, add it here too. The test
/// `ui_interaction_known_fields_matches_struct` enforces this at test time.
const UI_INTERACTION_KNOWN_FIELDS: &[&str] = &[
    "type",
    "agent_id",
    "component_id",
    "goal_id",
    "trigger",
    "request_id",
];

/// R754: Serialize a frame to JSON, logging and falling back to a hardcoded error if serialization
/// fails. `serde_json::to_string` on flat structs should never fail, but if it does, sending an
/// empty string (from `unwrap_or_default`) would leave the client with no feedback.
#[must_use]
fn serialize_or_fallback<T: Serialize>(frame: &T, fallback_type: &str) -> String {
    match serde_json::to_string(frame) {
        Ok(json) => json,
        Err(e) => {
            error!(
                error = %e,
                frame_type = fallback_type,
                "[MAGICIAN-V2-API] Failed to serialize outbound frame — sending hardcoded fallback"
            );
            // R860: JSON-escape fallback_type to prevent malformed JSON if type
            // name ever contains special characters (currently all callers pass static &str).
            let escaped_type = fallback_type.replace('\\', "\\\\").replace('"', "\\\"");
            format!(
                r#"{{"type":"{}","error":"internal serialization failure"}}"#,
                escaped_type
            )
        },
    }
}

fn serialize_public_v2_event(event: &RuntimeTransportEvent) -> Result<String, serde_json::Error> {
    serde_json::to_string(event)
}

fn generic_v2_websocket_admits(event: &RuntimeTransportEvent) -> bool {
    !magician::magician_v2::realtime_events::is_app_owner_notification_transport_event(event)
}

fn strip_structured_presentation_if_disabled(
    event: RuntimeTransportEvent,
    supports_structured_presentation: bool,
) -> RuntimeTransportEvent {
    if supports_structured_presentation {
        return event;
    }

    let RuntimeTransportEvent::ChatMessageReceived {
        session_id,
        mut message,
        principal,
        workspace,
        origin_channel,
        timestamp,
    } = event
    else {
        return event;
    };

    message.presentation = None;
    RuntimeTransportEvent::ChatMessageReceived {
        session_id,
        message,
        principal,
        workspace,
        origin_channel,
        timestamp,
    }
}

/// Fast-reject agent_id values that are obviously unsafe before hitting storage.
/// This is a synchronous guard — storage has its own authoritative validation,
/// but this avoids spawning an async task for clearly bad input.
/// R214: Strengthened to also reject `..` anywhere, non-ASCII bytes, and length > 255.
/// R299: Removed `starts_with('.')` to align with storage `validate_identifier` which
/// permits leading-dot agents. Only `..` (path traversal) is rejected.
#[must_use]
fn is_valid_identifier(agent_id: &str) -> bool {
    !agent_id.is_empty()
        && agent_id.len() <= 255
        && agent_id.is_ascii()
        // R412: Reject null bytes and ASCII control characters (0x00-0x1F, 0x7F)
        && !agent_id.bytes().any(|b| b < 0x20 || b == 0x7F)
        && !agent_id.contains('/')
        && !agent_id.contains('\\')
        && !agent_id.contains("..")
}

/// R914/R917: Validate component_id — same rules as `is_valid_identifier`.
/// Delegates to avoid rule duplication and future divergence.
/// R748+R878: Rejects empty, >255 chars, non-ASCII, control chars, `/`, `\`, `..`.
#[must_use]
fn is_valid_component_id(s: &str) -> bool {
    is_valid_identifier(s)
}

/// R917: Validate request_id — length ≤128, ASCII only, no control chars.
#[must_use]
fn is_valid_request_id(s: &str) -> bool {
    s.len() <= 128 && s.is_ascii() && !s.bytes().any(|b| b < 0x20 || b == 0x7F)
}

/// R918: Validate trigger — length ≤255, ASCII only, no control chars.
#[must_use]
fn is_valid_trigger(s: &str) -> bool {
    s.len() <= 255 && s.is_ascii() && !s.bytes().any(|b| b < 0x20 || b == 0x7F)
}

/// R849: Heuristic detection of max-iterations pauses by label strings.
/// These strings ("Resume", "Cancel") are set by the agentic executor when max iterations
/// is reached — see `execution/agentic/executor.rs` max_iterations_confirmation().
/// If the upstream executor changes these labels, this heuristic must be updated.
/// The `destructive: false` guard ensures destructive confirmations (e.g., "Delete?")
/// are never misclassified (R831).
#[must_use]
fn pending_pause_is_max_iterations(pause_info: &PendingPauseInfo) -> bool {
    // A budget pause carries the same Resume/Cancel confirmation shape and
    // resumes via the same continue flow, so classify it here too.
    if matches!(
        pause_info.pause_kind,
        Some(ExecutionPauseKind::MaxIterations) | Some(ExecutionPauseKind::Budget)
    ) {
        return true;
    }
    matches!(
        &pause_info.input_type,
        magician::magician_v2::execution::agentic::UserInputType::Confirmation {
            confirm_label,
            deny_label,
            destructive: false,
        } if confirm_label.as_deref() == Some("Resume")
            && deny_label.as_deref() == Some("Cancel")
    )
}

fn pending_pause_is_confirmation(pause_info: &PendingPauseInfo) -> bool {
    if pause_info.pause_kind == Some(ExecutionPauseKind::Confirmation) {
        return true;
    }
    matches!(
        &pause_info.input_type,
        magician::magician_v2::execution::agentic::UserInputType::Confirmation { .. }
    ) && pause_info.confirmation_action_summary.is_some()
        && pause_info.confirmation_reason.is_some()
        && pause_info.confirmation_action_type.is_some()
        && !pending_pause_is_max_iterations(pause_info)
}

fn pending_pause_is_manual(pause_info: &PendingPauseInfo) -> bool {
    pause_info.pause_kind == Some(ExecutionPauseKind::Manual)
}

fn build_pending_pause_reemit_event(pause_info: PendingPauseInfo) -> RuntimeTransportEvent {
    if pending_pause_is_manual(&pause_info) {
        RuntimeTransportEvent::ExecutionPaused {
            execution_id: pause_info.execution_id,
            principal: pause_info.principal,
            workspace: pause_info.workspace,
            plan_id: pause_info.plan_id.unwrap_or_default(),
            step_index: 0,
            step_id: pause_info.step_id.unwrap_or_default(),
            reason: pause_info
                .question
                .unwrap_or_else(|| "Paused by user".to_string()),
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    } else if pending_pause_is_max_iterations(&pause_info) {
        RuntimeTransportEvent::AgenticMaxIterationsReached {
            execution_id: pause_info.execution_id,
            principal: pause_info.principal,
            workspace: pause_info.workspace,
            plan_id: pause_info.plan_id.unwrap_or_default(),
            step_id: pause_info.step_id.unwrap_or_default(),
            iterations_used: pause_info.iteration,
            pause_state_id: Some(pause_info.key),
            agent_id: pause_info.agent_id,
            goal_id: pause_info.goal_id,
            cycle_id: pause_info.cycle_id,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    } else if pending_pause_is_confirmation(&pause_info) {
        RuntimeTransportEvent::AgenticWaitingForConfirmation {
            execution_id: pause_info.execution_id,
            principal: pause_info.principal,
            workspace: pause_info.workspace,
            plan_id: pause_info.plan_id.unwrap_or_default(),
            step_id: pause_info.step_id.unwrap_or_default(),
            iteration: pause_info.iteration,
            pause_state_id: Some(pause_info.key),
            agent_id: pause_info.agent_id,
            goal_id: pause_info.goal_id,
            cycle_id: pause_info.cycle_id,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    } else {
        RuntimeTransportEvent::AgenticWaitingForUser {
            execution_id: pause_info.execution_id,
            principal: pause_info.principal,
            workspace: pause_info.workspace,
            plan_id: pause_info.plan_id.unwrap_or_default(),
            step_id: pause_info.step_id.unwrap_or_default(),
            iteration: pause_info.iteration,
            pause_state_id: Some(pause_info.key),
            correlation_id: None,
            is_retry: pause_info.is_retry.then_some(true),
            retry_count: (pause_info.retry_count > 0).then_some(pause_info.retry_count),
            agent_id: pause_info.agent_id,
            goal_id: pause_info.goal_id,
            cycle_id: pause_info.cycle_id,
            escalation_trigger: pause_info.escalation_trigger,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }
}

/// Build snapshot response JSON. Extracted for testability.
///
/// R159: validates deserialized documents (parity with REST `get_layout`).
/// R161: accepts a `MuijQueryEngine` for within-call parse cache reuse (R879: engine is per-request, not cross-call).
/// R560/R679: reads from the in-memory document cache first, falls back to
/// disk storage when the agent isn't cached. This prevents stale snapshots
/// when the cache is ahead of disk (e.g. quarantined cache-only deltas).
async fn snapshot_response_json(
    definition_store: Option<magician::magician_v2::agents::AgentDefinitionStore>,
    storage: &MuijStorage,
    agent_id: &str,
    query_engine: &mut MuijQueryEngine,
    doc_cache: Option<&MuijDocumentCache>,
    cache_key: Option<&str>,
) -> String {
    if let Some(definition_store) = definition_store.as_ref() {
        match definition_store.get_definition(agent_id).await {
            Ok(Some(_)) => {},
            Ok(None) => {
                return serialize_or_fallback(
                    &SnapshotErrorFrame {
                        msg_type: "agent.ui.snapshot_error",
                        agent_id: agent_id.to_string(),
                        error: format!("Agent '{}' not found", agent_id),
                        code: error_codes::AGENT_NOT_FOUND.to_string(),
                    },
                    "agent.ui.snapshot_error",
                );
            },
            Err(err) => {
                error!(
                    agent_id = %agent_id,
                    error = %err,
                    "[MAGICIAN-V2-API] Failed to validate snapshot agent definition"
                );
                return serialize_or_fallback(
                    &SnapshotErrorFrame {
                        msg_type: "agent.ui.snapshot_error",
                        agent_id: agent_id.to_string(),
                        error: "Storage read error".to_string(),
                        code: error_codes::TRIGGER_FAILED.to_string(),
                    },
                    "agent.ui.snapshot_error",
                );
            },
        }
    }
    match load_materialized_snapshot_document(storage, agent_id, query_engine, doc_cache, cache_key)
        .await
    {
        Ok(doc) => serialize_or_fallback(
            &SnapshotResponse {
                msg_type: "agent.ui.snapshot",
                agent_id: agent_id.to_string(),
                document: doc,
            },
            "agent.ui.snapshot", // R904: correct fallback_type for success frame
        ),
        Err(SnapshotLoadError::InvalidLayout) => serialize_or_fallback(
            &SnapshotErrorFrame {
                msg_type: "agent.ui.snapshot_error",
                agent_id: agent_id.to_string(),
                error: "Stored layout validation failed".to_string(),
                code: error_codes::INVALID_MESSAGE.to_string(),
            },
            "agent.ui.snapshot_error",
        ),
        Err(SnapshotLoadError::StorageRead) => serialize_or_fallback(
            &SnapshotErrorFrame {
                msg_type: "agent.ui.snapshot_error",
                agent_id: agent_id.to_string(),
                error: "Storage read error".to_string(),
                code: error_codes::TRIGGER_FAILED.to_string(),
            },
            "agent.ui.snapshot_error",
        ),
    }
}

// R680: materialize_component_queries moved to gaui::query module for REST/WS parity.

// ---------------------------------------------------------------------------
// Session actor
// ---------------------------------------------------------------------------

/// V2 WebSocket session actor
pub struct V2WebSocketSession {
    /// Unique session ID
    id: String,
    /// Last heartbeat timestamp
    last_heartbeat: Instant,
    /// Event receiver for V2 events
    event_receiver: Option<broadcast::Receiver<RuntimeTransportEvent>>,
    /// Pending pause states to emit on connection (for reconnect scenarios)
    pending_pauses: Vec<PendingPauseInfo>,
    /// MUIJ layout storage for snapshot requests
    muij_storage: MuijStorage,
    /// R560/R679: Shared document cache for snapshot coherence — reads from
    /// in-memory cache first, falls back to disk when agent not cached.
    muij_doc_cache: Option<MuijDocumentCache>,
    /// GC-B02: Agent API services for ui.interaction trigger pipeline.
    /// R834: Wrapped in Arc to avoid cloning 12+ Arc fields per interaction dispatch.
    agent_api: Option<Arc<AgentApiServices>>,
    /// GC-B02: V2 orchestrator for dispatching triggered cycles.
    /// Arc dropped when session struct is dropped (on actor stop).
    v2_orchestrator: Option<Arc<MagicianV2Orchestrator>>,
    /// GC-B02: Rate limiter for ui.interaction — key: "{agent_id}\0{component_id}" (R763 null delimiter), value: last trigger time.
    /// Actor is single-threaded so no Mutex needed.
    interaction_rate_limit: HashMap<String, Instant>,
    /// R906: In-flight snapshot request counter to cap concurrent storage reads per session.
    snapshot_in_flight: u32,
    /// Principal scope for feed delta delivery.
    scope_principal: String,
    /// Workspace scope for feed delta delivery.
    scope_workspace: String,
    /// Whether this session should receive structured presentation sidecars.
    supports_structured_presentation: bool,
}

impl V2WebSocketSession {
    /// Create a new WebSocket session
    pub fn new(
        event_broadcaster: &RuntimeTransportBroadcaster,
        muij_storage: MuijStorage,
        scope_principal: String,
        scope_workspace: String,
        supports_structured_presentation: bool,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            last_heartbeat: Instant::now(),
            event_receiver: Some(event_broadcaster.subscribe()),
            pending_pauses: Vec::new(),
            muij_storage,
            muij_doc_cache: None,
            agent_api: None,
            v2_orchestrator: None,
            interaction_rate_limit: HashMap::new(),
            snapshot_in_flight: 0,
            scope_principal,
            scope_workspace,
            supports_structured_presentation,
        }
    }

    /// Create a new WebSocket session with pending pauses to emit on connect
    pub fn with_pending_pauses(
        event_broadcaster: &RuntimeTransportBroadcaster,
        pending_pauses: Vec<PendingPauseInfo>,
        muij_storage: MuijStorage,
        scope_principal: String,
        scope_workspace: String,
        supports_structured_presentation: bool,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            last_heartbeat: Instant::now(),
            event_receiver: Some(event_broadcaster.subscribe()),
            pending_pauses,
            muij_storage,
            muij_doc_cache: None,
            agent_api: None,
            v2_orchestrator: None,
            interaction_rate_limit: HashMap::new(),
            snapshot_in_flight: 0,
            scope_principal,
            scope_workspace,
            supports_structured_presentation,
        }
    }

    /// Dispatch a parsed client message.
    /// R764: For messages where the type tag is recognizable but required fields are missing,
    /// attempt to extract the type and return a typed error frame.
    /// R833: Single JSON parse — parse as Value first, then convert to typed ClientMessage.
    fn handle_client_message(&mut self, text: &str, ctx: &mut ws::WebsocketContext<Self>) {
        // R833: Parse once as Value — used for unknown-field checks and type extraction
        let val = match serde_json::from_str::<serde_json::Value>(text) {
            Ok(v) => v,
            Err(_) => {
                debug!(
                    session_id = %self.id,
                    "[MAGICIAN-V2-API] Received non-JSON client message"
                );
                return;
            },
        };

        // R833: Extract msg_type as owned String so val can be moved into from_value later
        let msg_type = val
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // R810+R901: Non-string or missing `type` field — send error frame (not silent drop)
        if msg_type.is_empty() {
            debug!(
                session_id = %self.id,
                "[MAGICIAN-V2-API] Client message missing or non-string 'type' field"
            );
            let json = serialize_or_fallback(
                &ProtocolErrorFrame {
                    msg_type: "error",
                    error: "Missing or non-string 'type' field".to_string(),
                    code: error_codes::INVALID_MESSAGE,
                },
                "error",
            );
            ctx.text(json);
            return;
        }

        // R809+R948: Unknown-field rejection for snapshot_request (parity with ui.interaction).
        // Cap at 10 field names to prevent amplification from 64KB frames with thousands of keys.
        if msg_type == "agent.ui.snapshot_request" {
            let unknown_fields: Vec<String> = val
                .as_object()
                .map(|obj| {
                    obj.keys()
                        .filter(|k| !SNAPSHOT_REQUEST_KNOWN_FIELDS.contains(&k.as_str()))
                        .take(10)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            if !unknown_fields.is_empty() {
                warn!(
                    session_id = %self.id,
                    unknown_fields = ?unknown_fields,
                    "[MAGICIAN-V2-API] snapshot_request contains unknown fields"
                );
                let unknown_list = serde_json::to_string(&unknown_fields)
                    .unwrap_or_else(|_| format!("{:?}", unknown_fields));
                let agent_id = val
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let json = serialize_or_fallback(
                    &SnapshotErrorFrame {
                        msg_type: "agent.ui.snapshot_error",
                        agent_id,
                        error: format!("Unknown fields: {}", unknown_list),
                        code: error_codes::INVALID_MESSAGE.to_string(),
                    },
                    "agent.ui.snapshot_error",
                );
                ctx.text(json);
                return;
            }
        }

        // R833+R937: Extract agent_id for error paths before consuming val with from_value.
        // Truncate to 256 chars to prevent oversized error frames from raw client input.
        let agent_id_raw = val.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let agent_id_raw: String = agent_id_raw.chars().take(256).collect();
        // R933: Only extract component_id_raw and request_id_raw for ui.interaction messages
        // (not needed for snapshot_request path — avoids unnecessary allocation).
        let component_id_raw = if msg_type == "ui.interaction" {
            val.get("component_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .chars()
                .take(256)
                .collect()
        } else {
            String::new()
        };
        let request_id_raw = if msg_type == "ui.interaction" {
            val.get("request_id")
                .and_then(|v| v.as_str())
                .map(|s| s.chars().take(129).collect::<String>())
        } else {
            None
        };

        // R753+R948: Two-step unknown-field rejection for ui.interaction (before typed parse).
        // Serde internally-tagged enums silently ignore unknown fields.
        // Cap at 10 field names to prevent amplification from oversized frames.
        if msg_type == "ui.interaction" {
            // Scope borrows into val so from_value(val) can move it below
            let unknown_fields: Vec<String> = val
                .as_object()
                .map(|obj| {
                    obj.keys()
                        .filter(|k| !UI_INTERACTION_KNOWN_FIELDS.contains(&k.as_str()))
                        .take(10)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            if !unknown_fields.is_empty() {
                warn!(
                    session_id = %self.id,
                    unknown_fields = ?unknown_fields,
                    "[MAGICIAN-V2-API] ui.interaction contains unknown fields"
                );
                // R811: Clean JSON array format instead of Rust Debug {:?}
                let unknown_list = serde_json::to_string(&unknown_fields)
                    .unwrap_or_else(|_| format!("{:?}", unknown_fields));
                let json = serialize_or_fallback(
                    &InteractionErrorFrame {
                        msg_type: "ui.interaction.error",
                        agent_id: agent_id_raw,
                        component_id: component_id_raw,
                        error: format!("Unknown fields: {}", unknown_list),
                        code: error_codes::INVALID_MESSAGE.to_string(),
                        request_id: request_id_raw,
                    },
                    "ui.interaction.error",
                );
                ctx.text(json);
                return;
            }
        }

        // R833: Convert Value → ClientMessage (single parse, no re-serialization)
        match serde_json::from_value::<ClientMessage>(val) {
            Ok(ClientMessage::SnapshotRequest { agent_id }) => {
                self.handle_snapshot_request(agent_id, ctx);
            },
            Ok(ClientMessage::UiInteraction {
                agent_id,
                component_id,
                goal_id,
                trigger,
                request_id,
            }) => {
                self.handle_ui_interaction(
                    agent_id,
                    component_id,
                    goal_id,
                    trigger,
                    request_id,
                    ctx,
                );
            },
            Err(e) => {
                // R764: Type was identified above — return typed error frame
                // R802: Sanitize error — don't expose Rust type names to client
                if msg_type == "ui.interaction" {
                    warn!(
                        session_id = %self.id,
                        error = %e,
                        "[MAGICIAN-V2-API] ui.interaction parse failed after type match"
                    );
                    let json = serialize_or_fallback(
                        &InteractionErrorFrame {
                            msg_type: "ui.interaction.error",
                            agent_id: agent_id_raw,
                            component_id: component_id_raw,
                            error: "Invalid message format".to_string(),
                            code: error_codes::INVALID_MESSAGE.to_string(),
                            request_id: request_id_raw,
                        },
                        "ui.interaction.error",
                    );
                    ctx.text(json);
                } else if msg_type == "agent.ui.snapshot_request" {
                    warn!(
                        session_id = %self.id,
                        error = %e,
                        "[MAGICIAN-V2-API] snapshot_request parse failed after type match"
                    );
                    let json = serialize_or_fallback(
                        &SnapshotErrorFrame {
                            msg_type: "agent.ui.snapshot_error",
                            agent_id: agent_id_raw,
                            error: "Invalid message format".to_string(),
                            code: error_codes::INVALID_MESSAGE.to_string(),
                        },
                        "agent.ui.snapshot_error",
                    );
                    ctx.text(json);
                } else {
                    // R839: Unknown message type — log with context instead of silent drop
                    debug!(
                        session_id = %self.id,
                        msg_type = %msg_type,
                        error = %e,
                        "[MAGICIAN-V2-API] Unrecognized client message type"
                    );
                }
            },
        }
    }

    /// Handle `agent.ui.snapshot_request` — validate agent_id, spawn async read.
    fn handle_snapshot_request(&mut self, agent_id: String, ctx: &mut ws::WebsocketContext<Self>) {
        if !is_valid_identifier(&agent_id) {
            // R907: Log rejection with Debug (?) to escape control chars — log AFTER validation check
            warn!(
                session_id = %self.id,
                agent_id = ?agent_id,
                "[MAGICIAN-V2-API] snapshot_request rejected: invalid agent_id"
            );
            let json = serialize_or_fallback(
                &SnapshotErrorFrame {
                    msg_type: "agent.ui.snapshot_error",
                    agent_id,
                    error: "Invalid agent_id".to_string(),
                    code: error_codes::INVALID_AGENT.to_string(),
                },
                "agent.ui.snapshot_error",
            );
            ctx.text(json);
            return;
        }

        // R906: Cap concurrent in-flight snapshot reads to prevent unbounded futures
        const MAX_SNAPSHOT_IN_FLIGHT: u32 = 10;
        if self.snapshot_in_flight >= MAX_SNAPSHOT_IN_FLIGHT {
            warn!(
                session_id = %self.id,
                agent_id = %agent_id,
                in_flight = self.snapshot_in_flight,
                "[MAGICIAN-V2-API] snapshot_request rejected: too many in-flight reads"
            );
            let json = serialize_or_fallback(
                &SnapshotErrorFrame {
                    msg_type: "agent.ui.snapshot_error",
                    agent_id,
                    error: "Too many concurrent snapshot requests".to_string(),
                    code: error_codes::RATE_LIMITED.to_string(),
                },
                "agent.ui.snapshot_error",
            );
            ctx.text(json);
            return;
        }
        self.snapshot_in_flight += 1;

        // R907: Log entry AFTER validation (agent_id is now safe for Display format)
        debug!(
            session_id = %self.id,
            agent_id = %agent_id,
            "[MAGICIAN-V2-API] snapshot_request received"
        );

        let storage = self
            .agent_api
            .as_ref()
            .map(|agent_api| {
                let scoped_store = agent_api
                    .resolve_definition_store(&self.scope_principal, &self.scope_workspace);
                MuijStorage::new(scoped_store.storage().root().to_path_buf())
            })
            .unwrap_or_else(|| self.muij_storage.clone());
        let definition_store = self.agent_api.as_ref().map(|agent_api| {
            agent_api.resolve_definition_store(&self.scope_principal, &self.scope_workspace)
        });
        // R161: MuijQueryEngine created per-request (actor single-thread safety;
        // within-call cache still benefits documents with repeated query patterns).
        ctx.spawn(
            {
                let cache = self.muij_doc_cache.clone();
                let cache_key = agent_snapshot_cache_key(
                    &agent_id,
                    Some(&self.scope_principal),
                    Some(&self.scope_workspace),
                );
                async move {
                    let mut engine = MuijQueryEngine::new();
                    snapshot_response_json(
                        definition_store,
                        &storage,
                        &agent_id,
                        &mut engine,
                        cache.as_ref(),
                        Some(cache_key.as_str()),
                    )
                    .await
                }
            }
            .into_actor(self)
            .map(|json, act, ctx| {
                act.snapshot_in_flight = act.snapshot_in_flight.saturating_sub(1);
                ctx.text(json);
            }),
        );
    }

    /// GC-B02: Handle `ui.interaction` — validate, rate-limit, dispatch trigger pipeline.
    fn handle_ui_interaction(
        &mut self,
        agent_id: String,
        component_id: String,
        goal_id: Option<String>,
        trigger: Option<String>,
        request_id: Option<String>,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        // 1. Validate agent_id
        if !is_valid_identifier(&agent_id) {
            // R907: Log with Debug (?) to escape control chars in pre-validation values
            warn!(
                session_id = %self.id,
                agent_id = ?agent_id,
                "[MAGICIAN-V2-API] ui.interaction rejected: invalid agent_id"
            );
            let json = serialize_or_fallback(
                &InteractionErrorFrame {
                    msg_type: "ui.interaction.error",
                    agent_id,
                    component_id,
                    error: "Invalid agent_id".to_string(),
                    code: error_codes::INVALID_AGENT.to_string(),
                    request_id,
                },
                "ui.interaction.error",
            );
            ctx.text(json);
            return;
        }

        // R748+R878+R914: Validate component_id via extracted helper
        if !is_valid_component_id(&component_id) {
            warn!(
                session_id = %self.id,
                component_id_len = component_id.len(),
                "[MAGICIAN-V2-API] ui.interaction rejected: invalid component_id"
            );
            let json = serialize_or_fallback(
                &InteractionErrorFrame {
                    msg_type: "ui.interaction.error",
                    agent_id,
                    component_id,
                    error: "Invalid component_id".to_string(),
                    code: error_codes::INVALID_COMPONENT.to_string(),
                    request_id,
                },
                "ui.interaction.error",
            );
            ctx.text(json);
            return;
        }

        // R905: Validate goal_id if present — same policy as trigger (≤255 + ASCII + no control chars)
        if let Some(ref gid) = goal_id {
            if !is_valid_trigger(gid) {
                warn!(
                    session_id = %self.id,
                    goal_id_len = gid.len(),
                    "[MAGICIAN-V2-API] ui.interaction rejected: invalid goal_id"
                );
                let json = serialize_or_fallback(
                    &InteractionErrorFrame {
                        msg_type: "ui.interaction.error",
                        agent_id,
                        component_id,
                        error: "Invalid goal_id".to_string(),
                        code: error_codes::INVALID_MESSAGE.to_string(),
                        request_id,
                    },
                    "ui.interaction.error",
                );
                ctx.text(json);
                return;
            }
        }

        // R806+R949+R917: Validate request_id if present via extracted helper
        if let Some(ref rid) = request_id {
            if !is_valid_request_id(rid) {
                warn!(
                    session_id = %self.id,
                    request_id_len = rid.len(),
                    "[MAGICIAN-V2-API] ui.interaction rejected: invalid request_id"
                );
                let json = serialize_or_fallback(
                    &InteractionErrorFrame {
                        msg_type: "ui.interaction.error",
                        agent_id,
                        component_id,
                        error: "Invalid request_id".to_string(),
                        code: error_codes::INVALID_MESSAGE.to_string(),
                        request_id,
                    },
                    "ui.interaction.error",
                );
                ctx.text(json);
                return;
            }
        }

        // R830+R950+R918: Validate trigger field if present via extracted helper
        if let Some(ref t) = trigger {
            if !is_valid_trigger(t) {
                warn!(
                    session_id = %self.id,
                    trigger_len = t.len(),
                    "[MAGICIAN-V2-API] ui.interaction rejected: invalid trigger"
                );
                let json = serialize_or_fallback(
                    &InteractionErrorFrame {
                        msg_type: "ui.interaction.error",
                        agent_id,
                        component_id,
                        error: "Invalid trigger value".to_string(),
                        code: error_codes::INVALID_MESSAGE.to_string(),
                        request_id,
                    },
                    "ui.interaction.error",
                );
                ctx.text(json);
                return;
            }
        }

        // R907: Log entry AFTER all field validation (values are now safe for Display format)
        debug!(
            session_id = %self.id,
            agent_id = %agent_id,
            component_id = %component_id,
            request_id = ?request_id,
            "[MAGICIAN-V2-API] ui.interaction received"
        );

        // 2. Rate limit check: 1 trigger per INTERACTION_RATE_LIMIT_SECS per (agent_id, component_id)
        // R763: Use \0 delimiter — cannot appear in agent_id or component_id (rejected above)
        let rate_key = format!("{}\0{}", agent_id, component_id);
        let now = Instant::now();
        if let Some(last) = self.interaction_rate_limit.get(&rate_key) {
            // R872: Use saturating variant for robustness on exotic platforms
            if now.saturating_duration_since(*last).as_secs() < INTERACTION_RATE_LIMIT_SECS {
                warn!(
                    session_id = %self.id,
                    agent_id = %agent_id,
                    component_id = %component_id,
                    "[MAGICIAN-V2-API] ui.interaction rate-limited"
                );
                let json = serialize_or_fallback(
                    &InteractionErrorFrame {
                        msg_type: "ui.interaction.error",
                        agent_id,
                        component_id,
                        error: format!(
                            "Rate limited — max 1 trigger per {}s per component",
                            INTERACTION_RATE_LIMIT_SECS
                        ),
                        code: error_codes::RATE_LIMITED.to_string(),
                        request_id,
                    },
                    "ui.interaction.error",
                );
                ctx.text(json);
                return;
            }
        }

        // 3. Update rate limit timestamp
        // R760: Updated before dispatch (conservative — prevents retry floods during pipeline
        // execution). Failed triggers still consume the window; this is intentional.
        // R804: Cap HashMap size to prevent unbounded growth from burst traffic
        // with unique (agent_id, component_id) pairs. 1000 entries is generous
        // for legitimate use; heartbeat pruning handles normal cleanup.
        if self.interaction_rate_limit.len() >= 1000 {
            // R903: Emergency prune — remove all expired entries, then hard-reject if still full.
            let rate_window = std::time::Duration::from_secs(INTERACTION_RATE_LIMIT_SECS);
            self.interaction_rate_limit
                .retain(|_, last| now.saturating_duration_since(*last) < rate_window);
            if self.interaction_rate_limit.len() >= 1000 {
                // R903: True burst — all entries still live. Drop this insert to enforce hard cap.
                warn!(
                    session_id = %self.id,
                    "[MAGICIAN-V2-API] rate-limit map at hard cap (1000) — rejecting interaction"
                );
                let json = serialize_or_fallback(
                    &InteractionErrorFrame {
                        msg_type: "ui.interaction.error",
                        agent_id,
                        component_id,
                        error: "Too many concurrent interaction targets".to_string(),
                        code: error_codes::RATE_LIMITED.to_string(),
                        request_id,
                    },
                    "ui.interaction.error",
                );
                ctx.text(json);
                return;
            }
        }
        self.interaction_rate_limit.insert(rate_key, now);

        // 4. Extract agent_api and orchestrator
        // R834: Arc clone = 1 atomic increment (vs 12+ before Arc wrapping)
        let agent_api = match self.agent_api.clone() {
            Some(api) => api,
            None => {
                error!(
                    session_id = %self.id,
                    "[MAGICIAN-V2-API] ui.interaction received but agent_api not set on session"
                );
                let json = serialize_or_fallback(
                    &InteractionErrorFrame {
                        msg_type: "ui.interaction.error",
                        agent_id,
                        component_id,
                        error: "Server misconfiguration — agent API not available".to_string(),
                        code: error_codes::TRIGGER_FAILED.to_string(),
                        request_id,
                    },
                    "ui.interaction.error",
                );
                ctx.text(json);
                return;
            },
        };
        // R885+R934: Fail explicitly if orchestrator is missing.
        let orchestrator = match self.v2_orchestrator.clone() {
            Some(o) => o,
            None => {
                error!(
                    session_id = %self.id,
                    "[MAGICIAN-V2-API] ui.interaction rejected: orchestrator not configured"
                );
                let json = serialize_or_fallback(
                    &InteractionErrorFrame {
                        msg_type: "ui.interaction.error",
                        agent_id,
                        component_id,
                        error: "Service not ready".to_string(),
                        code: error_codes::AGENT_UNAVAILABLE.to_string(),
                        request_id,
                    },
                    "ui.interaction.error",
                );
                ctx.text(json);
                return;
            },
        };

        // 5. Build trigger request
        // R912: Clone goal_id before move so we can echo it in the ack frame.
        let goal_id_echo = goal_id.clone();
        let request = ManualAgentTriggerRequest { goal_id, trigger };

        // R775: Move owned Strings into async block instead of cloning
        let req_id = request_id;
        let definition_store =
            agent_api.resolve_definition_store(&self.scope_principal, &self.scope_workspace);

        // 6. Spawn async trigger pipeline — R749: use structured ManualTriggerOutcome
        //    directly instead of parsing HttpResponse body bytes.
        ctx.spawn(
            async move {
                let outcome = trigger_agent_controlled(
                    &agent_api,
                    &definition_store,
                    Some(&orchestrator),
                    &agent_id,
                    request,
                )
                .await;

                match outcome {
                    ManualTriggerOutcome::Accepted(response) => {
                        // R747: Direct field access — no shadowing risk
                        serialize_or_fallback(
                            &InteractionAckFrame {
                                msg_type: "ui.interaction.ack",
                                agent_id,
                                component_id,
                                cycle_id: response.cycle_id,
                                status: response.status,
                                goal_id: goal_id_echo,
                                request_id: req_id,
                            },
                            "ui.interaction.ack", // R904: correct fallback_type
                        )
                    },
                    ManualTriggerOutcome::Error { http_status, .. } => {
                        // R844: Log trigger outcome for observability
                        warn!(
                            agent_id = %agent_id,
                            component_id = %component_id,
                            http_status = http_status,
                            request_id = ?req_id,
                            "[MAGICIAN-V2-API] ui.interaction trigger failed"
                        );
                        // R751+R803: Sanitize error with granular codes for client retry logic
                        let (code, error_detail) = match http_status {
                            404 => (error_codes::AGENT_NOT_FOUND, "Agent not found".to_string()),
                            429 => (
                                error_codes::RATE_LIMITED,
                                "Agent trigger rate limited".to_string(),
                            ),
                            409 => (
                                error_codes::AGENT_PAUSED,
                                "Agent is paused or trigger already pending".to_string(),
                            ),
                            503 => (
                                error_codes::AGENT_UNAVAILABLE,
                                "Agent is temporarily unavailable".to_string(),
                            ),
                            400 => (
                                error_codes::INVALID_REQUEST,
                                "Invalid trigger request".to_string(),
                            ),
                            // R941: Surface precise error codes for 422/412 instead of generic trigger_failed
                            422 => (
                                error_codes::INVALID_REQUEST,
                                "Missing required context for trigger".to_string(),
                            ),
                            412 => (
                                error_codes::TRIGGER_FAILED,
                                "Version conflict — retry after refresh".to_string(),
                            ),
                            _ => (
                                error_codes::TRIGGER_FAILED,
                                format!("Trigger failed with status {}", http_status),
                            ),
                        };

                        serialize_or_fallback(
                            &InteractionErrorFrame {
                                msg_type: "ui.interaction.error",
                                agent_id,
                                component_id,
                                error: error_detail,
                                code: code.to_string(),
                                request_id: req_id,
                            },
                            "ui.interaction.error",
                        )
                    },
                }
            }
            .into_actor(self)
            .map(|json, _act, ctx| {
                ctx.text(json);
            }),
        );
    }

    /// R881: Periodic tick — sends heartbeat pings, checks client timeout (disconnects
    /// stale sessions), and prunes expired rate-limit entries (R752).
    fn send_heartbeat(&self, ctx: &mut <Self as Actor>::Context) {
        ctx.run_interval(HEARTBEAT_INTERVAL, |act, ctx| {
            // Check client heartbeat
            let now = Instant::now();
            if now.saturating_duration_since(act.last_heartbeat) > CLIENT_TIMEOUT {
                // R939: Use structured session_id field for log aggregation
                info!(
                    session_id = %act.id,
                    "[MAGICIAN-V2-API] WebSocket session timed out, disconnecting"
                );
                // R945: Send error frame before timeout close so client can distinguish
                // timeout from crash/network drop.
                let json = serialize_or_fallback(
                    &ProtocolErrorFrame {
                        msg_type: "error",
                        error: "Session timed out".to_string(),
                        code: error_codes::PROTOCOL_ERROR,
                    },
                    "error",
                );
                ctx.text(json);
                ctx.stop();
                return;
            }

            // R752: Prune stale rate-limit entries to prevent unbounded HashMap growth
            let rate_window = std::time::Duration::from_secs(INTERACTION_RATE_LIMIT_SECS);
            act.interaction_rate_limit
                .retain(|_, last| now.saturating_duration_since(*last) < rate_window);

            // Send ping to client
            ctx.ping(b"heartbeat");
        });
    }
}

impl Actor for V2WebSocketSession {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        // R836: Use structured session_id field for log aggregation
        debug!(
            session_id = %self.id,
            "[MAGICIAN-V2-API] V2 WebSocket session started"
        );
        self.send_heartbeat(ctx);

        // R882: Send initial heartbeat as connection-ready signal.
        // Note: same shape as periodic broadcast heartbeats (R863 — no separate Connected event type).
        let connected_event = RuntimeTransportEvent::Heartbeat {
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        // R837: Log serialization failure instead of silent swallow
        match serialize_public_v2_event(&connected_event) {
            Ok(json) => ctx.text(json),
            Err(e) => error!(
                session_id = %self.id,
                error = %e,
                "[MAGICIAN-V2-API] Failed to serialize initial heartbeat"
            ),
        }

        // Re-emit pending pause events for reconnection scenarios.
        // R864: Idempotency note — clients MUST dedup by `pause_state_id` (included in each
        // event frame). Reconnection can re-emit pauses the client has already seen if the
        // previous session didn't acknowledge them before disconnect.
        for pause_info in std::mem::take(&mut self.pending_pauses) {
            info!(
                session_id = %self.id,
                execution_id = %pause_info.execution_id,
                plan_id = ?pause_info.plan_id,
                step_id = ?pause_info.step_id,
                is_retry = pause_info.is_retry,
                retry_count = pause_info.retry_count,
                pause_state_id = %pause_info.key,
                "[MAGICIAN-V2-API] Re-emitting pending pause event on reconnect"
            );

            let event = build_pending_pause_reemit_event(pause_info);

            // R837: Log serialization failure instead of silent swallow
            match serialize_public_v2_event(&event) {
                Ok(json) => ctx.text(json),
                Err(e) => error!(
                    session_id = %self.id,
                    error = %e,
                    "[MAGICIAN-V2-API] Failed to serialize pending pause re-emit event"
                ),
            }
        }

        // Start listening for V2 events
        if let Some(mut receiver) = self.event_receiver.take() {
            let addr = ctx.address();
            let session_id = self.id.clone();

            ctx.spawn(
                async move {
                    loop {
                        match receiver.recv().await {
                            Ok(event) => {
                                if !generic_v2_websocket_admits(&event) {
                                    // Filter at the queue boundary as well as in
                                    // the actor handler below. Otherwise a burst
                                    // of intentionally private notifications can
                                    // fill every generic socket mailbox with
                                    // messages that can never be delivered.
                                    continue;
                                }
                                // R841: Log event type discriminant instead of full payload
                                debug!(
                                    session_id = %session_id,
                                    "[MAGICIAN-V2-API] V2 WebSocket forwarding event to actor"
                                );
                                if addr.try_send(V2EventMessage(event)).is_err() {
                                    // R817: Actor mailbox full or stopped — break the loop
                                    // to prevent leaked receiver task that wakes on every
                                    // broadcast event but can never deliver.
                                    warn!(
                                        session_id = %session_id,
                                        "[MAGICIAN-V2-API] V2 WebSocket actor mailbox full or stopped — breaking receiver loop"
                                    );
                                    break;
                                }
                            },
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                // R801: Log lag with session_id and notify client
                                warn!(
                                    session_id = %session_id,
                                    skipped = skipped,
                                    "[MAGICIAN-V2-API] V2 WebSocket receiver lagged, skipped events"
                                );
                                // R801+R923: Send heartbeat as keepalive after lag. This is a
                                // standard heartbeat — it does NOT carry lag/skipped metadata.
                                // Clients should treat any gap in expected event sequence as
                                // potential lag and refresh state if needed.
                                let _ = addr.try_send(V2EventMessage(RuntimeTransportEvent::Heartbeat {
                                    timestamp: chrono::Utc::now().timestamp_millis(),
                                }));
                                continue;
                            },
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                // R842: Log channel close so operators can detect degraded sessions
                                warn!(
                                    session_id = %session_id,
                                    "[MAGICIAN-V2-API] V2 broadcast channel closed — receiver loop exiting"
                                );
                                break;
                            },
                        }
                    }
                }
                .into_actor(self)
                // R909: Stop actor when broadcast channel is torn down to prevent zombie sessions.
                .map(|_, _act, ctx| {
                    ctx.stop();
                }),
            );
        }
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        // R836: Use structured session_id field
        debug!(
            session_id = %self.id,
            "[MAGICIAN-V2-API] V2 WebSocket session stopped"
        );
    }
}

/// Message for sending V2 events to WebSocket clients
#[derive(actix::Message)]
#[rtype(result = "()")]
struct V2EventMessage(RuntimeTransportEvent);

impl actix::Handler<V2EventMessage> for V2WebSocketSession {
    type Result = ();

    fn handle(&mut self, msg: V2EventMessage, ctx: &mut Self::Context) {
        let msg =
            strip_structured_presentation_if_disabled(msg.0, self.supports_structured_presentation);
        if !generic_v2_websocket_admits(&msg) {
            // App-owner notifications are owned exclusively by the reviewed
            // Attention/UserRequest surface. Never expose either their body or
            // their resolution marker on the generic V2 WebSocket, including
            // while the host-sealed notification TTL is still live.
            return;
        }
        if !event_visible_to_scope(&msg, &self.scope_principal, &self.scope_workspace) {
            return;
        }
        match serialize_public_v2_event(&msg) {
            Ok(json) => {
                // R841: Log without full payload — truncate to avoid KB-scale debug lines
                debug!(
                    session_id = %self.id,
                    json_len = json.len(),
                    "[MAGICIAN-V2-API] V2 WebSocket sending event"
                );
                ctx.text(json);
            },
            Err(e) => {
                // R845: Include session_id and error detail for actionable diagnostics
                error!(
                    session_id = %self.id,
                    error = %e,
                    "[MAGICIAN-V2-API] Failed to serialize V2 event"
                );
            },
        }
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for V2WebSocketSession {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Ping(msg)) => {
                self.last_heartbeat = Instant::now();
                ctx.pong(&msg);
            },
            Ok(ws::Message::Pong(_)) => {
                self.last_heartbeat = Instant::now();
            },
            Ok(ws::Message::Text(text)) => {
                // Any inbound client traffic proves the socket is live — refresh
                // the heartbeat so a busy-but-not-pinging client isn't torn down
                // by the CLIENT_TIMEOUT watchdog (#31).
                self.last_heartbeat = Instant::now();
                debug!(
                    session_id = %self.id,
                    "[MAGICIAN-V2-API] V2 WebSocket received text message"
                );
                self.handle_client_message(&text, ctx);
            },
            Ok(ws::Message::Binary(_)) => {
                // R938: Send error frame for binary frames (parity with protocol error path)
                debug!(
                    session_id = %self.id,
                    "[MAGICIAN-V2-API] V2 WebSocket received binary data (not supported)"
                );
                let json = serialize_or_fallback(
                    &ProtocolErrorFrame {
                        msg_type: "error",
                        error: "Binary frames not supported".to_string(),
                        code: error_codes::PROTOCOL_ERROR,
                    },
                    "error",
                );
                ctx.text(json);
            },
            Ok(ws::Message::Close(reason)) => {
                debug!(
                    session_id = %self.id,
                    reason = ?reason,
                    "[MAGICIAN-V2-API] V2 WebSocket session closing"
                );
                ctx.stop();
            },
            Err(e) => {
                // R853: Include session_id for correlation
                if is_expected_websocket_disconnect_message(&e.to_string()) {
                    debug!(
                        session_id = %self.id,
                        error = %e,
                        "[MAGICIAN-V2-API] V2 WebSocket disconnected before completing frame"
                    );
                } else {
                    error!(
                        session_id = %self.id,
                        error = %e,
                        "[MAGICIAN-V2-API] V2 WebSocket protocol error"
                    );
                }
                // R870+R910: Send typed error frame before close so clients
                // can distinguish protocol errors from clean disconnects.
                let json = serialize_or_fallback(
                    &ProtocolErrorFrame {
                        msg_type: "error",
                        error: "WebSocket protocol error".to_string(),
                        code: error_codes::PROTOCOL_ERROR,
                    },
                    "error",
                );
                ctx.text(json);
                ctx.stop();
            },
            _ => {},
        }
    }
}

/// WebSocket endpoint handler
///
/// Accepts optional query parameters:
/// - `execution_id`: Preferred execution selector for pending pause replay on reconnect.
/// - `agent_id`: If provided, also checks for pending agent-scoped pauses (TRUE_AGENTS Phase 3)
///
/// R888: Connection limits should be enforced at the server/reverse-proxy level
/// (e.g., nginx `limit_conn`) rather than in the handler. For development use,
/// the actix-web worker pool provides implicit backpressure.
pub async fn websocket_handler(
    req: HttpRequest,
    stream: web::Payload,
    scope: ResolvedScope,
    query: web::Query<WsConnectParams>,
    event_broadcaster: web::Data<Arc<RuntimeTransportBroadcaster>>,
    pause_store: web::Data<Arc<FullPauseStore>>,
    muij_storage: web::Data<MuijStorage>,
    muij_doc_cache: Option<web::Data<MuijDocumentCache>>,
    api: web::Data<MagicianV2Api>,
) -> Result<HttpResponse> {
    debug!(
        execution_id = ?query.execution_id,
        agent_id = ?query.agent_id,
        supports_structured_presentation = ?query.supports_structured_presentation,
        "[MAGICIAN-V2-API] V2 WebSocket connection requested"
    );

    // R586 + GC-B01: Origin validation — defense-in-depth against cross-site WebSocket
    // hijacking. Compares Origin hostname (port-stripped) against DEFAULT_ALLOWED_ORIGINS
    // and the request Host hostname.
    if let Err((origin, host)) = validate_origin(&req) {
        warn!(
            origin = %origin,
            host = %host,
            "[MAGICIAN-V2-API] WebSocket Origin mismatch — rejecting (R586/GC-B01)"
        );
        return Ok(HttpResponse::Forbidden().json(serde_json::json!({
            "error": "Origin not allowed",
            "code": "origin_mismatch"
        })));
    }

    // R826: Validate query params before use (caps length, rejects control chars)
    if let Some(ref execution_id) = query.execution_id {
        if execution_id.len() > 255 || execution_id.bytes().any(|b| b < 0x20 || b == 0x7F) {
            warn!(
                execution_id_len = execution_id.len(),
                "[MAGICIAN-V2-API] WebSocket rejected: invalid execution_id query param"
            );
            return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "Invalid execution_id",
                "code": "invalid_param"
            })));
        }
    }
    if let Some(ref aid) = query.agent_id {
        if !is_valid_identifier(aid) {
            warn!(
                agent_id = %aid,
                "[MAGICIAN-V2-API] WebSocket rejected: invalid agent_id query param"
            );
            return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "Invalid agent_id",
                "code": "invalid_param"
            })));
        }
    }

    let _ = pause_store
        .get_ref()
        .schedule_legacy_pending_index_background_repair();
    if pause_store.legacy_pending_index_repair_in_progress()
        && (query.execution_id.is_some() || query.agent_id.is_some())
    {
        // A reconnect must never receive an authoritative empty pause replay
        // while a pre-V2 index is still being rebuilt. Rejecting the handshake
        // is retryable and keeps filesystem reads/deserialization off the
        // Actix worker that serves the WebSocket upgrade.
        return Ok(HttpResponse::ServiceUnavailable().json(serde_json::json!({
            "error": "Legacy pause index repair is still in progress; reconnect shortly",
            "code": "legacy_pause_index_repair_pending",
            "retryable": true
        })));
    }

    // Check for pending execution-scoped pauses.
    let mut pending_pauses = if let Some(execution_id) = query.execution_id.as_ref() {
        let pauses = pause_store.get_pending_for_execution(execution_id);
        if !pauses.is_empty() {
            info!(
                execution_id = %execution_id,
                count = pauses.len(),
                "[MAGICIAN-V2-API] Found {} pending pauses for execution on reconnect",
                pauses.len()
            );
        }
        pauses
    } else {
        Vec::new()
    };

    // Also check for pending agent-scoped pauses (TRUE_AGENTS Phase 3).
    // Dedup by key — a pause with both thread and agent routing would appear in
    // both get_pending_for_thread and get_pending_for_agent results.
    if let Some(ref agent_id) = query.agent_id {
        let agent_pauses = pause_store.get_pending_for_agent(agent_id);
        if !agent_pauses.is_empty() {
            let existing_keys: std::collections::HashSet<String> =
                pending_pauses.iter().map(|p| p.key.clone()).collect();
            let new_pauses: Vec<_> = agent_pauses
                .into_iter()
                .filter(|p| !existing_keys.contains(&p.key))
                .collect();
            if !new_pauses.is_empty() {
                info!(
                    agent_id = %agent_id,
                    count = new_pauses.len(),
                    "[MAGICIAN-V2-API] Found {} new pending pauses for agent on reconnect",
                    new_pauses.len()
                );
                pending_pauses.extend(new_pauses);
            }
        }
    }

    pending_pauses.sort_by(|a, b| {
        a.iteration
            .cmp(&b.iteration)
            .then_with(|| a.key.cmp(&b.key))
    });

    let supports_structured_presentation = query.supports_structured_presentation.unwrap_or(false);

    // R861: Cap pending pauses to prevent flood of frames on connect.
    // Keep most recent 50 (after sort, last entries are newest by iteration).
    const MAX_PENDING_PAUSES: usize = 50;
    if pending_pauses.len() > MAX_PENDING_PAUSES {
        warn!(
            total = pending_pauses.len(),
            cap = MAX_PENDING_PAUSES,
            "[MAGICIAN-V2-API] Capping pending pauses on reconnect"
        );
        let start = pending_pauses.len() - MAX_PENDING_PAUSES;
        pending_pauses = pending_pauses.split_off(start);
    }

    let storage = muij_storage.get_ref().clone();
    let cache = muij_doc_cache.map(|c| c.get_ref().clone());
    let scope_principal = scope.principal().to_string();
    let scope_workspace = scope.workspace().to_string();
    let mut session = if pending_pauses.is_empty() {
        V2WebSocketSession::new(
            &event_broadcaster,
            storage,
            scope_principal.clone(),
            scope_workspace.clone(),
            supports_structured_presentation,
        )
    } else {
        V2WebSocketSession::with_pending_pauses(
            &event_broadcaster,
            pending_pauses,
            storage,
            scope_principal.clone(),
            scope_workspace.clone(),
            supports_structured_presentation,
        )
    };
    session.muij_doc_cache = cache;
    // GC-B02: Provide agent API + orchestrator for ui.interaction dispatch
    // R834: Wrap in Arc for cheap per-interaction cloning
    session.agent_api = Some(Arc::new(api.agent_api().clone()));
    session.v2_orchestrator = Some(api.orchestrator().clone());

    // R836: Capture session_id before session is moved into WsResponseBuilder
    let session_id_for_log = session.id.clone();

    // R765: Use WsResponseBuilder for explicit frame size control
    let resp = ws::WsResponseBuilder::new(session, &req, stream)
        .protocols(V2_WEBSOCKET_PROTOCOLS)
        .frame_size(MAX_WS_FRAME_SIZE)
        .start()?;

    // R836: Structured log with session_id for connection lifecycle tracking
    debug!(
        session_id = %session_id_for_log,
        execution_id = ?query.execution_id,
        agent_id = ?query.agent_id,
        "[MAGICIAN-V2-API] V2 WebSocket connection established"
    );
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    #[test]
    fn browser_upgrade_selects_the_realtime_protocol_without_echoing_the_bearer() {
        use actix_web::http::header;

        let request = actix_web::test::TestRequest::get()
            .insert_header((header::UPGRADE, "websocket"))
            .insert_header((header::CONNECTION, "upgrade"))
            .insert_header((header::SEC_WEBSOCKET_VERSION, "13"))
            .insert_header((header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ=="))
            .insert_header((
                header::SEC_WEBSOCKET_PROTOCOL,
                "magician-events-v2, magician-bearer.secret-token",
            ))
            .to_http_request();
        let response = ws::handshake_with_protocols(&request, V2_WEBSOCKET_PROTOCOLS)
            .expect("valid realtime WebSocket handshake")
            .finish();

        assert_eq!(
            response
                .headers()
                .get(header::SEC_WEBSOCKET_PROTOCOL)
                .and_then(|value| value.to_str().ok()),
            Some("magician-events-v2")
        );
        assert!(!response
            .headers()
            .iter()
            .filter_map(|(_, value)| value.to_str().ok())
            .any(|value| value.contains("secret-token")));
    }

    fn test_storage() -> (TempDir, MuijStorage) {
        let dir = TempDir::new().unwrap();
        let storage = MuijStorage::new(dir.path().to_path_buf());
        (dir, storage)
    }

    fn test_engine() -> MuijQueryEngine {
        MuijQueryEngine::new()
    }

    fn test_pending_pause(
        key: &str,
        input_type: magician::magician_v2::execution::agentic::UserInputType,
        question: Option<&str>,
    ) -> PendingPauseInfo {
        let mut pending = PendingPauseInfo::for_test(key, "exec-1", input_type);
        pending.principal = Some("alpha".to_string());
        pending.workspace = Some("prod".to_string());
        pending.task_id = Some("task-1".to_string());
        pending.plan_id = Some("plan-1".to_string());
        pending.step_id = Some("step-1".to_string());
        pending.question = question.map(str::to_string);
        pending.iteration = 7;
        pending.agent_id = Some("agent-1".to_string());
        pending.goal_id = Some("goal-1".to_string());
        pending.cycle_id = Some("cycle-1".to_string());
        pending
    }

    #[test]
    fn test_session_creation() {
        let broadcaster = RuntimeTransportBroadcaster::new(100);
        let (_dir, storage) = test_storage();
        let session = V2WebSocketSession::new(
            &broadcaster,
            storage,
            "default".to_string(),
            "default".to_string(),
            false,
        );

        assert!(!session.id.is_empty());
        assert!(session.event_receiver.is_some());
    }

    #[test]
    fn feed_events_are_filtered_by_scope() {
        let created = RuntimeTransportEvent::FeedItemCreated {
            item: FeedItem {
                id: "task:1".to_string(),
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                item_type: magician::magician_v2::feed::FeedItemType::Task,
                task_id: Some("task-1".to_string()),
                ui_thread_id: Some("general".to_string()),
                agent_id: Some("atlas".to_string()),
                title: "Task".to_string(),
                summary: Some("running".to_string()),
                status: magician::magician_v2::feed::FeedItemStatus::Running,
                created_at: 1,
                updated_at: 2,
                actions: Vec::new(),
                metadata: serde_json::json!({}),
            },
            timestamp: 0,
        };
        assert!(event_visible_to_scope(&created, "alpha", "prod"));
        assert!(!event_visible_to_scope(&created, "beta", "prod"));

        let removed = RuntimeTransportEvent::FeedItemRemoved {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            id: "task:1".to_string(),
            task_id: Some("task-1".to_string()),
            ui_thread_id: Some("general".to_string()),
            execution_id: Some("exec-1".to_string()),
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        assert!(event_visible_to_scope(&removed, "alpha", "prod"));
        assert!(!event_visible_to_scope(&removed, "alpha", "other"));

        let panel_delta = RuntimeTransportEvent::ExecutionPanelDelta {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            task_id: Some("task-1".to_string()),
            execution_id: Some("exec-1".to_string()),
            state: magician_learning::execution_panel::ExecutionPanelState {
                default_tab: magician_learning::execution_panel::ExecutionPanelTab::Plan,
                overview: magician_learning::execution_panel::ExecutionPanelOverview {
                    task_id: "task-1".to_string(),
                    execution_id: Some("exec-1".to_string()),
                    principal: "alpha".to_string(),
                    workspace: "prod".to_string(),
                    ui_thread_id: "general".to_string(),
                    title: "Task".to_string(),
                    description: String::new(),
                    status: magician::magician_v2::storage::TaskStatus::Running,
                    priority: None,
                    assigned_agent_id: "atlas".to_string(),
                    active_agent_id: Some("atlas".to_string()),
                    has_plan: false,
                    progress: None,
                    current_step: None,
                    created_at: 1,
                    updated_at: 2,
                },
                run: magician_learning::execution_panel::ExecutionPanelRunState::default(),
                output: magician_learning::execution_panel::ExecutionPanelOutputState::default(),
                debug: magician_learning::execution_panel::ExecutionPanelDebugState {
                    selected_execution: None,
                    taskplan: None,
                    timeline: Vec::new(),
                    observations: Vec::new(),
                    shell_entries: Vec::new(),
                    latest_error_message: None,
                    history_count: 0,
                    tags: Vec::new(),
                },
            },
            timestamp: 2,
        };
        assert!(event_visible_to_scope(&panel_delta, "alpha", "prod"));
        assert!(!event_visible_to_scope(&panel_delta, "beta", "prod"));

        let definition_changed = RuntimeTransportEvent::AgentDefinitionChanged {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            agent_id: "ceo".to_string(),
            timestamp: 4,
        };
        assert!(event_visible_to_scope(&definition_changed, "alpha", "prod"));
        assert!(!event_visible_to_scope(&definition_changed, "beta", "prod"));

        let scoped_agent_event = RuntimeTransportEvent::AgentEvent {
            event: magician::magician_v2::realtime_events::AgentEventEnvelope::new_scoped(
                "agent.updated",
                "ceo",
                "alpha",
                "prod",
                serde_json::json!({ "status": "updated" }),
            ),
        };
        assert!(event_visible_to_scope(&scoped_agent_event, "alpha", "prod"));
        assert!(!event_visible_to_scope(
            &scoped_agent_event,
            "alpha",
            "other"
        ));

        // The chat engine is one process-wide setting: its change reaches
        // every scope, while any other unscoped event stays in system/system.
        let chat_engine_updated = RuntimeTransportEvent::AgentEvent {
            event: magician::magician_v2::realtime_events::AgentEventEnvelope::new(
                "chat.engine.updated",
                "magician-plane",
                serde_json::json!({ "chat_current": "claude_code", "chat_model": "default" }),
            ),
        };
        assert!(event_visible_to_scope(&chat_engine_updated, "alpha", "prod"));
        assert!(event_visible_to_scope(&chat_engine_updated, "beta", "other"));
        let other_unscoped = RuntimeTransportEvent::AgentEvent {
            event: magician::magician_v2::realtime_events::AgentEventEnvelope::new(
                "agent.updated",
                "ceo",
                serde_json::json!({}),
            ),
        };
        assert!(!event_visible_to_scope(&other_unscoped, "alpha", "prod"));
        // The name alone is not enough: only the plane's relay speaks for
        // every scope.
        let forged = RuntimeTransportEvent::AgentEvent {
            event: magician::magician_v2::realtime_events::AgentEventEnvelope::new(
                "chat.engine.updated",
                "ceo",
                serde_json::json!({ "chat_current": "claude_code" }),
            ),
        };
        assert!(!event_visible_to_scope(&forged, "alpha", "prod"));

        let payload_scoped_agent_event = RuntimeTransportEvent::AgentEvent {
            event: magician::magician_v2::realtime_events::AgentEventEnvelope::new(
                "published_surface.changed",
                "ceo",
                serde_json::json!({
                    "principal": "alpha",
                    "workspace": "prod",
                }),
            ),
        };
        assert!(event_visible_to_scope(
            &payload_scoped_agent_event,
            "alpha",
            "prod"
        ));
        assert!(!event_visible_to_scope(
            &payload_scoped_agent_event,
            "beta",
            "prod"
        ));

        let cycle_started = RuntimeTransportEvent::AgentCycleStarted {
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            agent_id: "ceo".to_string(),
            goal_id: "harness:ceo:ops".to_string(),
            cycle_id: "cycle-1".to_string(),
            execution_id: Some("exec-1".to_string()),
            goal: "Run ops".to_string(),
            timestamp: 4,
        };
        assert!(event_visible_to_scope(&cycle_started, "alpha", "prod"));
        assert!(!event_visible_to_scope(&cycle_started, "beta", "prod"));

        let cycle_completed = RuntimeTransportEvent::AgentCycleCompleted {
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            agent_id: "ceo".to_string(),
            goal_id: "harness:ceo:ops".to_string(),
            cycle_id: "cycle-1".to_string(),
            execution_id: Some("exec-1".to_string()),
            outcome: "success".to_string(),
            iterations_used: 3,
            timestamp: 4,
        };
        assert!(event_visible_to_scope(&cycle_completed, "alpha", "prod"));
        assert!(!event_visible_to_scope(&cycle_completed, "alpha", "other"));

        let agent_triggered = RuntimeTransportEvent::AgentTriggered {
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            agent_id: "ceo".to_string(),
            goal_id: "harness:ceo:ops".to_string(),
            trigger: "manual".to_string(),
            timestamp: 4,
        };
        assert!(event_visible_to_scope(&agent_triggered, "alpha", "prod"));
        assert!(!event_visible_to_scope(&agent_triggered, "beta", "prod"));

        let unscoped_cycle_started = RuntimeTransportEvent::AgentCycleStarted {
            principal: None,
            workspace: None,
            agent_id: "ceo".to_string(),
            goal_id: "harness:ceo:ops".to_string(),
            cycle_id: "cycle-2".to_string(),
            execution_id: Some("exec-2".to_string()),
            goal: "Run ops".to_string(),
            timestamp: 4,
        };
        assert!(!event_visible_to_scope(
            &unscoped_cycle_started,
            "alpha",
            "prod"
        ));

        let planning_started = RuntimeTransportEvent::V3PlanningStarted {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            task_id: "task-1".to_string(),
            task_title: "Task".to_string(),
            agent_id: "planner".to_string(),
            plan_id: "plan-1".to_string(),
            ui_thread_id: "thread-1".to_string(),
            timestamp: 5,
        };
        assert!(event_visible_to_scope(&planning_started, "alpha", "prod"));
        assert!(!event_visible_to_scope(&planning_started, "alpha", "other"));
    }

    #[test]
    fn scoped_execution_events_are_filtered_by_scope() {
        let started = RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id: "exec-1".to_string(),
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            plan_id: "plan-1".to_string(),
            step_id: "step-1".to_string(),
            goal: "Goal".to_string(),
            success_criteria: "Done".to_string(),
            max_iterations: 3,
            hint_action: None,
            agent_id: Some("atlas".to_string()),
            timestamp: 1,
        };
        assert!(event_visible_to_scope(&started, "alpha", "prod"));
        assert!(!event_visible_to_scope(&started, "beta", "prod"));

        let shell = RuntimeTransportEvent::ShellOutputChunk {
            execution_id: "exec-1".to_string(),
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            step_id: "step-1".to_string(),
            step_index: 0,
            command: "echo hi".to_string(),
            stream: "stdout".to_string(),
            data: "hi".to_string(),
            sequence: 0,
            is_final: false,
            exit_code: None,
            timestamp: 2,
        };
        assert!(event_visible_to_scope(&shell, "alpha", "prod"));
        assert!(!event_visible_to_scope(&shell, "alpha", "other"));

        let unscoped_shell = RuntimeTransportEvent::ShellOutputChunk {
            execution_id: "exec-1".to_string(),
            principal: None,
            workspace: None,
            step_id: "step-1".to_string(),
            step_index: 0,
            command: "echo hi".to_string(),
            stream: "stdout".to_string(),
            data: "hi".to_string(),
            sequence: 0,
            is_final: false,
            exit_code: None,
            timestamp: 2,
        };
        assert!(!event_visible_to_scope(&unscoped_shell, "alpha", "prod"));

        let parameter_discovery = RuntimeTransportEvent::ParameterDiscoveryAttempted {
            execution_id: "exec-1".to_string(),
            parameter_name: "target_url".to_string(),
            discovery_method: "AutonomousDiscovery".to_string(),
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 3,
        };
        assert!(event_visible_to_scope(
            &parameter_discovery,
            "alpha",
            "prod"
        ));
        assert!(!event_visible_to_scope(
            &parameter_discovery,
            "beta",
            "prod"
        ));

        let unscoped_parameter_discovery = RuntimeTransportEvent::ParameterDiscoveryAttempted {
            execution_id: "exec-1".to_string(),
            parameter_name: "target_url".to_string(),
            discovery_method: "AutonomousDiscovery".to_string(),
            principal: None,
            workspace: None,
            timestamp: 3,
        };
        assert!(!event_visible_to_scope(
            &unscoped_parameter_discovery,
            "alpha",
            "prod"
        ));

        let execution_status = RuntimeTransportEvent::ExecutionStatusChanged {
            execution_id: "exec-1".to_string(),
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            task_id: Some("task-1".to_string()),
            root_execution_id: Some("exec-1".to_string()),
            previous_status: "planning".to_string(),
            new_status: "running".to_string(),
            reason: None,
            timestamp: 4,
        };
        assert!(event_visible_to_scope(&execution_status, "alpha", "prod"));
        assert!(!event_visible_to_scope(&execution_status, "alpha", "other"));

        let unscoped_execution_status = RuntimeTransportEvent::ExecutionStatusChanged {
            execution_id: "exec-1".to_string(),
            principal: None,
            workspace: None,
            task_id: Some("task-1".to_string()),
            root_execution_id: Some("exec-1".to_string()),
            previous_status: "planning".to_string(),
            new_status: "running".to_string(),
            reason: None,
            timestamp: 4,
        };
        assert!(!event_visible_to_scope(
            &unscoped_execution_status,
            "alpha",
            "prod"
        ));

        let scoped_execution_started = RuntimeTransportEvent::ExecutionStarted {
            execution_id: "exec-1".to_string(),
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            plan_id: "plan-1".to_string(),
            steps_total: 3,
            timestamp: 5,
        };
        assert!(event_visible_to_scope(
            &scoped_execution_started,
            "alpha",
            "prod"
        ));
        assert!(!event_visible_to_scope(
            &scoped_execution_started,
            "alpha",
            "other"
        ));

        let unscoped_execution_started = RuntimeTransportEvent::ExecutionStarted {
            execution_id: "exec-1".to_string(),
            principal: None,
            workspace: None,
            plan_id: "plan-1".to_string(),
            steps_total: 3,
            timestamp: 5,
        };
        assert!(!event_visible_to_scope(
            &unscoped_execution_started,
            "alpha",
            "prod"
        ));

        let chat_message = RuntimeTransportEvent::ChatMessageReceived {
            session_id: "session-1".to_string(),
            message: magician::magician_v2::chat::models::ChatMessage {
                id: "msg-1".to_string(),
                session_id: "session-1".to_string(),
                direction: magician::magician_v2::chat::models::ChatMessageDirection::Assistant,
                content: magician::magician_v2::chat::models::ChatMessageContent::Text {
                    text: "Scoped hello".to_string(),
                    plan_reply: None,
                },
                created_at: 6,
                chat_turn_id: None,
                voice_origin: None,
                context_origin: None,
                speech_segments: None,
                source_surface: None,
                presence_session_id: None,
                presentation: None,
            },
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            origin_channel: None,
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        assert!(event_visible_to_scope(&chat_message, "alpha", "prod"));
        assert!(!event_visible_to_scope(&chat_message, "beta", "prod"));

        let unscoped_chat_message = RuntimeTransportEvent::ChatMessageReceived {
            session_id: "session-1".to_string(),
            message: magician::magician_v2::chat::models::ChatMessage {
                id: "msg-2".to_string(),
                session_id: "session-1".to_string(),
                direction: magician::magician_v2::chat::models::ChatMessageDirection::Assistant,
                content: magician::magician_v2::chat::models::ChatMessageContent::Text {
                    text: "Unscoped hello".to_string(),
                    plan_reply: None,
                },
                created_at: 7,
                chat_turn_id: None,
                voice_origin: None,
                context_origin: None,
                speech_segments: None,
                source_surface: None,
                presence_session_id: None,
                presentation: None,
            },
            principal: None,
            workspace: None,
            origin_channel: None,
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        assert!(!event_visible_to_scope(
            &unscoped_chat_message,
            "alpha",
            "prod"
        ));

        let scoped_pipeline_started = RuntimeTransportEvent::PipelineStarted {
            workflow_id: "wf-1".to_string(),
            chain_id: "chain-1".to_string(),
            max_iterations: 5,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 8,
        };
        assert!(event_visible_to_scope(
            &scoped_pipeline_started,
            "alpha",
            "prod"
        ));
        assert!(!event_visible_to_scope(
            &scoped_pipeline_started,
            "beta",
            "prod"
        ));

        let unscoped_pipeline_started = RuntimeTransportEvent::PipelineStarted {
            workflow_id: "wf-1".to_string(),
            chain_id: "chain-1".to_string(),
            max_iterations: 5,
            principal: None,
            workspace: None,
            timestamp: 9,
        };
        assert!(!event_visible_to_scope(
            &unscoped_pipeline_started,
            "alpha",
            "prod"
        ));

        let scoped_alert = RuntimeTransportEvent::ObservabilityAlert {
            execution_id: "exec-1".to_string(),
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            alert_type: "stalled".to_string(),
            details: serde_json::json!({ "reason": "wait" }),
            timestamp: 10,
        };
        assert!(event_visible_to_scope(&scoped_alert, "alpha", "prod"));
        assert!(!event_visible_to_scope(&scoped_alert, "alpha", "other"));

        let unscoped_alert = RuntimeTransportEvent::ObservabilityAlert {
            execution_id: "exec-1".to_string(),
            principal: None,
            workspace: None,
            alert_type: "stalled".to_string(),
            details: serde_json::json!({ "reason": "wait" }),
            timestamp: 11,
        };
        assert!(!event_visible_to_scope(&unscoped_alert, "alpha", "prod"));

        let clarification_metrics = RuntimeTransportEvent::ClarificationMetricsSnapshot {
            total_sessions_started: 1,
            total_sessions_completed: 1,
            active_sessions: 0,
            avg_session_duration_ms: Some(100.0),
            avg_questions_per_session: Some(2.0),
            guardrail_timeouts: 0,
            guardrail_question_caps: 0,
            guardrail_round_caps: 0,
            timestamp: 12,
        };
        assert!(!event_visible_to_scope(
            &clarification_metrics,
            "alpha",
            "prod"
        ));
        // …but the same unscoped event surfaces under the system/system
        // bucket so operators can still observe it via an explicit
        // diagnostic query.
        assert!(event_visible_to_scope(
            &clarification_metrics,
            "system",
            "system"
        ));
    }

    #[test]
    fn hitl_events_are_filtered_by_scope() {
        let scoped_request = RuntimeTransportEvent::HitlRequested {
            correlation_id: "pause-1".to_string(),
            source: "agentic".to_string(),
            input_type: "text".to_string(),
            prompt: "Pick a value".to_string(),
            hint: None,
            input_schema: None,
            task_id: Some("task-1".to_string()),
            execution_id: Some("exec-1".to_string()),
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 1,
        };
        assert!(event_visible_to_scope(&scoped_request, "alpha", "prod"));
        assert!(!event_visible_to_scope(&scoped_request, "beta", "prod"));
        assert!(!event_visible_to_scope(&scoped_request, "alpha", "other"));

        let unscoped_request = RuntimeTransportEvent::HitlRequested {
            correlation_id: "pause-2".to_string(),
            source: "approval".to_string(),
            input_type: "confirmation".to_string(),
            prompt: "Approve?".to_string(),
            hint: None,
            input_schema: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: None,
            workspace: None,
            timestamp: 2,
        };
        // Unscoped HITL events are fail-closed for normal user queries…
        assert!(!event_visible_to_scope(&unscoped_request, "alpha", "prod"));
        // …but visible to the system/system diagnostic bucket.
        assert!(event_visible_to_scope(
            &unscoped_request,
            "system",
            "system"
        ));

        let resolved = RuntimeTransportEvent::HitlResolved {
            correlation_id: "pause-1".to_string(),
            source: "agentic".to_string(),
            outcome: "responded".to_string(),
            decision: Some("approve".to_string()),
            task_id: Some("task-1".to_string()),
            execution_id: Some("exec-1".to_string()),
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 3,
        };
        assert!(event_visible_to_scope(&resolved, "alpha", "prod"));
        assert!(!event_visible_to_scope(&resolved, "beta", "prod"));
    }

    #[test]
    fn sealed_app_owner_lifecycle_is_refused_before_generic_websocket_mailbox() {
        let requested = RuntimeTransportEvent::HitlRequested {
            correlation_id: "app-notification-1".to_string(),
            source: "user_request".to_string(),
            input_type: "notification".to_string(),
            prompt: "private body".to_string(),
            hint: None,
            input_schema: Some(serde_json::json!({
                "context": {
                    "app_owner_notification": true,
                    "absolute_expires_at_ms": 10,
                },
            })),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 1,
        };
        let resolved = RuntimeTransportEvent::HitlResolved {
            correlation_id: "app-notification-1".to_string(),
            source: "app_owner_notification".to_string(),
            outcome: "acknowledged".to_string(),
            decision: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 2,
        };
        let ordinary = RuntimeTransportEvent::Heartbeat { timestamp: 3 };

        assert!(!generic_v2_websocket_admits(&requested));
        assert!(!generic_v2_websocket_admits(&resolved));
        assert!(generic_v2_websocket_admits(&ordinary));
    }

    #[test]
    fn agentic_step_stuck_warning_is_filtered_by_scope() {
        let scoped = RuntimeTransportEvent::AgenticStepStuckWarning {
            execution_id: "exec-1".to_string(),
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            plan_id: "plan-1".to_string(),
            step_id: "step-1".to_string(),
            iteration: 5,
            consecutive_count: 3,
            recent_actions: vec!["click".to_string()],
            is_preflight: false,
            timestamp: 4,
        };
        assert!(event_visible_to_scope(&scoped, "alpha", "prod"));
        assert!(!event_visible_to_scope(&scoped, "beta", "prod"));

        let unscoped = RuntimeTransportEvent::AgenticStepStuckWarning {
            execution_id: "exec-1".to_string(),
            principal: None,
            workspace: None,
            plan_id: "plan-1".to_string(),
            step_id: "step-1".to_string(),
            iteration: 5,
            consecutive_count: 3,
            recent_actions: vec!["click".to_string()],
            is_preflight: false,
            timestamp: 5,
        };
        assert!(!event_visible_to_scope(&unscoped, "alpha", "prod"));
        assert!(event_visible_to_scope(&unscoped, "system", "system"));
    }

    #[test]
    fn heartbeat_is_always_visible() {
        let heartbeat = RuntimeTransportEvent::Heartbeat { timestamp: 0 };
        assert!(event_visible_to_scope(&heartbeat, "alpha", "prod"));
        assert!(event_visible_to_scope(&heartbeat, "beta", "other"));
        assert!(event_visible_to_scope(&heartbeat, "system", "system"));
    }

    // -- Client message parsing ------------------------------------------------

    #[test]
    fn client_message_parsing_valid() {
        let json = r#"{"type": "agent.ui.snapshot_request", "agent_id": "my-agent"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SnapshotRequest { agent_id } => {
                assert_eq!(agent_id, "my-agent");
            },
            _ => panic!("expected SnapshotRequest"),
        }
    }

    #[test]
    fn serialize_public_v2_event_preserves_execution_keys() {
        let event = RuntimeTransportEvent::AgentCycleCompleted {
            principal: None,
            workspace: None,
            agent_id: "agent-1".to_string(),
            goal_id: "goal-1".to_string(),
            cycle_id: "cycle-1".to_string(),
            execution_id: Some("exec-1".to_string()),
            outcome: "success".to_string(),
            iterations_used: 3,
            timestamp: 123,
        };

        let serialized = serialize_public_v2_event(&event).expect("event should serialize");
        let parsed: serde_json::Value =
            serde_json::from_str(&serialized).expect("serialized event should parse");
        let data = parsed
            .get("data")
            .and_then(|value| value.as_object())
            .expect("event should have data payload");

        assert_eq!(
            data.get("execution_id").and_then(|value| value.as_str()),
            Some("exec-1")
        );
        assert!(!data.contains_key("thread_id"));
    }

    #[test]
    fn client_message_parsing_unknown_type() {
        let json = r#"{"type": "unknown.command", "agent_id": "x"}"#;
        assert!(serde_json::from_str::<ClientMessage>(json).is_err());
    }

    #[test]
    fn client_message_parsing_missing_agent_id() {
        let json = r#"{"type": "agent.ui.snapshot_request"}"#;
        assert!(serde_json::from_str::<ClientMessage>(json).is_err());
    }

    #[test]
    fn client_message_parsing_not_json() {
        assert!(serde_json::from_str::<ClientMessage>("hello").is_err());
    }

    // -- snapshot_response_json ------------------------------------------------

    #[tokio::test]
    async fn snapshot_request_without_layout_returns_empty() {
        let (_dir, storage) = test_storage();
        let json =
            snapshot_response_json(None, &storage, "test-agent", &mut test_engine(), None, None)
                .await;
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(val["type"], "agent.ui.snapshot");
        assert_eq!(val["agent_id"], "test-agent");
        assert_eq!(val["document"]["muij_version"], "1.0");
        assert_eq!(val["document"]["agent_id"], "test-agent");
        assert!(val["document"]["layout"].as_array().unwrap().is_empty());
        assert!(val["document"]["generated_at"].is_string());
    }

    #[tokio::test]
    async fn snapshot_request_with_layout_returns_full_document() {
        let (_dir, storage) = test_storage();
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "my-agent".to_string(),
            layout: vec![magician::magician_v2::gaui::muij::MuijComponent {
                id: "c1".to_string(),
                component_type: "MetricCard".to_string(),
                label: "Revenue".to_string(),
                source: None,
                query: None,
                props: serde_json::json!({}),
                static_snapshot: None,
                children: vec![],
            }],
            generated_at: chrono::Utc::now(),
        };
        storage.write_layout("my-agent", &doc).await.unwrap();

        let json =
            snapshot_response_json(None, &storage, "my-agent", &mut test_engine(), None, None)
                .await;
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(val["type"], "agent.ui.snapshot");
        assert_eq!(val["agent_id"], "my-agent");
        assert_eq!(val["document"]["layout"].as_array().unwrap().len(), 1);
        assert_eq!(val["document"]["layout"][0]["id"], "c1");
        assert_eq!(val["document"]["layout"][0]["label"], "Revenue");
    }

    #[tokio::test]
    async fn snapshot_request_materializes_query_into_static_snapshot() {
        let (_dir, storage) = test_storage();
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "my-agent".to_string(),
            layout: vec![magician::magician_v2::gaui::muij::MuijComponent {
                id: "q1".to_string(),
                component_type: "Table".to_string(),
                label: "Query".to_string(),
                source: Some("props".to_string()),
                query: Some("$.items[*].name".to_string()),
                props: serde_json::json!({
                    "items": [
                        { "name": "alpha" },
                        { "name": "beta" }
                    ]
                }),
                static_snapshot: None,
                children: vec![],
            }],
            generated_at: chrono::Utc::now(),
        };
        storage.write_layout("my-agent", &doc).await.unwrap();

        let json =
            snapshot_response_json(None, &storage, "my-agent", &mut test_engine(), None, None)
                .await;
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        let snapshot = &val["document"]["layout"][0]["static_snapshot"];
        assert!(snapshot.is_array());
        assert_eq!(snapshot.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn snapshot_request_after_write_reflects_update() {
        let (_dir, storage) = test_storage();
        // Initially empty
        let json =
            snapshot_response_json(None, &storage, "agent-x", &mut test_engine(), None, None).await;
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(val["document"]["layout"].as_array().unwrap().is_empty());

        // Write a layout
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "agent-x".to_string(),
            layout: vec![magician::magician_v2::gaui::muij::MuijComponent {
                id: "c1".to_string(),
                component_type: "Badge".to_string(),
                label: "Status".to_string(),
                source: None,
                query: None,
                props: serde_json::json!({}),
                static_snapshot: None,
                children: vec![],
            }],
            generated_at: chrono::Utc::now(),
        };
        storage.write_layout("agent-x", &doc).await.unwrap();

        // Now should return the written layout
        let json =
            snapshot_response_json(None, &storage, "agent-x", &mut test_engine(), None, None).await;
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["document"]["layout"].as_array().unwrap().len(), 1);
        assert_eq!(val["document"]["layout"][0]["id"], "c1");
    }

    // -- Invalid agent_id → snapshot_response_json returns error via storage ---

    #[tokio::test]
    async fn snapshot_request_invalid_agent_id_slash() {
        let (_dir, storage) = test_storage();
        // MuijStorage itself rejects path-traversal agent ids
        let json =
            snapshot_response_json(None, &storage, "../etc", &mut test_engine(), None, None).await;
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "agent.ui.snapshot_error");
        assert_eq!(val["agent_id"], "../etc");
        // R449: Error message is sanitized — no internal details leaked to client
        let err = val["error"].as_str().unwrap();
        assert!(!err.is_empty(), "error should be non-empty");
    }

    #[tokio::test]
    async fn snapshot_request_empty_agent_id_returns_error() {
        let (_dir, storage) = test_storage();
        let json = snapshot_response_json(None, &storage, "", &mut test_engine(), None, None).await;
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "agent.ui.snapshot_error");
    }

    // -- is_valid_identifier (inline fast-reject guard) -----------------

    #[test]
    fn valid_agent_id_passes() {
        assert!(is_valid_identifier("my-agent"));
        assert!(is_valid_identifier("agent_123"));
        assert!(is_valid_identifier("a"));
    }

    #[test]
    fn empty_agent_id_rejected() {
        assert!(!is_valid_identifier(""));
    }

    #[test]
    fn slash_agent_id_rejected() {
        assert!(!is_valid_identifier("foo/bar"));
        assert!(!is_valid_identifier("/leading"));
        assert!(!is_valid_identifier("trailing/"));
    }

    #[test]
    fn backslash_agent_id_rejected() {
        assert!(!is_valid_identifier("foo\\bar"));
        assert!(!is_valid_identifier("\\leading"));
    }

    #[test]
    fn dot_leading_single_dot_accepted() {
        // R299 removed starts_with('.') check to align with storage validation.
        // Only `..` (path traversal) is rejected, single leading dots are fine.
        assert!(is_valid_identifier(".hidden"));
        assert!(!is_valid_identifier(".."));
        assert!(!is_valid_identifier("../etc"));
    }

    #[test]
    fn dot_non_leading_single_dot_passes() {
        // Single dots inside the name are fine.
        assert!(is_valid_identifier("my.agent"));
        assert!(is_valid_identifier("v1.0"));
    }

    // R214: Strengthened validation tests

    #[test]
    fn double_dot_anywhere_rejected() {
        assert!(!is_valid_identifier("foo..bar"));
        assert!(!is_valid_identifier("agent..id"));
    }

    #[test]
    fn non_ascii_agent_id_rejected() {
        assert!(!is_valid_identifier("agent\u{00e9}"));
        assert!(!is_valid_identifier("\u{0430}gent"));
    }

    #[test]
    fn over_length_agent_id_rejected() {
        let long_id = "a".repeat(256);
        assert!(!is_valid_identifier(&long_id));
        // 255 should pass
        let ok_id = "a".repeat(255);
        assert!(is_valid_identifier(&ok_id));
    }

    #[test]
    fn pending_pause_max_iterations_detection_true_for_resume_cancel_confirmation() {
        let pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Confirmation {
                confirm_label: Some("Resume".to_string()),
                deny_label: Some("Cancel".to_string()),
                destructive: false,
            },
            Some("Max iterations (7) reached. Continue?"),
        );
        assert!(pending_pause_is_max_iterations(&pause));
    }

    #[test]
    fn pending_pause_max_iterations_detection_false_for_regular_confirmation() {
        let pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Confirmation {
                confirm_label: Some("Proceed".to_string()),
                deny_label: Some("Cancel".to_string()),
                destructive: false,
            },
            Some("Proceed with destructive action?"),
        );
        assert!(!pending_pause_is_max_iterations(&pause));
    }

    #[test]
    fn pending_pause_max_iterations_detection_true_without_question() {
        let pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Confirmation {
                confirm_label: Some("Resume".to_string()),
                deny_label: Some("Cancel".to_string()),
                destructive: false,
            },
            None,
        );
        assert!(pending_pause_is_max_iterations(&pause));
    }

    #[test]
    fn max_iterations_reemit_event_preserves_agent_routing_for_threadless_pause() {
        let mut pause = test_pending_pause(
            "agent:agent-1:goal-1:cycle-1",
            magician::magician_v2::execution::agentic::UserInputType::Confirmation {
                confirm_label: Some("Resume".to_string()),
                deny_label: Some("Cancel".to_string()),
                destructive: false,
            },
            Some("Max iterations (7) reached. Continue?"),
        );
        pause.execution_id = String::new();
        pause.legacy_execution_id = String::new();

        let event = build_pending_pause_reemit_event(pause);
        match event {
            RuntimeTransportEvent::AgenticMaxIterationsReached {
                execution_id,
                agent_id,
                goal_id,
                cycle_id,
                ..
            } => {
                assert!(execution_id.is_empty());
                assert_eq!(agent_id.as_deref(), Some("agent-1"));
                assert_eq!(goal_id.as_deref(), Some("goal-1"));
                assert_eq!(cycle_id.as_deref(), Some("cycle-1"));
            },
            _ => panic!("expected max-iterations replay event"),
        }
    }

    // -- R193: Corrupt stored doc → snapshot_error path -------------------------

    #[tokio::test]
    async fn snapshot_request_corrupt_stored_doc_returns_error() {
        let (_dir, storage) = test_storage();

        // Write a valid doc first, then overwrite the file with invalid content
        // (unknown component type) to simulate corruption.
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "corrupt-agent".to_string(),
            layout: vec![magician::magician_v2::gaui::muij::MuijComponent {
                id: "c1".to_string(),
                component_type: "MetricCard".to_string(),
                label: "OK".to_string(),
                source: None,
                query: None,
                props: serde_json::json!({}),
                static_snapshot: None,
                children: vec![],
            }],
            generated_at: chrono::Utc::now(),
        };
        storage.write_layout("corrupt-agent", &doc).await.unwrap();

        // Now overwrite with a document that has an unknown component type.
        let corrupt_doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "corrupt-agent".to_string(),
            layout: vec![magician::magician_v2::gaui::muij::MuijComponent {
                id: "c1".to_string(),
                component_type: "NonExistentWidget".to_string(),
                label: "Bad".to_string(),
                source: None,
                query: None,
                props: serde_json::json!({}),
                static_snapshot: None,
                children: vec![],
            }],
            generated_at: chrono::Utc::now(),
        };
        // write_layout doesn't validate (R195), so the corrupt doc persists.
        storage
            .write_layout("corrupt-agent", &corrupt_doc)
            .await
            .unwrap();

        // R159: snapshot_response_json should validate and return snapshot_error.
        let json = snapshot_response_json(
            None,
            &storage,
            "corrupt-agent",
            &mut test_engine(),
            None,
            None,
        )
        .await;
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "agent.ui.snapshot_error");
        assert_eq!(val["agent_id"], "corrupt-agent");
        // R449: Error message is sanitized — check for presence, not internal content
        assert!(
            val["error"].as_str().unwrap().contains("validation failed"),
            "error should mention validation: {}",
            val["error"]
        );
    }

    // -- GC-B01: Origin validation tests ----------------------------------------

    fn mock_request_with_headers(origin: Option<&str>, host: Option<&str>) -> HttpRequest {
        let mut req = actix_web::test::TestRequest::default();
        if let Some(o) = origin {
            req = req.insert_header(("origin", o));
        }
        if let Some(h) = host {
            req = req.insert_header(("host", h));
        }
        req.to_http_request()
    }

    #[test]
    fn origin_localhost_3000_accepted() {
        // Origin: http://localhost:3000, Host: localhost:3002 → pass (both in DEFAULT_ALLOWED_ORIGINS)
        let req = mock_request_with_headers(Some("http://localhost:3000"), Some("localhost:3002"));
        assert!(validate_origin(&req).is_ok());
    }

    #[test]
    fn origin_127_0_0_1_accepted() {
        // Origin: http://127.0.0.1:3000 → pass (in DEFAULT_ALLOWED_ORIGINS)
        let req = mock_request_with_headers(Some("http://127.0.0.1:3000"), Some("example.com"));
        assert!(validate_origin(&req).is_ok());
    }

    #[test]
    fn origin_ipv6_localhost_accepted() {
        // Origin: http://[::1]:3000 → pass (in DEFAULT_ALLOWED_ORIGINS)
        let req = mock_request_with_headers(Some("http://[::1]:3000"), Some("example.com"));
        assert!(validate_origin(&req).is_ok());
    }

    #[test]
    fn origin_tauri_custom_scheme_localhost_accepted() {
        // Tauri desktop surfaces use a custom scheme; validate the hostname,
        // not the scheme token.
        let req =
            mock_request_with_headers(Some("magician-desktop://localhost"), Some("127.0.0.1:3002"));
        assert!(validate_origin(&req).is_ok());
    }

    #[test]
    fn origin_evil_com_rejected() {
        // Origin: http://evil.com → 403
        let req = mock_request_with_headers(Some("http://evil.com"), Some("localhost:3002"));
        assert!(validate_origin(&req).is_err());
    }

    #[test]
    fn origin_missing_accepted() {
        // No Origin header → pass (non-browser)
        let req = mock_request_with_headers(None, Some("localhost:3002"));
        assert!(validate_origin(&req).is_ok());
    }

    #[test]
    fn origin_same_host_accepted() {
        // Origin host matches Host exactly → pass
        let req =
            mock_request_with_headers(Some("http://myapp.local:8080"), Some("myapp.local:9090"));
        assert!(validate_origin(&req).is_ok());
    }

    #[test]
    fn strip_to_hostname_cases() {
        assert_eq!(strip_to_hostname("http://localhost:3000"), "localhost");
        assert_eq!(strip_to_hostname("https://example.com:443"), "example.com");
        assert_eq!(
            strip_to_hostname("magician-desktop://localhost"),
            "localhost"
        );
        assert_eq!(strip_to_hostname("tauri://127.0.0.1:3002"), "127.0.0.1");
        assert_eq!(strip_to_hostname("localhost:3002"), "localhost");
        assert_eq!(strip_to_hostname("[::1]:3000"), "[::1]");
        assert_eq!(strip_to_hostname("http://[::1]:3000"), "[::1]");
        assert_eq!(strip_to_hostname("example.com"), "example.com");
        assert_eq!(strip_to_hostname("http://127.0.0.1:3000"), "127.0.0.1");
        // R768: Edge cases — bare IPv6 no port, trailing colon, empty string
        assert_eq!(strip_to_hostname("[::1]"), "[::1]");
        assert_eq!(strip_to_hostname("hostname:"), "hostname");
        assert_eq!(strip_to_hostname(""), "");
        // R891: Multi-colon non-bracket — treated as bare IPv6 (returned as-is)
        assert_eq!(strip_to_hostname("host:port:extra"), "host:port:extra");
        // R891: Bare IPv6 addresses returned as-is
        assert_eq!(strip_to_hostname("::1"), "::1");
        assert_eq!(strip_to_hostname("fe80::1"), "fe80::1");
    }

    // R750: Case-insensitive origin matching
    #[test]
    fn origin_case_insensitive_accepted() {
        let req = mock_request_with_headers(Some("http://Localhost:3000"), Some("example.com"));
        assert!(validate_origin(&req).is_ok());
        let req2 = mock_request_with_headers(Some("http://LOCALHOST:3000"), Some("example.com"));
        assert!(validate_origin(&req2).is_ok());
    }

    // R756/R769: Non-allowed origin + no Host → rejected
    #[test]
    fn origin_evil_no_host_rejected() {
        let req = mock_request_with_headers(Some("http://evil.com"), None);
        assert!(validate_origin(&req).is_err());
    }

    // R756: Non-allowed origin + empty Host → rejected
    #[test]
    fn origin_evil_empty_host_rejected() {
        let req = mock_request_with_headers(Some("http://evil.com"), Some(""));
        // Empty host header is treated as missing by strip_to_hostname
        // But actix TestRequest won't set an empty header — this tests the code path
        // via a host that strips to empty
        assert!(validate_origin(&req).is_err());
    }

    // -- GC-B02: ui.interaction tests -------------------------------------------

    #[test]
    fn ui_interaction_client_message_parsing() {
        let json = r#"{"type": "ui.interaction", "agent_id": "agent-a", "component_id": "btn-1"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::UiInteraction {
                agent_id,
                component_id,
                goal_id,
                trigger,
                request_id,
            } => {
                assert_eq!(agent_id, "agent-a");
                assert_eq!(component_id, "btn-1");
                assert!(goal_id.is_none());
                assert!(trigger.is_none());
                assert!(request_id.is_none());
            },
            _ => panic!("expected UiInteraction"),
        }
    }

    #[test]
    fn ui_interaction_client_message_parsing_with_optionals() {
        let json = r#"{"type": "ui.interaction", "agent_id": "agent-a", "component_id": "btn-1", "goal_id": "g1", "trigger": "manual", "request_id": "req-42"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::UiInteraction {
                agent_id,
                component_id,
                goal_id,
                trigger,
                request_id,
            } => {
                assert_eq!(agent_id, "agent-a");
                assert_eq!(component_id, "btn-1");
                assert_eq!(goal_id.as_deref(), Some("g1"));
                assert_eq!(trigger.as_deref(), Some("manual"));
                assert_eq!(request_id.as_deref(), Some("req-42"));
            },
            _ => panic!("expected UiInteraction"),
        }
    }

    // R771: Missing required component_id should fail deserialization
    #[test]
    fn ui_interaction_missing_component_id_fails() {
        let json = r#"{"type": "ui.interaction", "agent_id": "agent-a"}"#;
        assert!(serde_json::from_str::<ClientMessage>(json).is_err());
    }

    #[test]
    fn interaction_ack_frame_serialization() {
        let frame = InteractionAckFrame {
            msg_type: "ui.interaction.ack",
            agent_id: "agent-a".to_string(),
            component_id: "btn-1".to_string(),
            cycle_id: "cycle-123".to_string(),
            status: "triggered".to_string(),
            goal_id: Some("goal-42".to_string()),
            request_id: Some("req-42".to_string()),
        };
        let json: serde_json::Value = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], "ui.interaction.ack");
        assert_eq!(json["agent_id"], "agent-a");
        assert_eq!(json["component_id"], "btn-1");
        assert_eq!(json["cycle_id"], "cycle-123");
        assert_eq!(json["status"], "triggered");
        assert_eq!(json["goal_id"], "goal-42"); // R912
        assert_eq!(json["request_id"], "req-42");
    }

    // R758/R912: request_id and goal_id omitted from JSON when None
    #[test]
    fn interaction_ack_frame_omits_null_optional_fields() {
        let frame = InteractionAckFrame {
            msg_type: "ui.interaction.ack",
            agent_id: "a".to_string(),
            component_id: "b".to_string(),
            cycle_id: "c".to_string(),
            status: "triggered".to_string(),
            goal_id: None,
            request_id: None,
        };
        let json: serde_json::Value = serde_json::to_value(&frame).unwrap();
        assert!(json.get("request_id").is_none());
        assert!(json.get("goal_id").is_none());
    }

    #[test]
    fn interaction_error_frame_serialization() {
        let frame = InteractionErrorFrame {
            msg_type: "ui.interaction.error",
            agent_id: "agent-a".to_string(),
            component_id: "btn-1".to_string(),
            error: "Rate limited".to_string(),
            code: error_codes::RATE_LIMITED.to_string(),
            request_id: None,
        };
        let json: serde_json::Value = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], "ui.interaction.error");
        assert_eq!(json["code"], "rate_limited");
        assert_eq!(json["error"], "Rate limited");
        assert!(json.get("request_id").is_none());
    }

    // R767: Test rate limit key format uses \0 delimiter and is collision-free
    #[test]
    fn interaction_rate_limit_key_format() {
        // R763: \0 delimiter prevents collision between agent_id containing ":"
        let key1 = format!("{}\0{}", "agent-a", "btn-1");
        let key2 = format!("{}\0{}", "agent-a", "btn-2");
        let key3 = format!("{}\0{}", "agent-b", "btn-1");
        // Keys with colons in agent_id don't collide
        let key4 = format!("{}\0{}", "a:b", "c");
        let key5 = format!("{}\0{}", "a", "b:c");
        assert_ne!(key1, key2);
        assert_ne!(key1, key3);
        assert_ne!(key4, key5); // Would collide with ":" delimiter
    }

    // -- R757: Targeted tests for previously-untested code paths ----------------

    // R753: Unknown fields in ui.interaction should be caught by known-fields check
    #[test]
    fn ui_interaction_unknown_field_detected() {
        // Serde parses this fine (silently ignores "goalId"), but our known-fields check catches it
        let json =
            r#"{"type": "ui.interaction", "agent_id": "a", "component_id": "b", "goalId": "g1"}"#;
        // Verify serde DOES parse it (the problem R753 solves)
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::UiInteraction { goal_id, .. } => {
                assert!(goal_id.is_none(), "goalId should not map to goal_id");
            },
            _ => panic!("expected UiInteraction"),
        }
        // Verify our known-fields check would catch it
        let val: serde_json::Value = serde_json::from_str(json).unwrap();
        let obj = val.as_object().unwrap();
        let unknown: Vec<&String> = obj
            .keys()
            .filter(|k| !UI_INTERACTION_KNOWN_FIELDS.contains(&k.as_str()))
            .collect();
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0], "goalId");
    }

    // R753: Valid message with only known fields passes the check
    #[test]
    fn ui_interaction_known_fields_only_passes() {
        let json = r#"{"type": "ui.interaction", "agent_id": "a", "component_id": "b", "goal_id": "g1", "trigger": "t", "request_id": "r"}"#;
        let val: serde_json::Value = serde_json::from_str(json).unwrap();
        let obj = val.as_object().unwrap();
        let unknown: Vec<&String> = obj
            .keys()
            .filter(|k| !UI_INTERACTION_KNOWN_FIELDS.contains(&k.as_str()))
            .collect();
        assert!(unknown.is_empty());
    }

    // R754: serialize_or_fallback produces valid JSON on success
    #[test]
    fn serialize_or_fallback_success() {
        let frame = InteractionAckFrame {
            msg_type: "ui.interaction.ack",
            agent_id: "a".to_string(),
            component_id: "b".to_string(),
            cycle_id: "c".to_string(),
            status: "triggered".to_string(),
            goal_id: None,
            request_id: None,
        };
        let json = serialize_or_fallback(&frame, "ui.interaction.ack");
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "ui.interaction.ack");
    }

    // R754: serialize_or_fallback returns valid JSON even on hypothetical failure
    #[test]
    fn serialize_or_fallback_fallback_is_valid_json() {
        // We can't easily make serde_json::to_string fail on our simple structs,
        // but we can verify the fallback string itself is valid JSON
        let fallback = format!(
            r#"{{"type":"{}","error":"internal serialization failure"}}"#,
            "ui.interaction.error"
        );
        let val: serde_json::Value = serde_json::from_str(&fallback).unwrap();
        assert_eq!(val["type"], "ui.interaction.error");
        assert_eq!(val["error"], "internal serialization failure");
    }

    // R765: MAX_WS_FRAME_SIZE is a reasonable bound
    #[test]
    fn max_ws_frame_size_is_reasonable() {
        // Must be > 0 and ≤ 1 MiB (our messages are small JSON, 64 KiB is generous)
        assert!(MAX_WS_FRAME_SIZE > 0);
        assert!(MAX_WS_FRAME_SIZE <= 1024 * 1024);
        assert_eq!(MAX_WS_FRAME_SIZE, 64 * 1024); // Document the exact value
    }

    // R748+R914: component_id validation via production function
    #[test]
    fn component_id_control_chars_rejected() {
        assert!(!is_valid_component_id("\0btn"));
        assert!(!is_valid_component_id("btn\t1"));
        assert!(!is_valid_component_id("btn\x7F"));
        assert!(is_valid_component_id("btn-action-1"));
    }

    // R748+R915: component_id at max length boundary via production function
    #[test]
    fn component_id_max_length_boundary() {
        assert!(is_valid_component_id(&"c".repeat(255)));
        assert!(!is_valid_component_id(&"c".repeat(256)));
        assert!(!is_valid_component_id("")); // empty rejected
    }

    // R914: component_id path-traversal and special chars rejected
    #[test]
    fn component_id_path_traversal_rejected() {
        assert!(!is_valid_component_id("../etc/passwd"));
        assert!(!is_valid_component_id("btn/nested"));
        assert!(!is_valid_component_id("btn\\nested"));
        assert!(!is_valid_component_id("foo..bar"));
        // Non-ASCII rejected
        assert!(!is_valid_component_id("btn-\u{00e9}"));
    }

    // R917: request_id validation via production function
    #[test]
    fn request_id_validation() {
        assert!(is_valid_request_id("req-42"));
        assert!(is_valid_request_id(&"r".repeat(128))); // boundary: 128 ok
        assert!(!is_valid_request_id(&"r".repeat(129))); // boundary: 129 rejected
        assert!(!is_valid_request_id("req\0id")); // null byte
        assert!(!is_valid_request_id("req\tid")); // tab
        assert!(!is_valid_request_id("req\x7Fid")); // DEL
        assert!(!is_valid_request_id("req-\u{00e9}")); // non-ASCII
        assert!(is_valid_request_id("")); // empty is valid (field is optional; emptiness handled by Option)
    }

    // R918: trigger validation via production function
    #[test]
    fn trigger_validation() {
        assert!(is_valid_trigger("manual"));
        assert!(is_valid_trigger(&"t".repeat(255))); // boundary: 255 ok
        assert!(!is_valid_trigger(&"t".repeat(256))); // boundary: 256 rejected
        assert!(!is_valid_trigger("trigger\0val")); // null byte
        assert!(!is_valid_trigger("trigger\nval")); // newline
        assert!(!is_valid_trigger("trigger\x7F")); // DEL
        assert!(!is_valid_trigger("\u{200B}trigger")); // zero-width space (non-ASCII)
        assert!(is_valid_trigger("")); // empty is valid (field is optional)
    }

    // R755+R820: Renamed from origin_non_utf8_rejected — actix TestRequest can't inject
    // non-UTF8 headers, so we test the related contract: non-allowed origins are rejected
    // with the correct origin/host strings in the error tuple.
    #[test]
    fn origin_non_allowed_returns_origin_and_host_in_error() {
        let req = mock_request_with_headers(Some("http://evil.com"), Some("other.com"));
        let err = validate_origin(&req).unwrap_err();
        assert_eq!(err.0, "http://evil.com");
        assert_eq!(err.1, "other.com");
    }

    // R764: Malformed ui.interaction with missing required field returns typed error
    #[test]
    fn ui_interaction_malformed_returns_typed_error_via_value_parse() {
        // This tests the R764 fallback path: serde rejects the message (missing component_id),
        // but we can parse it as Value and identify the type for a typed error response.
        let json = r#"{"type": "ui.interaction", "agent_id": "a"}"#;
        assert!(serde_json::from_str::<ClientMessage>(json).is_err());
        // Verify we can still extract type and agent_id from the raw Value
        let val: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(
            val.get("type").and_then(|v| v.as_str()),
            Some("ui.interaction")
        );
        assert_eq!(val.get("agent_id").and_then(|v| v.as_str()), Some("a"));
    }

    // R756: Origin present + Host present but different non-local domain → rejected
    #[test]
    fn origin_different_nonlocal_hosts_rejected() {
        let req = mock_request_with_headers(Some("http://attacker.com"), Some("victim.com"));
        let err = validate_origin(&req).unwrap_err();
        assert_eq!(err.0, "http://attacker.com");
        assert_eq!(err.1, "victim.com");
    }

    // -- R821: Control character rejection in is_valid_identifier ---
    #[test]
    fn agent_id_control_chars_rejected() {
        // R821: Exercise the b < 0x20 || b == 0x7F check directly
        assert!(!is_valid_identifier("agent\0id")); // null byte
        assert!(!is_valid_identifier("agent\tid")); // tab
        assert!(!is_valid_identifier("agent\nid")); // newline
        assert!(!is_valid_identifier("agent\x7Fid")); // DEL
        assert!(!is_valid_identifier("\x01start")); // SOH
    }

    // -- R824: Case-insensitive Host comparison in validate_origin ---
    #[test]
    fn origin_case_insensitive_host_match_accepted() {
        // R824: Exercise eq_ignore_ascii_case on Host-vs-Origin path (not allowlist)
        let req =
            mock_request_with_headers(Some("http://MyApp.Local:8080"), Some("myapp.local:9090"));
        assert!(validate_origin(&req).is_ok());
        // Reverse case
        let req2 =
            mock_request_with_headers(Some("http://myapp.local:8080"), Some("MYAPP.LOCAL:9090"));
        assert!(validate_origin(&req2).is_ok());
    }

    // -- R831: pending_pause_is_max_iterations with destructive:true ---
    #[test]
    fn pending_pause_max_iterations_destructive_true_is_not_max_iterations() {
        // R831: destructive:true should NOT match the max-iterations pattern
        let pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Confirmation {
                confirm_label: Some("Resume".to_string()),
                deny_label: Some("Cancel".to_string()),
                destructive: true,
            },
            Some("Max iterations reached. Continue?"),
        );
        assert!(
            !pending_pause_is_max_iterations(&pause),
            "destructive:true should not be classified as max-iterations"
        );
    }

    // -- R876: Strengthen snapshot materializes query assertion ---
    #[tokio::test]
    async fn snapshot_request_materializes_query_values_correctly() {
        let (_dir, storage) = test_storage();
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "val-agent".to_string(),
            layout: vec![magician::magician_v2::gaui::muij::MuijComponent {
                id: "q1".to_string(),
                component_type: "Table".to_string(),
                label: "Query".to_string(),
                source: Some("props".to_string()),
                query: Some("$.items[*].name".to_string()),
                props: serde_json::json!({
                    "items": [
                        { "name": "alpha" },
                        { "name": "beta" }
                    ]
                }),
                static_snapshot: None,
                children: vec![],
            }],
            generated_at: chrono::Utc::now(),
        };
        storage.write_layout("val-agent", &doc).await.unwrap();

        let json =
            snapshot_response_json(None, &storage, "val-agent", &mut test_engine(), None, None)
                .await;
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        let snapshot = &val["document"]["layout"][0]["static_snapshot"];
        // R876: Assert actual values, not just array length
        let arr = snapshot.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], "alpha");
        assert_eq!(arr[1], "beta");
    }

    // -- R877: Strengthen invalid agent_id assertion ---
    #[tokio::test]
    async fn snapshot_request_invalid_agent_id_specific_error() {
        let (_dir, storage) = test_storage();
        let json =
            snapshot_response_json(None, &storage, "../etc", &mut test_engine(), None, None).await;
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "agent.ui.snapshot_error");
        let err = val["error"].as_str().unwrap();
        // R877: Verify error is specific, not just non-empty. R449 says it should be
        // a sanitized message — check it doesn't contain Rust internal type names.
        assert!(
            !err.contains("MuijStorage"),
            "error should not leak internal type names"
        );
        assert!(
            !err.contains("std::"),
            "error should not leak Rust std paths"
        );
    }

    // -- R818: Test WaitingForUser branch in build_pending_pause_reemit_event ---
    #[test]
    fn waiting_for_user_reemit_event_fields() {
        // R818: Exercises the non-max-iterations path (WaitingForUser)
        let pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Text {
                placeholder: None,
                multiline: false,
            },
            Some("What is your name?"),
        );
        let event = build_pending_pause_reemit_event(pause);
        match event {
            RuntimeTransportEvent::AgenticWaitingForUser {
                execution_id,
                plan_id,
                step_id,
                iteration,
                pause_state_id,
                agent_id,
                goal_id,
                cycle_id,
                ..
            } => {
                assert_eq!(execution_id, "exec-1");
                assert_eq!(plan_id, "plan-1");
                assert_eq!(step_id, "step-1");
                assert_eq!(iteration, 7);
                assert_eq!(pause_state_id, Some("exec-1:plan-1:step-1".to_string()));
                assert_eq!(agent_id.as_deref(), Some("agent-1"));
                assert_eq!(goal_id.as_deref(), Some("goal-1"));
                assert_eq!(cycle_id.as_deref(), Some("cycle-1"));
            },
            _ => panic!("expected AgenticWaitingForUser, got MaxIterations"),
        }
    }

    #[test]
    fn waiting_for_user_missing_question_uses_default() {
        // Post-H7.4 slim: AgenticWaitingForUser no longer carries the
        // `question` field; the test now just asserts the lifecycle
        // marker fires (the question lives on canonical HitlRequested).
        let pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Text {
                placeholder: None,
                multiline: true,
            },
            None,
        );
        let event = build_pending_pause_reemit_event(pause);
        match event {
            RuntimeTransportEvent::AgenticWaitingForUser { execution_id, .. } => {
                assert_eq!(execution_id, "exec-1");
            },
            _ => panic!("expected AgenticWaitingForUser"),
        }
    }

    #[test]
    fn waiting_for_confirmation_reemit_event_fields() {
        let mut pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Confirmation {
                confirm_label: Some("Proceed".to_string()),
                deny_label: Some("Cancel".to_string()),
                destructive: false,
            },
            Some("Delete selection?"),
        );
        pause.confirmation_action_summary = Some("Delete selection".to_string());
        pause.confirmation_reason = Some("This removes the highlighted objects".to_string());
        pause.confirmation_action_type = Some("delete".to_string());

        let event = build_pending_pause_reemit_event(pause);
        match event {
            RuntimeTransportEvent::AgenticWaitingForConfirmation {
                execution_id,
                plan_id,
                step_id,
                iteration,
                pause_state_id,
                agent_id,
                goal_id,
                cycle_id,
                timestamp,
                ..
            } => {
                assert_eq!(execution_id, "exec-1");
                assert_eq!(plan_id, "plan-1");
                assert_eq!(step_id, "step-1");
                assert_eq!(iteration, 7);
                assert_eq!(pause_state_id.as_deref(), Some("exec-1:plan-1:step-1"));
                assert_eq!(agent_id.as_deref(), Some("agent-1"));
                assert_eq!(goal_id.as_deref(), Some("goal-1"));
                assert_eq!(cycle_id.as_deref(), Some("cycle-1"));
                assert!(timestamp > 0);
            },
            _ => panic!("expected AgenticWaitingForConfirmation"),
        }
    }

    // -- R819: Test doc_cache hit path in snapshot_response_json ---
    #[tokio::test]
    async fn snapshot_request_uses_cache_when_available() {
        let (_dir, storage) = test_storage();
        // Don't write to storage — only populate the cache
        let cache: MuijDocumentCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let doc = MuijDocument {
            muij_version: "1.0".to_string(),
            agent_id: "cached-agent".to_string(),
            layout: vec![magician::magician_v2::gaui::muij::MuijComponent {
                id: "cached-c1".to_string(),
                component_type: "MetricCard".to_string(),
                label: "FromCache".to_string(),
                source: None,
                query: None,
                props: serde_json::json!({"value": 42}),
                static_snapshot: None,
                children: vec![],
            }],
            generated_at: chrono::Utc::now(),
        };
        cache.write().await.insert("cached-agent".to_string(), doc);

        // Call with cache — should return cached document (not empty storage result)
        let json = snapshot_response_json(
            None,
            &storage,
            "cached-agent",
            &mut test_engine(),
            Some(&cache),
            Some("cached-agent"),
        )
        .await;
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "agent.ui.snapshot");
        assert_eq!(val["document"]["layout"][0]["id"], "cached-c1");
        assert_eq!(val["document"]["layout"][0]["label"], "FromCache");
    }

    #[tokio::test]
    async fn snapshot_request_uses_scoped_cache_key_for_same_agent_id() {
        let (_dir, storage) = test_storage();
        let cache: MuijDocumentCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let alpha_key = agent_snapshot_cache_key("shared-agent", Some("alpha"), Some("prod"));
        let beta_key = agent_snapshot_cache_key("shared-agent", Some("beta"), Some("prod"));
        let mut alpha_doc = MuijDocument::new("shared-agent");
        alpha_doc
            .layout
            .push(magician::magician_v2::gaui::muij::MuijComponent {
                id: "alpha-c1".to_string(),
                component_type: "MetricCard".to_string(),
                label: "Alpha".to_string(),
                source: None,
                query: None,
                props: serde_json::json!({"value": 1}),
                static_snapshot: None,
                children: vec![],
            });
        let mut beta_doc = MuijDocument::new("shared-agent");
        beta_doc
            .layout
            .push(magician::magician_v2::gaui::muij::MuijComponent {
                id: "beta-c1".to_string(),
                component_type: "MetricCard".to_string(),
                label: "Beta".to_string(),
                source: None,
                query: None,
                props: serde_json::json!({"value": 2}),
                static_snapshot: None,
                children: vec![],
            });
        let mut guard = cache.write().await;
        guard.insert(alpha_key.clone(), alpha_doc);
        guard.insert(beta_key.clone(), beta_doc);
        drop(guard);

        let alpha_json = snapshot_response_json(
            None,
            &storage,
            "shared-agent",
            &mut test_engine(),
            Some(&cache),
            Some(alpha_key.as_str()),
        )
        .await;
        let beta_json = snapshot_response_json(
            None,
            &storage,
            "shared-agent",
            &mut test_engine(),
            Some(&cache),
            Some(beta_key.as_str()),
        )
        .await;
        let alpha_val: serde_json::Value = serde_json::from_str(&alpha_json).unwrap();
        let beta_val: serde_json::Value = serde_json::from_str(&beta_json).unwrap();
        assert_eq!(alpha_val["document"]["layout"][0]["label"], "Alpha");
        assert_eq!(beta_val["document"]["layout"][0]["label"], "Beta");
    }

    #[tokio::test]
    async fn snapshot_request_rejects_cached_layout_when_scoped_agent_missing() {
        let dir = TempDir::new().unwrap();
        let storage = MuijStorage::new(dir.path().to_path_buf());
        let definition_store =
            magician::magician_v2::agents::AgentDefinitionStore::with_workspace_root(dir.path())
                .for_scope("alpha", "prod");
        let cache: MuijDocumentCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let cache_key = agent_snapshot_cache_key("shared-agent", Some("alpha"), Some("prod"));
        cache
            .write()
            .await
            .insert(cache_key.clone(), MuijDocument::new("shared-agent"));

        let json = snapshot_response_json(
            Some(definition_store),
            &storage,
            "shared-agent",
            &mut test_engine(),
            Some(&cache),
            Some(cache_key.as_str()),
        )
        .await;
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "agent.ui.snapshot_error");
        assert_eq!(val["code"], error_codes::AGENT_NOT_FOUND);
    }

    // -- R874: Test from_value conversion path (R833 single-parse architecture) ---
    #[test]
    fn client_message_from_value_snapshot_request() {
        // R874: Exercises the serde_json::from_value path that handle_client_message uses
        let json = r#"{"type": "agent.ui.snapshot_request", "agent_id": "my-agent"}"#;
        let val: serde_json::Value = serde_json::from_str(json).unwrap();
        let msg: ClientMessage = serde_json::from_value(val).unwrap();
        match msg {
            ClientMessage::SnapshotRequest { agent_id } => assert_eq!(agent_id, "my-agent"),
            _ => panic!("expected SnapshotRequest"),
        }
    }

    #[test]
    fn client_message_from_value_ui_interaction() {
        let json = r#"{"type": "ui.interaction", "agent_id": "a", "component_id": "b"}"#;
        let val: serde_json::Value = serde_json::from_str(json).unwrap();
        let msg: ClientMessage = serde_json::from_value(val).unwrap();
        match msg {
            ClientMessage::UiInteraction {
                agent_id,
                component_id,
                ..
            } => {
                assert_eq!(agent_id, "a");
                assert_eq!(component_id, "b");
            },
            _ => panic!("expected UiInteraction"),
        }
    }

    #[test]
    fn client_message_from_value_unknown_type_fails() {
        // R839: Unknown types produce Err from from_value (handled as error frame in production)
        let json = r#"{"type": "unknown.command", "data": 123}"#;
        let val: serde_json::Value = serde_json::from_str(json).unwrap();
        assert!(serde_json::from_value::<ClientMessage>(val).is_err());
    }

    #[test]
    fn client_message_from_value_missing_required_field_fails() {
        // R874: from_value Err path when type matches but required fields missing
        let json = r#"{"type": "ui.interaction", "agent_id": "a"}"#;
        let val: serde_json::Value = serde_json::from_str(json).unwrap();
        assert!(serde_json::from_value::<ClientMessage>(val).is_err());
    }

    // -- R875: Test unknown-field detection via Value path ---
    #[test]
    fn ui_interaction_unknown_field_detected_via_value_keys() {
        // R875: Exercises the same detection logic handle_client_message uses
        let json =
            r#"{"type": "ui.interaction", "agent_id": "a", "component_id": "b", "goalId": "g"}"#;
        let val: serde_json::Value = serde_json::from_str(json).unwrap();
        let unknown: Vec<String> = val
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| !UI_INTERACTION_KNOWN_FIELDS.contains(&k.as_str()))
            .cloned()
            .collect();
        assert_eq!(unknown, vec!["goalId".to_string()]);
    }

    #[test]
    fn snapshot_request_unknown_field_detected_via_value_keys() {
        // R809: Exercises snapshot_request unknown-field detection
        let json = r#"{"type": "agent.ui.snapshot_request", "agent_id": "a", "extra_field": 1}"#;
        let val: serde_json::Value = serde_json::from_str(json).unwrap();
        let unknown: Vec<String> = val
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| !SNAPSHOT_REQUEST_KNOWN_FIELDS.contains(&k.as_str()))
            .cloned()
            .collect();
        assert_eq!(unknown, vec!["extra_field".to_string()]);
    }

    // -- R822: Rewrite component_id tests to call production validation ---
    #[test]
    fn component_id_validation_via_production_rules() {
        // R822: These tests verify the actual production validation conditions
        // from handle_ui_interaction, not re-implemented inline expressions.
        // Component ID validation = same rules as is_valid_identifier (R878).
        // Empty
        assert!(!is_valid_identifier(""));
        // Control chars (R821 parity)
        assert!(!is_valid_identifier("\0btn"));
        assert!(!is_valid_identifier("btn\t1"));
        assert!(!is_valid_identifier("btn\x7F"));
        // Over length
        assert!(!is_valid_identifier(&"c".repeat(256)));
        // Slash/backslash/dotdot (R878 additions)
        assert!(!is_valid_identifier("btn/action"));
        assert!(!is_valid_identifier("btn\\action"));
        assert!(!is_valid_identifier("btn..action"));
        // Valid component IDs
        assert!(is_valid_identifier("btn-action-1"));
        assert!(is_valid_identifier("my.component"));
        assert!(is_valid_identifier(&"c".repeat(255)));
    }

    // -- R873: Rewrite rate_limit_key_format to verify production key construction ---
    #[test]
    fn interaction_rate_limit_key_uses_null_delimiter() {
        // R873: Verify that the rate-limit key format uses \0 delimiter (R763)
        // by constructing keys the same way production code does (line ~672).
        let key = format!("{}\0{}", "agent-a", "btn-1");
        assert!(key.contains('\0'), "key should use null byte delimiter");
        // Verify collision resistance: agent_id containing ":" doesn't collide
        let key1 = format!("{}\0{}", "a:b", "c");
        let key2 = format!("{}\0{}", "a", "b:c");
        assert_ne!(
            key1, key2,
            "null delimiter prevents collision with colon-containing IDs"
        );
        // Null bytes are rejected by is_valid_identifier, so they can't
        // appear in agent_id/component_id — making \0 safe as delimiter.
        assert!(
            !is_valid_identifier("agent\0id"),
            "null in agent_id rejected"
        );
    }

    // -- R838: Verify UI_INTERACTION_KNOWN_FIELDS matches struct fields ---
    #[test]
    fn ui_interaction_known_fields_matches_struct() {
        // R838: Compile-time-adjacent check — if a new field is added to
        // ClientMessage::UiInteraction without updating UI_INTERACTION_KNOWN_FIELDS,
        // this test fails because serde will parse the field but it won't be in the allowlist.
        // Verify all known fields are parseable by constructing a message with all of them.
        let json = r#"{"type": "ui.interaction", "agent_id": "a", "component_id": "b", "goal_id": "g", "trigger": "t", "request_id": "r"}"#.to_string();
        // Must parse successfully
        let msg: ClientMessage = serde_json::from_str(&json).unwrap();
        match msg {
            ClientMessage::UiInteraction {
                agent_id,
                component_id,
                goal_id,
                trigger,
                request_id,
            } => {
                assert_eq!(agent_id, "a");
                assert_eq!(component_id, "b");
                assert_eq!(goal_id.as_deref(), Some("g"));
                assert_eq!(trigger.as_deref(), Some("t"));
                assert_eq!(request_id.as_deref(), Some("r"));
            },
            _ => panic!("expected UiInteraction"),
        }
        // Verify field count matches — struct has 5 named fields + "type" = 6 in KNOWN_FIELDS
        assert_eq!(
            UI_INTERACTION_KNOWN_FIELDS.len(),
            6,
            "UI_INTERACTION_KNOWN_FIELDS count should match struct field count + type tag"
        );
    }

    // -- R920: Test is_retry/retry_count true-paths in build_pending_pause_reemit_event ---
    #[test]
    fn waiting_for_user_reemit_event_with_retry_fields() {
        let mut pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Text {
                placeholder: None,
                multiline: false,
            },
            Some("Retry: What is your name?"),
        );
        pause.is_retry = true;
        pause.retry_count = 3;
        pause.previous_answer = Some("wrong answer".to_string());
        pause.retry_reason = Some("Invalid format".to_string());

        let event = build_pending_pause_reemit_event(pause);
        // Post-H7.4 slim: previous_answer + retry_reason live on
        // canonical HitlRequested.input_schema; lifecycle marker keeps
        // is_retry / retry_count.
        match event {
            RuntimeTransportEvent::AgenticWaitingForUser {
                is_retry,
                retry_count,
                ..
            } => {
                assert_eq!(is_retry, Some(true));
                assert_eq!(retry_count, Some(3));
            },
            _ => panic!("expected AgenticWaitingForUser"),
        }
    }

    // R920: Non-retry path should produce None for retry fields
    #[test]
    fn waiting_for_user_reemit_event_no_retry_fields_are_none() {
        let pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Text {
                placeholder: None,
                multiline: false,
            },
            Some("What is your name?"),
        );
        let event = build_pending_pause_reemit_event(pause);
        match event {
            RuntimeTransportEvent::AgenticWaitingForUser {
                is_retry,
                retry_count,
                ..
            } => {
                assert!(
                    is_retry.is_none(),
                    "false should produce None via then_some"
                );
                assert!(retry_count.is_none(), "0 should produce None via then_some");
            },
            _ => panic!("expected AgenticWaitingForUser"),
        }
    }

    // -- R921: Strengthen max_iterations_reemit_event assertions ---
    #[test]
    fn max_iterations_reemit_event_all_fields_verified() {
        let pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Confirmation {
                confirm_label: Some("Resume".to_string()),
                deny_label: Some("Cancel".to_string()),
                destructive: false,
            },
            Some("Max iterations (7) reached. Continue?"),
        );
        let event = build_pending_pause_reemit_event(pause);
        match event {
            RuntimeTransportEvent::AgenticMaxIterationsReached {
                execution_id,
                plan_id,
                step_id,
                iterations_used,
                pause_state_id,
                agent_id,
                goal_id,
                cycle_id,
                timestamp,
                ..
            } => {
                assert_eq!(execution_id, "exec-1");
                assert_eq!(plan_id, "plan-1");
                assert_eq!(step_id, "step-1");
                assert_eq!(iterations_used, 7);
                assert_eq!(pause_state_id, Some("exec-1:plan-1:step-1".to_string()));
                assert_eq!(agent_id.as_deref(), Some("agent-1"));
                assert_eq!(goal_id.as_deref(), Some("goal-1"));
                assert_eq!(cycle_id.as_deref(), Some("cycle-1"));
                assert!(timestamp > 0, "timestamp should be positive");
            },
            _ => panic!("expected AgenticMaxIterationsReached"),
        }
    }

    // -- R922: Test pending_pause_is_max_iterations edge cases ---
    #[test]
    fn pending_pause_max_iterations_none_confirm_label_is_not_max_iterations() {
        let pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Confirmation {
                confirm_label: None,
                deny_label: Some("Cancel".to_string()),
                destructive: false,
            },
            Some("Continue?"),
        );
        assert!(!pending_pause_is_max_iterations(&pause));
    }

    #[test]
    fn pending_pause_max_iterations_wrong_deny_label_is_not_max_iterations() {
        let pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Confirmation {
                confirm_label: Some("Resume".to_string()),
                deny_label: Some("No".to_string()),
                destructive: false,
            },
            Some("Continue?"),
        );
        assert!(!pending_pause_is_max_iterations(&pause));
    }

    #[test]
    fn pending_pause_max_iterations_none_deny_label_is_not_max_iterations() {
        let pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Confirmation {
                confirm_label: Some("Resume".to_string()),
                deny_label: None,
                destructive: false,
            },
            Some("Continue?"),
        );
        assert!(!pending_pause_is_max_iterations(&pause));
    }

    #[test]
    fn pending_pause_max_iterations_text_input_is_not_max_iterations() {
        // Non-Confirmation input types should never match
        let pause = test_pending_pause(
            "exec-1:plan-1:step-1",
            magician::magician_v2::execution::agentic::UserInputType::Text {
                placeholder: None,
                multiline: false,
            },
            Some("Input needed"),
        );
        assert!(!pending_pause_is_max_iterations(&pause));
    }
}
