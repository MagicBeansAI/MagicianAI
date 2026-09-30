//! PlanningListener — mirrors V3 task-plan lifecycle events into the active
//! thread chat session as persisted task-status messages.

use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::magician_v2::progress_channel_seam::surface_routing::chat_surface_renders_agent_event;
use crate::magician_v2::realtime_events::{
    RuntimeAgentEventType, RuntimeTransportBroadcaster, RuntimeTransportEvent,
};

use super::models::{
    ChatChannel, ChatMessage, ChatMessageContent, ChatMessageDirection, ChatSessionStatus,
};
use super::storage::ChatStore;

pub struct PlanningListener {
    chat_store: Arc<dyn ChatStore>,
    event_broadcaster: Arc<RuntimeTransportBroadcaster>,
}

impl PlanningListener {
    pub fn new(
        chat_store: Arc<dyn ChatStore>,
        event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        Self {
            chat_store,
            event_broadcaster,
        }
    }

    pub fn start(&self) {
        let mut receiver = self.event_broadcaster.subscribe();
        let chat_store = Arc::clone(&self.chat_store);
        let broadcaster_for_scope = Arc::clone(&self.event_broadcaster);
        let event_broadcaster = Arc::clone(&self.event_broadcaster);

        tokio::spawn(async move {
            debug!("[PLANNING-LISTENER] Background listener started");

            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        let Some(mut update) = planning_update_from_event(&event) else {
                            continue;
                        };
                        // Canonical HITL clarification events don't carry
                        // `ui_thread_id` directly. The request arm
                        // recovers it from `input_schema.ui_thread_id`
                        // (populated by V3 service); the resolve arm
                        // can't (HitlResolved has no input_schema), so
                        // recover from the broadcaster's in-memory
                        // canonical-scope registry keyed by execution_id.
                        // This is a sync DashMap read — no disk I/O, no
                        // flock — so it's safe on the listener hot path.
                        // The H8.9 review explicitly flagged that calling
                        // `v3_service.get_task` here was a critical
                        // mistake: it acquires a cross-process flock on
                        // every event, can block the broadcast channel,
                        // and triggers Lagged drops for unrelated
                        // subscribers under load.
                        if update.ui_thread_id
                            == crate::magician_v2::storage::task_models::default_ui_thread_id()
                        {
                            let execution_id = execution_id_for_event(&event);
                            if let Some(exec_id) = execution_id {
                                if let Some(scope) =
                                    broadcaster_for_scope.lookup_canonical_event_scope(exec_id)
                                {
                                    if !scope.ui_thread_id.is_empty() {
                                        update.ui_thread_id = scope.ui_thread_id;
                                    }
                                }
                            }
                        }
                        // Taxonomy-driven visibility gate. Planning
                        // lifecycle status updates land on chat as
                        // `task.status_changed`-equivalent rows; if a
                        // future chat policy hides those, the listener
                        // stops injecting too.
                        if !chat_surface_renders_agent_event(
                            RuntimeAgentEventType::TaskStatusChanged.as_str(),
                        ) {
                            continue;
                        }
                        let Some(session_id) = find_or_create_session_for_thread(
                            &update.principal,
                            &update.workspace,
                            &update.ui_thread_id,
                            &update.agent_id,
                            chat_store.as_ref(),
                        )
                        .await
                        else {
                            continue;
                        };

                        let message = ChatMessage::new(
                            Uuid::new_v4().to_string(),
                            session_id.clone(),
                            ChatMessageDirection::System,
                            ChatMessageContent::TaskStatusUpdate {
                                task_id: update.task_id.clone(),
                                status: update.status,
                                display_label: None,
                                summary: Some(update.summary),
                                execution_id: None,
                                ui_thread_id: Some(update.ui_thread_id.clone()),
                                output_files: Vec::new(),
                                synthesis_pending: false,
                                speech_tts: None,
                            },
                            Utc::now().timestamp_millis(),
                        )
                        .with_chat_turn_id(None)
                        .with_source_surface(None)
                        .with_presence_session_id(None)
                        .with_voice_origin(None)
                        .with_speech_segments(None);

                        // Emit-only: `ChatStoreSink` subscribes to
                        // `ChatMessageReceived` and is the sole writer to
                        // `chat_store`. A previous version of this code
                        // wrote directly AND emitted, which caused every
                        // planning update to land in the per-session
                        // chat document twice.
                        event_broadcaster.emit_transport_only(
                            RuntimeTransportEvent::ChatMessageReceived {
                                session_id: session_id.clone(),
                                message,
                                principal: Some(update.principal.clone()),
                                workspace: Some(update.workspace.clone()),
                                origin_channel: session_origin_channel(
                                    chat_store.as_ref(),
                                    &session_id,
                                )
                                .await,
                                timestamp: chrono::Utc::now().timestamp_millis(),
                            },
                        );
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(
                            skipped,
                            "[PLANNING-LISTENER] Lagged on planning transport stream"
                        );
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

struct PlanningChatUpdate {
    principal: String,
    workspace: String,
    task_id: String,
    agent_id: String,
    ui_thread_id: String,
    status: String,
    summary: String,
}

fn planning_update_from_event(event: &RuntimeTransportEvent) -> Option<PlanningChatUpdate> {
    match event {
        RuntimeTransportEvent::V3PlanningStarted {
            principal,
            workspace,
            task_id,
            task_title,
            agent_id,
            ui_thread_id,
            ..
        } => Some(PlanningChatUpdate {
            principal: principal.clone(),
            workspace: workspace.clone(),
            task_id: task_id.clone(),
            agent_id: agent_id.clone(),
            ui_thread_id: ui_thread_id.clone(),
            status: "planning".to_string(),
            summary: format!("Planning started for \"{}\".", task_title),
        }),
        RuntimeTransportEvent::V3PlanningProgress {
            principal,
            workspace,
            task_id,
            ui_thread_id,
            agent_id,
            phase,
            detail,
            ..
        } => Some(PlanningChatUpdate {
            principal: principal.clone(),
            workspace: workspace.clone(),
            task_id: task_id.clone(),
            agent_id: agent_id.clone(),
            ui_thread_id: ui_thread_id.clone(),
            status: "planning".to_string(),
            summary: detail
                .clone()
                .unwrap_or_else(|| format!("Planning phase completed: {}.", phase)),
        }),
        // Phase H8.3 — canonical `HitlRequested` / `HitlResolved` arms
        // for planning clarifications. Re-enables the chat-status update
        // that the H7.3 retirement of `V3PlanningClarificationNeeded` /
        // `V3PlanningClarificationResolved` removed. `task_id`,
        // `agent_id`, `principal`, `workspace` come from the canonical
        // envelope. `ui_thread_id` defaults to the user's main thread
        // since canonical envelopes don't carry it (this listener uses
        // the chat store to resolve from there).
        RuntimeTransportEvent::HitlRequested {
            correlation_id: _,
            source,
            prompt,
            input_schema,
            task_id,
            agent_id,
            principal,
            workspace,
            execution_id,
            ..
        } if source == "clarification" => {
            // Scope-enrichment fills `principal` / `workspace` from the
            // registered execution scope; require those. `task_id` falls
            // back to `execution_id` (V3 invariant `task_id ==
            // workflow_id == execution_id`). `agent_id` may be absent on
            // the ask-loop helper emit; default to `DEFAULT_AGENT_ID`.
            // `ui_thread_id` rides on `input_schema.ui_thread_id` for V3
            // emits (`emit_v3_planning_clarification_needed`); fall back
            // to the default thread for V2 ask-loop emits that don't
            // carry it.
            let principal = principal.clone()?;
            let workspace = workspace.clone()?;
            let task_id_value = task_id.clone().or_else(|| execution_id.clone())?;
            let agent_id_value = agent_id
                .clone()
                .unwrap_or_else(|| super::DEFAULT_AGENT_ID.to_string());
            let ui_thread_id = input_schema
                .as_ref()
                .and_then(|schema| schema.get("ui_thread_id"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .unwrap_or_else(crate::magician_v2::storage::task_models::default_ui_thread_id);
            let preview = prompt.trim();
            let summary = if preview.is_empty() {
                "Planner needs clarification before it can finish.".to_string()
            } else {
                let truncated = if preview.chars().count() > 160 {
                    format!(
                        "{}…",
                        preview.chars().take(160).collect::<String>().trim_end()
                    )
                } else {
                    preview.to_string()
                };
                format!("Planner needs clarification: {truncated}")
            };
            Some(PlanningChatUpdate {
                principal,
                workspace,
                task_id: task_id_value,
                agent_id: agent_id_value,
                ui_thread_id,
                status: "planning".to_string(),
                summary,
            })
        },
        RuntimeTransportEvent::HitlResolved {
            correlation_id: _,
            source,
            outcome,
            task_id,
            agent_id,
            principal,
            workspace,
            execution_id,
            ..
        } if source == "clarification" => {
            // Scope-enrichment fills `principal` / `workspace` from the
            // registered execution scope; require those. `task_id` falls
            // back to `execution_id` (V3 invariant `task_id ==
            // workflow_id == execution_id`). `agent_id` may be absent
            // on the ask-loop emit; default to `DEFAULT_AGENT_ID` so the
            // chat session lookup still works.
            let principal = principal.clone()?;
            let workspace = workspace.clone()?;
            let task_id_value = task_id.clone().or_else(|| execution_id.clone())?;
            let agent_id_value = agent_id
                .clone()
                .unwrap_or_else(|| super::DEFAULT_AGENT_ID.to_string());
            let summary = match outcome.as_str() {
                "cancelled" | "dismissed" => {
                    "Planner withdrew a clarification question — planning is reorganising."
                        .to_string()
                },
                "expired" => "Planning clarification timed out without a response.".to_string(),
                _ => "Planning response received. Resuming plan generation.".to_string(),
            };
            Some(PlanningChatUpdate {
                principal,
                workspace,
                task_id: task_id_value,
                agent_id: agent_id_value,
                ui_thread_id: crate::magician_v2::storage::task_models::default_ui_thread_id(),
                status: "planning".to_string(),
                summary,
            })
        },
        RuntimeTransportEvent::V3PlanningCompleted {
            principal,
            workspace,
            task_id,
            ui_thread_id,
            agent_id,
            ..
        } => Some(PlanningChatUpdate {
            principal: principal.clone(),
            workspace: workspace.clone(),
            task_id: task_id.clone(),
            agent_id: agent_id.clone(),
            ui_thread_id: ui_thread_id.clone(),
            status: "draft".to_string(),
            summary: "Plan ready for review.".to_string(),
        }),
        RuntimeTransportEvent::V3PlanningFailed {
            principal,
            workspace,
            task_id,
            ui_thread_id,
            agent_id,
            error,
            ..
        } => Some(PlanningChatUpdate {
            principal: principal.clone(),
            workspace: workspace.clone(),
            task_id: task_id.clone(),
            agent_id: agent_id.clone(),
            ui_thread_id: ui_thread_id.clone(),
            status: "failed".to_string(),
            summary: format!("Planning failed: {}", error),
        }),
        _ => None,
    }
}

/// Pick the `execution_id` from the event so the listener can look up
/// the corresponding canonical scope on the broadcaster. Covers the
/// V3 planning lifecycle variants (which carry `execution_id` directly)
/// plus canonical HITL envelopes whose `execution_id` may have been
/// populated by emit-site logic or scope enrichment.
fn execution_id_for_event(event: &RuntimeTransportEvent) -> Option<&str> {
    match event {
        RuntimeTransportEvent::V3PlanningStarted { task_id, .. }
        | RuntimeTransportEvent::V3PlanningProgress { task_id, .. }
        | RuntimeTransportEvent::V3PlanningCompleted { task_id, .. }
        | RuntimeTransportEvent::V3PlanningFailed { task_id, .. } => Some(task_id),
        RuntimeTransportEvent::HitlRequested { execution_id, .. } => execution_id.as_deref(),
        RuntimeTransportEvent::HitlResolved { execution_id, .. } => execution_id.as_deref(),
        _ => None,
    }
}

async fn find_or_create_session_for_thread(
    principal: &str,
    workspace: &str,
    ui_thread_id: &str,
    agent_id: &str,
    chat_store: &dyn ChatStore,
) -> Option<String> {
    let sessions = match chat_store.list_sessions(principal, workspace).await {
        Ok(sessions) => sessions,
        Err(error) => {
            warn!(
                error = %error,
                principal = principal,
                workspace = workspace,
                "[PLANNING-LISTENER] Failed to list chat sessions for thread"
            );
            return None;
        },
    };

    if let Some(active) = sessions.iter().find(|session| {
        session.status == ChatSessionStatus::Active && session.ui_thread_id == ui_thread_id
    }) {
        return Some(active.id.clone());
    }

    match chat_store
        .get_or_create_active_session(
            principal,
            workspace,
            ui_thread_id,
            &ChatChannel::web(),
            agent_id,
        )
        .await
    {
        Ok(session) => Some(session.id),
        Err(error) => {
            warn!(
                error = %error,
                principal = principal,
                workspace = workspace,
                ui_thread_id = ui_thread_id,
                "[PLANNING-LISTENER] Failed to create planning chat session"
            );
            None
        },
    }
}

async fn session_origin_channel(
    chat_store: &dyn ChatStore,
    session_id: &str,
) -> Option<ChatChannel> {
    chat_store
        .get_session(session_id)
        .await
        .ok()
        .flatten()
        .map(|session| session.origin_channel)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::chat::storage::{ChatStore, FileChatStore};

    #[test]
    fn planning_update_preserves_agent_for_started_events() {
        let event = RuntimeTransportEvent::V3PlanningStarted {
            principal: "principal".to_string(),
            workspace: "workspace".to_string(),
            task_id: "task-1".to_string(),
            task_title: "Plan task".to_string(),
            agent_id: "specialist-agent".to_string(),
            plan_id: "plan-1".to_string(),
            ui_thread_id: "thread-1".to_string(),
            timestamp: 1,
        };

        let update = planning_update_from_event(&event).expect("planning update");

        assert_eq!(update.agent_id, "specialist-agent");
        assert_eq!(update.status, "planning");
    }

    #[tokio::test]
    async fn find_or_create_session_for_thread_reuses_existing_thread_session() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(tmp.path());

        let existing = store
            .get_or_create_active_session(
                "principal",
                "workspace",
                "thread-1",
                &ChatChannel::web(),
                "agent-a",
            )
            .await
            .expect("existing session");

        let session_id = find_or_create_session_for_thread(
            "principal",
            "workspace",
            "thread-1",
            "agent-b",
            &store,
        )
        .await
        .expect("existing session reused");

        assert_eq!(session_id, existing.id);
    }

    #[tokio::test]
    async fn find_or_create_session_for_thread_creates_requested_agent_when_thread_is_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = FileChatStore::new(tmp.path());

        let session_id = find_or_create_session_for_thread(
            "principal",
            "workspace",
            "thread-1",
            "agent-b",
            &store,
        )
        .await
        .expect("new session");

        let session = store
            .get_session(&session_id)
            .await
            .expect("load session")
            .expect("session exists");
        assert_eq!(session.agent_id, "agent-b");
    }
}
