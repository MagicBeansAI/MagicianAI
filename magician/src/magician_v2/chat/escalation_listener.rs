//! EscalationListener — monitors execution escalation events and injects
//! action-required messages into the user's active chat session.
//!
//! Follows the same pattern as `TaskWatcher`: subscribe to
//! `RuntimeTransportBroadcaster`, match relevant events, build a `ChatMessage`,
//! persist via `ChatStore`, and re-broadcast as `ChatMessageReceived` so
//! the frontend picks it up over WebSocket.
//!
//! **Execution -> principal/workspace -> chat session** lookup:
//! 1. Resolve the execution-scoped task summary (has `principal` + `workspace`).
//! 2. `ChatStore::list_sessions(principal, workspace)` to find the active session.

use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::magician_v2::artifact_v2::{ArtifactV2Service, V3ReadApi};
use crate::magician_v2::json_traversal::clone_json_iteratively;
use crate::magician_v2::orchestrator::MagicianV2Orchestrator;
use crate::magician_v2::progress_channel_seam::surface_routing::chat_surface_renders_agent_event;
use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

use super::models::{
    ChatChannel, ChatMessage, ChatMessageContent, ChatMessageDirection, ChatSessionStatus,
    ContentBlockRecord, EscalationOption,
};
use super::storage::ChatStore;
use super::DEFAULT_AGENT_ID;

const MAX_CHAT_RESOLUTION_SUMMARY_CHARS: usize = 280;
const RESOLVED_ELSEWHERE_INPUT_TYPE: &str = "resolved_elsewhere";
const CLARIFICATION_RESPONDER_REQUEST_PREFIX: &str = "clarification_responder:";

fn clarification_responder_request_id(responder_id: &str) -> String {
    format!("{CLARIFICATION_RESPONDER_REQUEST_PREFIX}{responder_id}")
}

fn normalize_chat_resolution_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chat_resolution(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let mut end = 0usize;
    for (count, (idx, ch)) in trimmed.char_indices().enumerate() {
        if count >= max_chars {
            break;
        }
        end = idx + ch.len_utf8();
    }

    let candidate = trimmed[..end].trim_end();
    match candidate.rfind(char::is_whitespace) {
        Some(idx) if idx >= max_chars / 2 => format!("{}...", candidate[..idx].trim_end()),
        _ => format!("{candidate}..."),
    }
}

fn take_chat_resolution_sentences(text: &str, count: usize) -> String {
    let mut remaining = text.trim();
    let mut sentences = Vec::new();

    while !remaining.is_empty() && sentences.len() < count {
        if let Some(index) = remaining.find(". ") {
            let sentence = remaining[..=index].trim();
            if !sentence.is_empty() {
                sentences.push(sentence.to_string());
            }
            remaining = remaining[index + 2..].trim();
            continue;
        }

        sentences.push(remaining.to_string());
        break;
    }

    sentences.join(" ")
}

fn compact_execution_resolution_details(summary: &str) -> String {
    let normalized = normalize_chat_resolution_whitespace(summary);
    if normalized.is_empty() {
        return String::new();
    }

    let mut candidate = normalized.as_str();

    if let Some((prefix, _)) = candidate.split_once("Partial progress:") {
        candidate = prefix.trim();
    }

    if let Some((prefix, _)) = candidate.split_once("Test results:") {
        candidate = prefix.trim();
    }

    if let Some(after_goal) = candidate.strip_prefix("Goal achieved:") {
        let after_goal = after_goal.trim();
        if let Some(index) = after_goal.find(". ") {
            let remainder = after_goal[index + 2..].trim();
            if !remainder.is_empty() {
                candidate = remainder;
            }
        }
    }

    let concise = take_chat_resolution_sentences(candidate, 2);
    truncate_chat_resolution(&concise, MAX_CHAT_RESOLUTION_SUMMARY_CHARS)
}

fn build_execution_resolution_summary(outcome: &str, summary: &str) -> String {
    let compact = compact_execution_resolution_details(summary);
    if compact.is_empty() {
        format!("Execution completed: {outcome}")
    } else {
        format!("Execution completed: {outcome} — {compact}")
    }
}

/// Background listener that converts execution escalation events into chat
/// messages so the user sees them in the conversational UI (web chat, bubble,
/// and consumer channel bots).
pub struct EscalationListener {
    chat_store: Arc<dyn ChatStore>,
    v3_service: Arc<ArtifactV2Service>,
    orchestrator: Arc<MagicianV2Orchestrator>,
    event_broadcaster: Arc<RuntimeTransportBroadcaster>,
}

impl EscalationListener {
    pub fn new(
        chat_store: Arc<dyn ChatStore>,
        v3_service: Arc<ArtifactV2Service>,
        orchestrator: Arc<MagicianV2Orchestrator>,
        event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        Self {
            chat_store,
            v3_service,
            orchestrator,
            event_broadcaster,
        }
    }

    /// Spawn a background tokio task that listens for escalation events.
    ///
    /// The task runs until the broadcast channel is closed (process exit).
    pub fn start(&self) {
        let mut receiver = self.event_broadcaster.subscribe();
        let chat_store = Arc::clone(&self.chat_store);
        let v3_service = Arc::clone(&self.v3_service);
        let orchestrator = Arc::clone(&self.orchestrator);
        let event_broadcaster = Arc::clone(&self.event_broadcaster);

        tokio::spawn(async move {
            debug!("[ESCALATION-LISTENER] Background listener started");

            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        // --- Escalation events: inject action-required message ---
                        if let Some(escalation) = extract_escalation(&event) {
                            // Taxonomy-driven visibility gate. Canonical
                            // `hitl.requested` is the single taxonomy
                            // row for every escalation source. The
                            // listener still owns session lookup +
                            // EscalationCard rendering; this gate just
                            // inherits the chat surface's policy on
                            // whether HITL escalations are visible at
                            // all.
                            if !chat_surface_renders_agent_event("hitl.requested") {
                                continue;
                            }
                            let mut session_id = find_session_for_execution(
                                &escalation.execution_id,
                                v3_service.as_ref(),
                                &orchestrator,
                                chat_store.as_ref(),
                            )
                            .await;
                            if session_id.is_none() {
                                if let (Some(principal), Some(workspace)) = (
                                    escalation.principal.as_deref(),
                                    escalation.workspace.as_deref(),
                                ) {
                                    session_id = find_session_for_principal(
                                        principal,
                                        workspace,
                                        chat_store.as_ref(),
                                    )
                                    .await;
                                }
                            }
                            let session_id = match session_id {
                                Some(id) => id,
                                None => {
                                    debug!(
                                        "[ESCALATION-LISTENER] No chat session found for execution {}",
                                        escalation.execution_id
                                    );
                                    continue;
                                },
                            };

                            let msg = ChatMessage::new(
                                Uuid::new_v4().to_string(),
                                session_id.clone(),
                                ChatMessageDirection::System,
                                ChatMessageContent::Escalation {
                                    execution_id: escalation.execution_id.clone(),
                                    pause_state_id: escalation.pause_state_id.clone(),
                                    request_id: escalation
                                        .clarification_responder_id
                                        .as_deref()
                                        .map(clarification_responder_request_id),
                                    escalation_type: escalation.escalation_type.clone(),
                                    input_type: Some(escalation.input_type.clone()),
                                    input_schema: escalation
                                        .input_schema
                                        .as_ref()
                                        .map(clone_json_iteratively),
                                    question: escalation.question.clone(),
                                    hint: escalation.hint.clone(),
                                    options: escalation.options.clone(),
                                    resolved: false,
                                },
                                Utc::now().timestamp_millis(),
                            )
                            // Escalations live OUTSIDE chat-turn scope —
                            // they're not anchored to a specific user
                            // request's activity card. Future: tie via
                            // the pause's owning task's chat_turn_id.
                            .with_chat_turn_id(None)
                            .with_source_surface(None)
                            .with_presence_session_id(None)
                            .with_voice_origin(None)
                            .with_speech_segments(None);

                            // Single-bus model: emit-only. ChatStoreSink
                            // owns the chat-store write; it logs its own
                            // persistence failures.
                            let (principal, workspace, origin_channel) =
                                session_transport_context(chat_store.as_ref(), &session_id)
                                    .await
                                    .map(|(principal, workspace, origin_channel)| {
                                        (Some(principal), Some(workspace), Some(origin_channel))
                                    })
                                    .unwrap_or((None, None, None));
                            event_broadcaster.emit_transport_only(
                                RuntimeTransportEvent::ChatMessageReceived {
                                    session_id: session_id.clone(),
                                    message: msg,
                                    principal,
                                    workspace,
                                    origin_channel,
                                    timestamp: chrono::Utc::now().timestamp_millis(),
                                },
                            );

                            debug!(
                                "[ESCALATION-LISTENER] Injected {} escalation for execution {} into session {}",
                                escalation.escalation_type, escalation.execution_id, session_id
                            );
                        }

                        // --- Canonical HitlRequested { source: "user_request" }
                        // — inject escalation card for service-backed user
                        // requests. Post-H7.1 replacement for the legacy
                        // UserRequestPending chat injection.
                        if let Some(ur_info) = extract_canonical_user_request(&event) {
                            if chat_surface_renders_agent_event("hitl.requested") {
                                let session_id_opt = if !ur_info.principal.is_empty() {
                                    find_session_for_principal(
                                        &ur_info.principal,
                                        &ur_info.workspace,
                                        chat_store.as_ref(),
                                    )
                                    .await
                                } else if let Some(exec_id) = ur_info.execution_id.as_deref() {
                                    find_session_for_execution(
                                        exec_id,
                                        v3_service.as_ref(),
                                        &orchestrator,
                                        chat_store.as_ref(),
                                    )
                                    .await
                                } else {
                                    None
                                };
                                if let Some(session_id) = session_id_opt {
                                    let execution_id = ur_info
                                        .execution_id
                                        .clone()
                                        .unwrap_or_else(|| ur_info.correlation_id.clone());
                                    let msg = ChatMessage::new(
                                        Uuid::new_v4().to_string(),
                                        session_id.clone(),
                                        ChatMessageDirection::System,
                                        ChatMessageContent::Escalation {
                                            execution_id,
                                            pause_state_id: ur_info.correlation_id.clone(),
                                            request_id: Some(ur_info.correlation_id.clone()),
                                            escalation_type: ur_info.request_type.clone(),
                                            input_type: Some(ur_info.input_type.clone()),
                                            input_schema: ur_info
                                                .input_schema
                                                .as_ref()
                                                .map(clone_json_iteratively),
                                            question: ur_info.question.clone(),
                                            hint: ur_info.hint.clone(),
                                            options: ur_info.options.clone(),
                                            resolved: false,
                                        },
                                        Utc::now().timestamp_millis(),
                                    )
                                    .with_chat_turn_id(None)
                                    .with_source_surface(None)
                                    .with_presence_session_id(None)
                                    .with_voice_origin(None)
                                    .with_speech_segments(None);
                                    // Single-bus model: emit-only; ChatStoreSink persists.
                                    let (principal, workspace, origin_channel) =
                                        session_transport_context(chat_store.as_ref(), &session_id)
                                            .await
                                            .map(|(p, w, c)| (Some(p), Some(w), Some(c)))
                                            .unwrap_or((None, None, None));
                                    event_broadcaster.emit_transport_only(
                                        RuntimeTransportEvent::ChatMessageReceived {
                                            session_id: session_id.clone(),
                                            message: msg,
                                            principal,
                                            workspace,
                                            origin_channel,
                                            timestamp: chrono::Utc::now().timestamp_millis(),
                                        },
                                    );
                                    debug!(
                                        "[ESCALATION-LISTENER] Injected canonical user_request {} into session {}",
                                        ur_info.correlation_id, session_id
                                    );
                                }
                            }
                        }

                        // --- Canonical HitlResolved { source: "user_request" }
                        // — inject resolved chat row.
                        if let Some(resolved) = extract_canonical_user_request_resolved(&event) {
                            let session_id_opt = if !resolved.principal.is_empty() {
                                find_session_for_principal(
                                    &resolved.principal,
                                    &resolved.workspace,
                                    chat_store.as_ref(),
                                )
                                .await
                            } else if let Some(exec_id) = resolved.execution_id.as_deref() {
                                find_session_for_execution(
                                    exec_id,
                                    v3_service.as_ref(),
                                    &orchestrator,
                                    chat_store.as_ref(),
                                )
                                .await
                            } else {
                                None
                            };
                            if let Some(session_id) = session_id_opt {
                                let execution_id = resolved
                                    .execution_id
                                    .clone()
                                    .unwrap_or_else(|| resolved.correlation_id.clone());
                                let summary = match resolved.decision.as_deref() {
                                    Some(decision) if !decision.is_empty() => format!(
                                        "User request resolved: {} ({})",
                                        decision, resolved.outcome
                                    ),
                                    _ => format!("User request resolved ({})", resolved.outcome),
                                };
                                let msg = ChatMessage::new(
                                    Uuid::new_v4().to_string(),
                                    session_id.clone(),
                                    ChatMessageDirection::System,
                                    ChatMessageContent::EscalationResolved {
                                        execution_id,
                                        pause_state_id: Some(resolved.correlation_id.clone()),
                                        request_id: Some(resolved.correlation_id.clone()),
                                        summary,
                                        task_id: None,
                                        output_files: Vec::new(),
                                    },
                                    Utc::now().timestamp_millis(),
                                )
                                .with_chat_turn_id(None)
                                .with_source_surface(None)
                                .with_presence_session_id(None)
                                .with_voice_origin(None)
                                .with_speech_segments(None);
                                // Single-bus model: emit-only; ChatStoreSink persists.
                                let (principal, workspace, origin_channel) =
                                    session_transport_context(chat_store.as_ref(), &session_id)
                                        .await
                                        .map(|(p, w, c)| (Some(p), Some(w), Some(c)))
                                        .unwrap_or((None, None, None));
                                event_broadcaster.emit_transport_only(
                                    RuntimeTransportEvent::ChatMessageReceived {
                                        session_id: session_id.clone(),
                                        message: msg,
                                        principal,
                                        workspace,
                                        origin_channel,
                                        timestamp: chrono::Utc::now().timestamp_millis(),
                                    },
                                );
                                debug!(
                                    "[ESCALATION-LISTENER] Injected canonical user_request resolution {} into session {}",
                                    resolved.correlation_id, session_id
                                );
                            }
                        }

                        // Persist canonical clarification resolutions so a
                        // reloaded chat can reconcile the durable escalation
                        // row by its question id. The resolution deliberately
                        // matches on `pause_state_id`, not the compatibility
                        // responder marker, because one task may have several
                        // independently pending questions.
                        if let Some(resolved) = extract_canonical_clarification_resolved(&event) {
                            if chat_surface_renders_agent_event("hitl.resolved") {
                                let mut session_id = find_session_for_execution(
                                    &resolved.execution_id,
                                    v3_service.as_ref(),
                                    &orchestrator,
                                    chat_store.as_ref(),
                                )
                                .await;
                                if session_id.is_none() {
                                    if let (Some(principal), Some(workspace)) = (
                                        resolved.principal.as_deref(),
                                        resolved.workspace.as_deref(),
                                    ) {
                                        session_id = find_session_for_principal(
                                            principal,
                                            workspace,
                                            chat_store.as_ref(),
                                        )
                                        .await;
                                    }
                                }
                                if let Some(session_id) = session_id {
                                    let summary = if resolved.outcome == "cancelled" {
                                        "Clarification no longer active".to_string()
                                    } else {
                                        "Clarification answered".to_string()
                                    };
                                    let msg = ChatMessage::new(
                                        Uuid::new_v4().to_string(),
                                        session_id.clone(),
                                        ChatMessageDirection::System,
                                        ChatMessageContent::EscalationResolved {
                                            execution_id: resolved.execution_id.clone(),
                                            pause_state_id: Some(resolved.correlation_id.clone()),
                                            request_id: None,
                                            summary,
                                            task_id: resolved.responder_id.clone(),
                                            output_files: Vec::new(),
                                        },
                                        Utc::now().timestamp_millis(),
                                    )
                                    .with_chat_turn_id(None)
                                    .with_source_surface(None)
                                    .with_presence_session_id(None)
                                    .with_voice_origin(None)
                                    .with_speech_segments(None);
                                    let (principal, workspace, origin_channel) =
                                        session_transport_context(chat_store.as_ref(), &session_id)
                                            .await
                                            .map(|(p, w, c)| (Some(p), Some(w), Some(c)))
                                            .unwrap_or((None, None, None));
                                    event_broadcaster.emit_transport_only(
                                        RuntimeTransportEvent::ChatMessageReceived {
                                            session_id,
                                            message: msg,
                                            principal,
                                            workspace,
                                            origin_channel,
                                            timestamp: chrono::Utc::now().timestamp_millis(),
                                        },
                                    );
                                }
                            }
                        }

                        // --- Resolution events: inject resolved message ---
                        if let Some(resolution) = extract_resolution(&event) {
                            // Canonical `hitl.resolved` — same gate as
                            // `hitl.requested` above, governs resolution
                            // injection.
                            if !chat_surface_renders_agent_event("hitl.resolved") {
                                continue;
                            }
                            let session_id = match find_session_for_execution(
                                &resolution.execution_id,
                                v3_service.as_ref(),
                                &orchestrator,
                                chat_store.as_ref(),
                            )
                            .await
                            {
                                Some(id) => id,
                                None => continue,
                            };

                            // Look up the resolved execution's task so we can
                            // project its `outputs/` deliverables into the
                            // resolution message. The chat UI renders these
                            // inline (markdown/PDF/image previews + download
                            // chips for unknown MIMEs) instead of leaving
                            // the user staring at a text-only "completed"
                            // card with no link to the actual artefact.
                            let (task_id, output_files, user_summary): (
                                Option<String>,
                                Vec<ContentBlockRecord>,
                                Option<String>,
                            ) = match v3_service
                                .find_execution_scope(&resolution.execution_id)
                                .await
                            {
                                Ok(Some((scope, found_task_id, _))) => {
                                    let blocks = match chat_store.get_session(&session_id).await {
                                        Ok(Some(session)) => {
                                            crate::magician_v2::chat::service::project_task_outputs_for_chat_session(
                                                v3_service.as_ref(),
                                                &session,
                                                &found_task_id,
                                            )
                                            .await
                                        },
                                        Ok(None) => Vec::new(),
                                        Err(error) => {
                                            warn!(
                                                "[ESCALATION-LISTENER] Failed to load chat session {} for output projection: {}",
                                                session_id, error
                                            );
                                            Vec::new()
                                        },
                                    };
                                    let user_summary =
                                        crate::magician_v2::chat::service::load_task_user_summary(
                                            v3_service.workspace(),
                                            &scope.principal(),
                                            &scope.workspace(),
                                            &found_task_id,
                                        )
                                        .await;
                                    (Some(found_task_id), blocks, user_summary)
                                },
                                _ => (None, Vec::new(), None),
                            };

                            // Prefer the agent's `out_task_user_<task>_<exec>.md`
                            // deliverable as the displayed summary when
                            // present — it's the well-formatted, canonical
                            // answer the agent's finalizer writes. The
                            // LLM-supplied `resolution.summary` (the
                            // `goal_achieved` text) is often a goal
                            // restatement and unhelpful as a chat-visible
                            // outcome. Falls back to it only when the
                            // markdown file is missing.
                            let resolved_summary =
                                user_summary.unwrap_or_else(|| resolution.summary.clone());

                            let msg = ChatMessage::new(
                                Uuid::new_v4().to_string(),
                                session_id.clone(),
                                ChatMessageDirection::System,
                                ChatMessageContent::EscalationResolved {
                                    execution_id: resolution.execution_id.clone(),
                                    pause_state_id: resolution.pause_state_id.clone(),
                                    request_id: None,
                                    summary: resolved_summary,
                                    task_id,
                                    output_files,
                                },
                                Utc::now().timestamp_millis(),
                            )
                            .with_chat_turn_id(None)
                            .with_source_surface(None)
                            .with_presence_session_id(None)
                            .with_voice_origin(None)
                            .with_speech_segments(None);

                            // Single-bus model: emit-only; ChatStoreSink persists.
                            let (principal, workspace, origin_channel) =
                                session_transport_context(chat_store.as_ref(), &session_id)
                                    .await
                                    .map(|(principal, workspace, origin_channel)| {
                                        (Some(principal), Some(workspace), Some(origin_channel))
                                    })
                                    .unwrap_or((None, None, None));
                            event_broadcaster.emit_transport_only(
                                RuntimeTransportEvent::ChatMessageReceived {
                                    session_id: session_id.clone(),
                                    message: msg,
                                    principal,
                                    workspace,
                                    origin_channel,
                                    timestamp: chrono::Utc::now().timestamp_millis(),
                                },
                            );

                            debug!(
                                "[ESCALATION-LISTENER] Injected resolution for execution {} into session {}",
                                resolution.execution_id, session_id
                            );
                        }
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            "[ESCALATION-LISTENER] Broadcast receiver lagged, skipped {} events",
                            n
                        );
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        debug!("[ESCALATION-LISTENER] Broadcast channel closed, stopping listener");
                        break;
                    },
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Intermediate struct for extracted escalation data.
struct EscalationInfo {
    execution_id: String,
    pause_state_id: String,
    /// Task/workflow id used by the clarification responder. This differs
    /// from `execution_id` for V3 planning (`task_*` vs `planexec_*`).
    clarification_responder_id: Option<String>,
    principal: Option<String>,
    workspace: Option<String>,
    escalation_type: String,
    /// The pause's expected `input_type` (e.g. `external_action`,
    /// `confirmation`, `guidance`). The UI dispatches the resume payload
    /// off this rather than `escalation_type` because the two don't always
    /// agree — e.g. an LLM-issued `need_user_input(input_type=external_action)`
    /// produces `escalation_type=user_input` but a payload that must be
    /// `external_action` (otherwise the resume API rejects with 400).
    input_type: String,
    input_schema: Option<serde_json::Value>,
    question: String,
    hint: Option<String>,
    options: Vec<EscalationOption>,
}

/// Map a realtime event to an `EscalationInfo` if it represents an escalation.
///
/// Post-H7.4 lifecycle slim: agentic pause HITL payload now lives on
/// canonical `HitlRequested { source: "agentic" }`. The legacy
/// `AgenticWaitingForUser` / `AgenticWaitingForConfirmation` are
/// lifecycle markers only and no longer carry the question / options /
/// action_summary fields this function needs. Read canonical envelopes
/// for the HITL fields; the legacy variants stay for execution-panel
/// observability but are not consulted here.
fn extract_escalation(event: &RuntimeTransportEvent) -> Option<EscalationInfo> {
    match event {
        RuntimeTransportEvent::HitlRequested {
            correlation_id,
            source,
            input_type,
            prompt,
            hint,
            input_schema,
            execution_id,
            principal,
            workspace,
            ..
        } if source == "agentic" => {
            let pause_id = correlation_id.clone();
            if pause_id.is_empty() {
                return None;
            }
            let exec_id = execution_id.clone()?;
            let escalation_trigger = input_schema
                .as_ref()
                .and_then(|schema| schema.get("escalation_trigger"))
                .and_then(|value| value.as_str())
                .map(str::to_string);
            let action_type = input_schema
                .as_ref()
                .and_then(|schema| schema.get("action_type"))
                .and_then(|value| value.as_str());
            let (esc_type, options) = match escalation_trigger.as_deref() {
                Some("cannot_proceed") => (
                    "cannot_proceed".to_string(),
                    vec![
                        EscalationOption::external_action("guidance", "Provide Guidance", true),
                        EscalationOption::external_action(
                            "done",
                            schema_string(input_schema.as_ref(), "done_label", "Mark Done"),
                            false,
                        ),
                    ],
                ),
                Some("loop_detected") => (
                    "loop_detected".to_string(),
                    vec![
                        EscalationOption::external_action("guidance", "Provide Guidance", true),
                        EscalationOption::external_action(
                            "done",
                            schema_string(input_schema.as_ref(), "done_label", "Mark Done"),
                            false,
                        ),
                    ],
                ),
                Some("tool_authorization") => (
                    "tool_authorization".to_string(),
                    vec![
                        EscalationOption::choice("allow_once", "Allow Once", false),
                        EscalationOption::choice("allow_always", "Allow for This Run", false),
                        EscalationOption::choice("deny", "Deny", false),
                    ],
                ),
                Some("sandbox_override") => (
                    "sandbox_override".to_string(),
                    vec![
                        EscalationOption::choice("allow_once", "Allow Once", false),
                        EscalationOption::choice("deny", "Deny", false),
                    ],
                ),
                _ if input_type == "confirmation" || action_type.is_some() => (
                    "confirmation".to_string(),
                    vec![
                        EscalationOption::confirmation(
                            "approve",
                            schema_string(input_schema.as_ref(), "confirm_label", "Approve"),
                            true,
                        ),
                        EscalationOption::confirmation(
                            "deny",
                            schema_string(input_schema.as_ref(), "deny_label", "Deny"),
                            false,
                        ),
                    ],
                ),
                _ => {
                    let opts = match input_type.as_str() {
                        "external_action" => vec![
                            EscalationOption::external_action(
                                "done",
                                schema_string(input_schema.as_ref(), "done_label", "Mark Done"),
                                false,
                            ),
                            EscalationOption::external_action("guidance", "Provide Guidance", true),
                        ],
                        _ => vec![EscalationOption::choice("respond", "Respond", true)],
                    };
                    ("user_input".to_string(), opts)
                },
            };
            Some(EscalationInfo {
                execution_id: exec_id,
                pause_state_id: pause_id,
                clarification_responder_id: None,
                principal: principal.clone(),
                workspace: workspace.clone(),
                escalation_type: esc_type,
                input_type: input_type.clone(),
                input_schema: input_schema.as_ref().map(clone_json_iteratively),
                question: prompt.clone(),
                hint: hint.clone(),
                options,
            })
        },

        // Phase H8.4 — canonical `HitlRequested { source: "clarification" }`
        // arm. V3 planning clarifications (and V2 ask-loop clarifications)
        // now inject an escalation card into the active chat thread so the
        // user can answer from the same surface they read the question in.
        // The canonical respond endpoint accepts either freeform text or a
        // single-choice "respond" affordance.
        RuntimeTransportEvent::HitlRequested {
            correlation_id,
            source,
            input_type,
            prompt,
            hint,
            input_schema,
            task_id,
            execution_id,
            principal,
            workspace,
            ..
        } if source == "clarification" => {
            let pause_id = correlation_id.clone();
            if pause_id.is_empty() {
                return None;
            }
            // V3 carries the durable `planexec_*` id in execution_id and
            // the responder's task/workflow id in task_id. Legacy AskLoop
            // has no task_id, so its workflow execution id serves both roles.
            let exec_id = execution_id.clone()?;
            let responder_id = task_id.clone().unwrap_or_else(|| exec_id.clone());
            Some(EscalationInfo {
                execution_id: exec_id,
                pause_state_id: pause_id,
                clarification_responder_id: Some(responder_id),
                principal: principal.clone(),
                workspace: workspace.clone(),
                escalation_type: "clarification".to_string(),
                input_type: input_type.clone(),
                input_schema: input_schema.as_ref().map(clone_json_iteratively),
                question: prompt.clone(),
                hint: hint.clone(),
                options: clarification_options(input_type, input_schema.as_ref()),
            })
        },

        RuntimeTransportEvent::AgenticMaxIterationsReached {
            execution_id,
            iterations_used,
            pause_state_id,
            ..
        } => {
            let pause_id = pause_state_id.clone().unwrap_or_default();
            if pause_id.is_empty() {
                return None;
            }

            Some(EscalationInfo {
                execution_id: execution_id.clone(),
                pause_state_id: pause_id,
                clarification_responder_id: None,
                principal: None,
                workspace: None,
                escalation_type: "max_iterations".to_string(),
                // max-iterations escalations route through agentic-continue
                // (Keep Trying) or use a confirmation payload (Stop). The
                // resume API never receives this input_type, but record the
                // logical shape for completeness.
                input_type: "confirmation".to_string(),
                input_schema: Some(serde_json::json!({
                    "type": "confirmation",
                    "confirm_label": "Keep Trying",
                    "deny_label": "Stop"
                })),
                question: format!(
                    "Agent reached the maximum of {} iterations. Would you like it to keep trying?",
                    iterations_used
                ),
                hint: None,
                options: vec![
                    EscalationOption::continue_execution("continue", "Keep Trying"),
                    EscalationOption::confirmation("stop", "Stop", false),
                ],
            })
        },

        _ => None,
    }
}

fn clarification_options(
    input_type: &str,
    input_schema: Option<&serde_json::Value>,
) -> Vec<EscalationOption> {
    if input_type == "confirmation" {
        return vec![
            EscalationOption::confirmation(
                "confirm",
                schema_string(input_schema, "confirm_label", "Confirm"),
                true,
            ),
            EscalationOption::confirmation(
                "deny",
                schema_string(input_schema, "deny_label", "Cancel"),
                false,
            ),
        ];
    }

    let options = input_schema
        .and_then(|schema| schema.get("options"))
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| {
                    let id = value.get("id").or_else(|| value.get("value"))?.as_str()?;
                    let label = value.get("label")?.as_str()?;
                    let requires_input = value
                        .get("requires_input")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false);
                    Some(if input_type == "external_action" {
                        EscalationOption::external_action(id, label, requires_input)
                    } else {
                        EscalationOption::choice(id, label, requires_input)
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !options.is_empty() {
        return options;
    }

    if input_type == "external_action" {
        vec![EscalationOption::external_action(
            "done",
            schema_string(input_schema, "done_label", "Mark Done"),
            false,
        )]
    } else {
        vec![EscalationOption::choice("respond", "Respond", true)]
    }
}

fn schema_string(input_schema: Option<&serde_json::Value>, key: &str, fallback: &str) -> String {
    input_schema
        .and_then(|schema| schema.get(key))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(fallback)
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolutionInfo {
    execution_id: String,
    pause_state_id: Option<String>,
    summary: String,
}

/// Extract resolution info from `AgenticResumed` / `AgenticExecutionCompleted`.
fn extract_resolution(event: &RuntimeTransportEvent) -> Option<ResolutionInfo> {
    match event {
        RuntimeTransportEvent::AgenticResumed {
            execution_id,
            pause_state_id,
            input_type,
            ..
        } => Some(ResolutionInfo {
            execution_id: execution_id.clone(),
            pause_state_id: pause_state_id.clone(),
            summary: if input_type == RESOLVED_ELSEWHERE_INPUT_TYPE {
                "Escalation already answered or no longer active".to_string()
            } else {
                format!("Agent resumed ({})", input_type)
            },
        }),

        RuntimeTransportEvent::AgenticExecutionCompleted {
            execution_id,
            outcome,
            summary,
            ..
        } => Some(ResolutionInfo {
            execution_id: execution_id.clone(),
            pause_state_id: None,
            summary: build_execution_resolution_summary(outcome, summary),
        }),

        _ => None,
    }
}

/// Look up the active chat session for the principal that owns a given execution.
///
/// Path: execution_id -> bound execution scope -> task -> principal/workspace
///       -> ChatStore::list_sessions(principal, workspace) -> first Active session.
/// Canonical-envelope view of a pending user_request escalation,
/// destructured from `RuntimeTransportEvent::HitlRequested { source:
/// "user_request" }`. Carries everything the chat injection block
/// needs without re-parsing the variant inline.
struct CanonicalUserRequestInfo {
    correlation_id: String,
    request_type: String,
    input_type: String,
    input_schema: Option<serde_json::Value>,
    question: String,
    hint: Option<String>,
    principal: String,
    workspace: String,
    execution_id: Option<String>,
    options: Vec<EscalationOption>,
}

fn extract_canonical_user_request(
    event: &RuntimeTransportEvent,
) -> Option<CanonicalUserRequestInfo> {
    // Owner notifications deliberately use the canonical user_request
    // request source for response routing, but must never be copied into the
    // unbounded ChatStore escalation history.
    if crate::magician_v2::realtime_events::is_app_owner_notification_transport_event(event) {
        return None;
    }
    let RuntimeTransportEvent::HitlRequested {
        correlation_id,
        source,
        input_type,
        prompt,
        hint,
        input_schema,
        execution_id,
        principal,
        workspace,
        ..
    } = event
    else {
        return None;
    };
    if source != "user_request" {
        return None;
    }
    let request_type = input_schema
        .as_ref()
        .and_then(|schema| schema.get("request_type"))
        .and_then(|value| value.as_str())
        .unwrap_or("user_request")
        .to_string();
    let options = clarification_options(input_type, input_schema.as_ref());
    Some(CanonicalUserRequestInfo {
        correlation_id: correlation_id.clone(),
        request_type,
        input_type: input_type.clone(),
        input_schema: input_schema.as_ref().map(clone_json_iteratively),
        question: prompt.clone(),
        hint: hint.clone(),
        principal: principal.clone().unwrap_or_default(),
        workspace: workspace.clone().unwrap_or_default(),
        execution_id: execution_id.clone(),
        options,
    })
}

struct CanonicalUserRequestResolvedInfo {
    correlation_id: String,
    outcome: String,
    decision: Option<String>,
    principal: String,
    workspace: String,
    execution_id: Option<String>,
}

struct CanonicalClarificationResolvedInfo {
    correlation_id: String,
    outcome: String,
    responder_id: Option<String>,
    execution_id: String,
    principal: Option<String>,
    workspace: Option<String>,
}

fn extract_canonical_clarification_resolved(
    event: &RuntimeTransportEvent,
) -> Option<CanonicalClarificationResolvedInfo> {
    let RuntimeTransportEvent::HitlResolved {
        correlation_id,
        source,
        outcome,
        task_id,
        execution_id,
        principal,
        workspace,
        ..
    } = event
    else {
        return None;
    };
    if source != "clarification" || correlation_id.is_empty() {
        return None;
    }
    let durable_execution_id = execution_id.clone()?;
    Some(CanonicalClarificationResolvedInfo {
        correlation_id: correlation_id.clone(),
        outcome: outcome.clone(),
        responder_id: task_id
            .clone()
            .or_else(|| Some(durable_execution_id.clone())),
        execution_id: durable_execution_id,
        principal: principal.clone(),
        workspace: workspace.clone(),
    })
}

fn extract_canonical_user_request_resolved(
    event: &RuntimeTransportEvent,
) -> Option<CanonicalUserRequestResolvedInfo> {
    if crate::magician_v2::realtime_events::is_app_owner_notification_transport_event(event) {
        return None;
    }
    let RuntimeTransportEvent::HitlResolved {
        correlation_id,
        source,
        outcome,
        decision,
        execution_id,
        principal,
        workspace,
        ..
    } = event
    else {
        return None;
    };
    if source != "user_request" {
        return None;
    }
    Some(CanonicalUserRequestResolvedInfo {
        correlation_id: correlation_id.clone(),
        outcome: outcome.clone(),
        decision: decision.clone(),
        principal: principal.clone().unwrap_or_default(),
        workspace: workspace.clone().unwrap_or_default(),
        execution_id: execution_id.clone(),
    })
}

async fn find_session_for_principal(
    principal: &str,
    workspace: &str,
    chat_store: &dyn ChatStore,
) -> Option<String> {
    if principal.is_empty() {
        return None;
    }
    let sessions = match chat_store.list_sessions(principal, workspace).await {
        Ok(s) => s,
        Err(e) => {
            warn!(
                "[ESCALATION-LISTENER] Failed to list sessions for principal {} workspace {}: {}",
                principal, workspace, e
            );
            return None;
        },
    };
    let default_thread = crate::magician_v2::storage::task_models::default_ui_thread_id();
    if let Some(active) = sessions
        .iter()
        .find(|s| s.status == ChatSessionStatus::Active && s.ui_thread_id == default_thread)
    {
        return Some(active.id.clone());
    }
    match chat_store
        .get_or_create_active_session(
            principal,
            workspace,
            &default_thread,
            &ChatChannel::web(),
            DEFAULT_AGENT_ID,
        )
        .await
    {
        Ok(session) => Some(session.id),
        Err(e) => {
            warn!(
                "[ESCALATION-LISTENER] Failed to create session for principal {} workspace {}: {}",
                principal, workspace, e
            );
            None
        },
    }
}

async fn find_session_for_execution(
    execution_id: &str,
    v3_service: &ArtifactV2Service,
    _orchestrator: &MagicianV2Orchestrator,
    chat_store: &dyn ChatStore,
) -> Option<String> {
    // 1. Execution -> V3 task scope
    let (scope, task_id, _) = v3_service.find_execution_scope(execution_id).await.ok()??;
    let task = v3_service.get_task(&scope, &task_id).await.ok()?;
    let principal = &scope.principal();

    // v0.6.653 — chat-spawned tasks (Internal and any Persistent task
    // created via `dispatch_create_task`) carry the
    // exact spawning session in `manifest.chat_session_id`. Prefer
    // that over the principal+ui_thread_id scan, which would land
    // the escalation on the wrong session when multiple Active
    // sessions exist on the same thread (multi-active-sessions
    // feature). Fall back to the scan when `chat_session_id` is
    // unset (non-chat origins — scheduler, API, autonomous agents).
    if let Some(ref pinned_session_id) = task.manifest.chat_session_id {
        if !pinned_session_id.is_empty() {
            match chat_store.get_session(pinned_session_id).await {
                Ok(Some(session)) if session.status == ChatSessionStatus::Active => {
                    return Some(session.id);
                },
                Ok(_) => {
                    // Session vanished or non-active — fall through
                    // to the scan so the escalation still lands
                    // somewhere usable.
                },
                Err(e) => {
                    warn!(
                        "[ESCALATION-LISTENER] Failed to load pinned chat session {} for task {}: {} \
                         — falling back to thread scan",
                        pinned_session_id, task_id, e
                    );
                },
            }
        }
    }

    // 2. Principal/workspace -> Active chat session
    let sessions =
        match chat_store
            .list_sessions(principal, &scope.workspace())
            .await
        {
            Ok(s) => s,
            Err(e) => {
                warn!(
                "[ESCALATION-LISTENER] Failed to list sessions for principal {} workspace {}: {}",
                principal, scope.workspace(), e
            );
                return None;
            },
        };

    // If no active session exists, create one so escalations always land somewhere
    if let Some(active) = sessions.iter().find(|s| {
        s.status == ChatSessionStatus::Active && s.ui_thread_id == task.manifest.ui_thread_id
    }) {
        return Some(active.id.clone());
    }

    // Create session on demand
    match chat_store
        .get_or_create_active_session(
            principal,
            &scope.workspace(),
            &task.manifest.ui_thread_id,
            &ChatChannel::web(),
            DEFAULT_AGENT_ID,
        )
        .await
    {
        Ok(session) => Some(session.id),
        Err(e) => {
            warn!(
                "[ESCALATION-LISTENER] Failed to create session for principal {}: {}",
                principal, e
            );
            None
        },
    }
}

async fn session_transport_context(
    chat_store: &dyn ChatStore,
    session_id: &str,
) -> Option<(String, String, ChatChannel)> {
    chat_store
        .get_session(session_id)
        .await
        .ok()
        .flatten()
        .map(|session| (session.principal, session.workspace, session.origin_channel))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::{
        build_execution_resolution_summary, clarification_responder_request_id,
        compact_execution_resolution_details, extract_canonical_clarification_resolved,
        extract_canonical_user_request, extract_escalation, extract_resolution,
        RESOLVED_ELSEWHERE_INPUT_TYPE,
    };
    use crate::magician_v2::chat::models::EscalationOptionAction;
    use crate::magician_v2::realtime_events::RuntimeTransportEvent;

    #[test]
    fn compact_execution_resolution_removes_partial_progress_trace() {
        let raw = "Execution cancelled after 32 iteration(s). Partial progress:\nIteration 32: browser:click_coords(648.9615, 364.9832) - SUCCESS (11114ms)";
        assert_eq!(
            compact_execution_resolution_details(raw),
            "Execution cancelled after 32 iteration(s)."
        );
    }

    #[test]
    fn compact_execution_resolution_drops_repeated_goal_sentence() {
        let raw = "Goal achieved: Navigate to http://localhost:5173/tests/sota-tests/16-scroll-at-element.html and then execute all tests on this page and set pass/fail appropriately. All 4 tests on the page were executed successfully and marked as Pass. window.testRunner.getResults() returned {\"passed\":4,\"failed\":0,\"pending\":0}. Test results: omitted";
        assert_eq!(
            compact_execution_resolution_details(raw),
            "All 4 tests on the page were executed successfully and marked as Pass. window.testRunner.getResults() returned {\"passed\":4,\"failed\":0,\"pending\":0}."
        );
    }

    #[test]
    fn resolved_elsewhere_resume_event_uses_inactive_summary() {
        let event = RuntimeTransportEvent::AgenticResumed {
            execution_id: "exec-1".to_string(),
            principal: None,
            workspace: None,
            pause_state_id: Some("pause-1".to_string()),
            plan_id: "plan-1".to_string(),
            step_id: "step-1".to_string(),
            resumed_from_iteration: 0,
            input_type: RESOLVED_ELSEWHERE_INPUT_TYPE.to_string(),
            user_responded: true,
            agent_id: None,
            goal_id: None,
            cycle_id: None,
            timestamp: 1,
        };

        let resolution = extract_resolution(&event).expect("resolution");
        assert_eq!(resolution.execution_id, "exec-1");
        assert_eq!(resolution.pause_state_id.as_deref(), Some("pause-1"));
        assert_eq!(
            resolution.summary,
            "Escalation already answered or no longer active"
        );
    }

    #[test]
    fn build_execution_resolution_summary_keeps_prefix_and_shortens_body() {
        let raw =
            "Goal achieved: Navigate somewhere. All checks passed cleanly. Extra details follow.";
        assert_eq!(
            build_execution_resolution_summary("goal_achieved", raw),
            "Execution completed: goal_achieved — All checks passed cleanly. Extra details follow."
        );
    }

    #[test]
    fn v3_clarification_keeps_plan_execution_and_task_responder() {
        let event = RuntimeTransportEvent::HitlRequested {
            correlation_id: "question-1".to_string(),
            source: "clarification".to_string(),
            input_type: "multi_choice".to_string(),
            prompt: "Which environments?".to_string(),
            hint: Some("Choose every deployment target".to_string()),
            input_schema: Some(serde_json::json!({
                "options": [
                    {"id": "staging", "label": "Staging"},
                    {"value": "prod", "label": "Production"}
                ]
            })),
            task_id: Some("task-1".to_string()),
            execution_id: Some("planexec-1".to_string()),
            agent_id: None,
            principal: Some("owner".to_string()),
            workspace: Some("default".to_string()),
            timestamp: 1,
        };

        let escalation = extract_escalation(&event).expect("clarification escalation");
        assert_eq!(escalation.execution_id, "planexec-1");
        assert_eq!(
            escalation.clarification_responder_id.as_deref(),
            Some("task-1")
        );
        assert_eq!(escalation.input_type, "multi_choice");
        assert_eq!(
            escalation.hint.as_deref(),
            Some("Choose every deployment target")
        );
        assert_eq!(
            escalation
                .options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            vec!["staging", "prod"]
        );
        assert_eq!(
            clarification_responder_request_id("task-1"),
            "clarification_responder:task-1"
        );
    }

    #[test]
    fn legacy_ask_loop_clarification_uses_workflow_for_both_identities() {
        let event = RuntimeTransportEvent::HitlRequested {
            correlation_id: "legacy-question".to_string(),
            source: "clarification".to_string(),
            input_type: "file_path".to_string(),
            prompt: "Which file?".to_string(),
            hint: None,
            input_schema: Some(serde_json::json!({"multiple": true})),
            task_id: None,
            execution_id: Some("workflow-legacy".to_string()),
            agent_id: None,
            principal: Some("owner".to_string()),
            workspace: Some("default".to_string()),
            timestamp: 1,
        };

        let escalation = extract_escalation(&event).expect("legacy clarification escalation");
        assert_eq!(escalation.execution_id, "workflow-legacy");
        assert_eq!(
            escalation.clarification_responder_id.as_deref(),
            Some("workflow-legacy")
        );
        assert_eq!(escalation.input_type, "file_path");
        assert_eq!(
            escalation.input_schema,
            Some(serde_json::json!({"multiple": true}))
        );
    }

    #[test]
    fn confirmation_actions_and_labels_are_server_authored() {
        let event = RuntimeTransportEvent::HitlRequested {
            correlation_id: "confirmation-1".to_string(),
            source: "clarification".to_string(),
            input_type: "confirmation".to_string(),
            prompt: "Ship it?".to_string(),
            hint: None,
            input_schema: Some(serde_json::json!({
                "confirm_label": "Launch",
                "deny_label": "Hold"
            })),
            task_id: Some("task-1".to_string()),
            execution_id: Some("planexec-1".to_string()),
            agent_id: None,
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            timestamp: 1,
        };

        let escalation = extract_escalation(&event).expect("confirmation escalation");
        assert_eq!(escalation.options[0].label, "Launch");
        assert_eq!(escalation.options[1].label, "Hold");
        assert_eq!(
            escalation.options[0].action,
            EscalationOptionAction::RespondConfirmation { confirmed: true }
        );
        assert_eq!(
            escalation.options[1].action,
            EscalationOptionAction::RespondConfirmation { confirmed: false }
        );
    }

    #[test]
    fn max_iterations_exposes_dedicated_continuation_action() {
        let event = RuntimeTransportEvent::AgenticMaxIterationsReached {
            execution_id: "exec-1".to_string(),
            iterations_used: 32,
            pause_state_id: Some("pause-1".to_string()),
            plan_id: "plan-1".to_string(),
            step_id: "step-1".to_string(),
            agent_id: None,
            goal_id: None,
            cycle_id: None,
            principal: None,
            workspace: None,
            timestamp: 1,
        };

        let escalation = extract_escalation(&event).expect("max-iteration escalation");
        assert_eq!(
            escalation.options[0].action,
            EscalationOptionAction::ContinueExecution
        );
        assert_eq!(
            escalation.options[1].action,
            EscalationOptionAction::RespondConfirmation { confirmed: false }
        );
    }

    #[test]
    fn user_request_keeps_input_contract_and_choice_action() {
        let schema = serde_json::json!({
            "request_type": "pick_account",
            "allow_other": true,
            "options": [{"id": "billing", "label": "Billing"}]
        });
        let event = RuntimeTransportEvent::HitlRequested {
            correlation_id: "request-1".to_string(),
            source: "user_request".to_string(),
            input_type: "choice".to_string(),
            prompt: "Which account?".to_string(),
            hint: Some("Use the billing alias".to_string()),
            input_schema: Some(schema.clone()),
            task_id: None,
            execution_id: Some("exec-1".to_string()),
            agent_id: None,
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            timestamp: 1,
        };

        let request = extract_canonical_user_request(&event).expect("user request");
        assert_eq!(request.input_type, "choice");
        assert_eq!(request.input_schema, Some(schema));
        assert_eq!(request.hint.as_deref(), Some("Use the billing alias"));
        assert_eq!(
            request.options[0].action,
            EscalationOptionAction::RespondChoice
        );
    }

    #[test]
    fn clarification_resolution_keeps_durable_execution_and_responder() {
        let event = RuntimeTransportEvent::HitlResolved {
            correlation_id: "question-2".to_string(),
            source: "clarification".to_string(),
            outcome: "responded".to_string(),
            decision: None,
            task_id: Some("task-2".to_string()),
            execution_id: Some("planexec-2".to_string()),
            agent_id: None,
            principal: Some("owner".to_string()),
            workspace: Some("default".to_string()),
            timestamp: 2,
        };

        let resolved =
            extract_canonical_clarification_resolved(&event).expect("clarification resolution");
        assert_eq!(resolved.execution_id, "planexec-2");
        assert_eq!(resolved.responder_id.as_deref(), Some("task-2"));
        assert_eq!(resolved.correlation_id, "question-2");
    }
}
