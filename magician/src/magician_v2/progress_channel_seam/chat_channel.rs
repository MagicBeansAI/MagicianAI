use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::RwLock;
use tracing::warn;
use uuid::Uuid;

use crate::magician_v2::{
    artifact_v2::{ArtifactV2Service, ScopeRef, V3ReadApi},
    chat::{
        models::{
            ChatMessage, ChatMessageContent, ChatMessageDirection, ChatSessionStatus,
            ContentBlockRecord,
        },
        service::{load_task_user_summary, project_task_outputs_for_chat_session},
        storage::ChatStore,
    },
    realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
};

use crate::magician_v2::progress_channel_seam::{
    channel::ProgressChannel,
    surface_routing::{chat_render_kind, chat_surface_renders_agent_event},
    types::{ProgressMessage, ProgressMessageKind, Subscription},
};
use crate::magician_v2::realtime_events::{ChatRenderKind, RuntimeAgentEventType};

const PROGRESS_UPDATE_COOLDOWN_MS: i64 = 8_000;
const MAX_INLINE_DELEGATE_SUMMARY_CHARS: usize = 280;

/// Subscription metadata key that carries a canonical taxonomy
/// `event_type` string. Used by emit sites to tag a subscription with
/// the context it represents (`chat.delegate.status_changed`,
/// `chat.agent_turn.status_changed`, `task.status_changed`, …);
/// `effective_event_type` resolves it to the master
/// `GAUI_EVENT_TAXONOMY` row that drives every chat delivery
/// decision.
const CHAT_DELIVERY_KIND_KEY: &str = "chat_delivery_kind";

/// Render-data keys carried alongside `chat_delivery_kind` for the
/// chat channel to use when producing the right content. These are
/// pure parameters (pack name to label the card, tool_call_id to look
/// up output files, agent_id to format the delegate-completion line)
/// — visibility / shape decisions live on the taxonomy row, not here.
// Retired alongside the delegate-completion pill — kept as a comment
// breadcrumb so anyone re-introducing a delegate-status chat surface
// knows the metadata key the upstream emit sites use:
// `chat_inline_delegate_agent_id` lives on the subscription metadata
// that chat/service.rs::dispatch_delegate_to_agent builds.
const CHAT_INLINE_DELEGATE_AGENT_ID_KEY: &str = "chat_inline_delegate_agent_id";
const CHAT_AGENT_TURN_AGENT_ID_KEY: &str = "chat_agent_turn_agent_id";
const CHAT_TASK_DISPLAY_LABEL_KEY: &str = "chat_task_display_label";

#[derive(Clone)]
pub struct ChatChannel {
    chat_store: Arc<dyn ChatStore>,
    event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    v3_service: Arc<ArtifactV2Service>,
    last_progress_emit_at: Arc<RwLock<HashMap<String, i64>>>,
}

impl ChatChannel {
    pub fn new(
        chat_store: Arc<dyn ChatStore>,
        event_broadcaster: Arc<RuntimeTransportBroadcaster>,
        v3_service: Arc<ArtifactV2Service>,
    ) -> Self {
        Self {
            chat_store,
            event_broadcaster,
            v3_service,
            last_progress_emit_at: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn append_message(
        &self,
        session_id: &str,
        content: ChatMessageContent,
        chat_turn_id: Option<&str>,
    ) -> anyhow::Result<()> {
        // Single-bus model: this method only emits the event. The
        // chat-store write is owned by `ChatStoreSink` which subscribes
        // to `ChatMessageReceived` events. Removes the dual-write that
        // used to exist here (both `chat_store.append_message` AND
        // `emit_transport_only`), so the bus is the sole entry point
        // for chat-channel-synthesized system messages
        // (TaskStatusUpdate / PackProgress / escalation_resolved /
        // delegate-status text rows).
        let session = self
            .chat_store
            .get_session(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("chat session not found: {session_id}"))?;
        if session.status != ChatSessionStatus::Active {
            anyhow::bail!("chat session {} is not active", session_id);
        }
        let origin_channel = Some(session.origin_channel.clone());
        let message = ChatMessage::new(
            Uuid::new_v4().to_string(),
            session_id.to_string(),
            ChatMessageDirection::System,
            content,
            Utc::now().timestamp_millis(),
        )
        // Preserve the spawning turn carried by chat-originated task
        // subscriptions. The web client uses this correlation to keep
        // the correct activity card live after the hand-off response,
        // rather than guessing that every active historical task belongs
        // to the session's newest user message.
        .with_chat_turn_id(chat_turn_id.map(str::to_string))
        .with_voice_origin(None)
        .with_speech_segments(None)
        .with_source_surface(None)
        .with_presence_session_id(None);
        self.event_broadcaster
            .emit_transport_only(RuntimeTransportEvent::ChatMessageReceived {
                session_id: session_id.to_string(),
                message,
                principal: Some(session.principal),
                workspace: Some(session.workspace),
                origin_channel,
                timestamp: chrono::Utc::now().timestamp_millis(),
            });
        Ok(())
    }

    fn tracking_key(message: &ProgressMessage) -> String {
        message
            .root_task_id
            .clone()
            .or_else(|| message.execution_id.clone())
            .unwrap_or_else(|| message.id.clone())
    }

    fn task_message_key(message: &ProgressMessage) -> String {
        message
            .root_task_id
            .clone()
            .or_else(|| message.task_id.clone())
            .or_else(|| message.execution_id.clone())
            .unwrap_or_else(|| message.id.clone())
    }

    fn execution_message_key(message: &ProgressMessage) -> Option<String> {
        // Prefer the task's ROOT execution (consistent with
        // `task_message_key` / `tracking_key`, which both prefer the
        // `root_*` id). A nested child execution — e.g. a chat-pack run
        // that delegates to a sub-agent, or any task whose root spawns a
        // child — would otherwise stamp its CHILD `execution_id` onto the
        // chat status card. Mixed execution_ids split one task's card into
        // separate `task::execution` groups in the UI and leave the
        // deep-panel "Inspect run →" target ambiguous (root vs child).
        // Fall back to the event's own `execution_id` when no root is set
        // (the root execution's own events / non-nested tasks).
        message
            .root_execution_id
            .clone()
            .or_else(|| message.execution_id.clone())
    }

    fn progress_summary(message: &ProgressMessage) -> Option<String> {
        match &message.kind {
            ProgressMessageKind::ActionProgress {
                iteration,
                action_type,
                target,
                success,
                error,
            } => {
                if *success {
                    Some(format!(
                        "Iteration {}: {} {}",
                        iteration, action_type, target
                    ))
                } else if let Some(error) = error.as_ref() {
                    Some(format!(
                        "Iteration {}: {} {} failed ({})",
                        iteration, action_type, target, error
                    ))
                } else {
                    Some(format!(
                        "Iteration {}: {} {} failed",
                        iteration, action_type, target
                    ))
                }
            },
            _ => None,
        }
    }

    fn default_terminal_status_summary(status: &str) -> String {
        match status {
            "completed" => "Execution completed.".to_string(),
            "failed" => "Execution failed.".to_string(),
            "cancelled" => "Execution cancelled.".to_string(),
            other => format!("Execution ended with status {other}."),
        }
    }

    fn normalize_inline_summary_whitespace(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn truncate_inline_summary(text: &str, max_chars: usize) -> String {
        if text.chars().count() <= max_chars {
            return text.to_string();
        }

        let candidate: String = text.chars().take(max_chars).collect();
        if let Some(last_space) = candidate.rfind(' ') {
            if last_space > max_chars / 2 {
                return format!("{}...", candidate[..last_space].trim_end());
            }
        }

        format!("{}...", candidate.trim_end())
    }

    fn take_inline_summary_sentences(text: &str, count: usize) -> String {
        let mut sentences = Vec::new();
        let mut remaining = text.trim();

        while !remaining.is_empty() && sentences.len() < count {
            if let Some(separator_index) = remaining.find(". ") {
                sentences.push(remaining[..=separator_index].trim().to_string());
                remaining = remaining[separator_index + 2..].trim();
            } else {
                sentences.push(remaining.to_string());
                break;
            }
        }

        sentences.join(" ")
    }

    fn compact_inline_delegate_summary(status: &str, summary: &str) -> Option<String> {
        let generic_terminal_summary = Self::default_terminal_status_summary(status);
        let normalized = Self::normalize_inline_summary_whitespace(summary);
        if normalized.is_empty() || normalized == generic_terminal_summary {
            return None;
        }

        let mut candidate = normalized;
        let generic_with_space = format!("{generic_terminal_summary} ");
        if let Some(stripped) = candidate.strip_prefix(&generic_with_space) {
            candidate = stripped.trim().to_string();
        }

        let execution_prefix = format!("Execution {status}:");
        if let Some(stripped) = candidate.strip_prefix(&execution_prefix) {
            candidate = stripped.trim().to_string();
        }

        if candidate.is_empty() {
            return None;
        }

        Some(Self::truncate_inline_summary(
            &Self::take_inline_summary_sentences(&candidate, 2),
            MAX_INLINE_DELEGATE_SUMMARY_CHARS,
        ))
    }

    fn stale_or_generic_terminal_summary(status: &str, summary: &str) -> bool {
        let normalized = Self::normalize_inline_summary_whitespace(summary);
        if normalized.is_empty() || normalized == Self::default_terminal_status_summary(status) {
            return true;
        }
        matches!(
            normalized.to_ascii_lowercase().as_str(),
            "execution is running."
                | "execution is running"
                | "execution running."
                | "execution running"
                | "execution started."
                | "execution started"
                | "execution is ready."
                | "execution is ready"
                | "task is running."
                | "task is running"
        )
    }

    async fn terminal_status_summary(
        &self,
        session_id: &str,
        task_id: &str,
        status: &str,
        summary: &Option<String>,
    ) -> Option<String> {
        if let Some(summary) = summary
            .as_deref()
            .map(str::trim)
            .filter(|value| !Self::stale_or_generic_terminal_summary(status, value))
        {
            return Some(summary.to_string());
        }

        if let Ok(Some(session)) = self.chat_store.get_session(session_id).await {
            if let Some(task_summary) = load_task_user_summary(
                self.v3_service.workspace(),
                &session.principal,
                &session.workspace,
                task_id,
            )
            .await
            {
                return Some(Self::truncate_inline_summary(&task_summary, 6_000));
            }
        }

        Some(Self::default_terminal_status_summary(status))
    }

    /// Resolves the effective `event_type` for a delivery decision —
    /// the value used to look up render hints in the master taxonomy.
    ///
    /// Precedence (high → low):
    ///   1. **Subscription `chat_delivery_kind` override** — wins when
    ///      set. The value is a canonical taxonomy event_type string
    ///      (`chat.delegate.status_changed`, `chat.agent_turn.status_changed`,
    ///      `task.status_changed`, …). The subscription knows the
    ///      *context* the message arrived in (a delegate execution
    ///      streams its `StatusChanged` events as the generic
    ///      `task.status_changed` synthetic, but the chat needs to
    ///      render them as `chat.delegate.status_changed`).
    ///      Subscriptions filter on specific execution_ids via
    ///      `SubscriptionFilter`, so the override doesn't leak across
    ///      unrelated traffic.
    ///   2. `message.event_type` set at the emit site — for messages
    ///      whose own emit site already chose the right canonical key
    ///      (AGUI envelope events).
    ///   3. `None` — the per-`ProgressMessageKind` fallback below
    ///      handles legacy emit sites that haven't set event_type.
    fn effective_event_type(
        subscription: &Subscription,
        message: &ProgressMessage,
    ) -> Option<String> {
        if let Some(override_kind) = subscription
            .metadata
            .get(CHAT_DELIVERY_KIND_KEY)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            return Some(override_kind.to_string());
        }
        message.event_type.as_deref().map(str::to_string)
    }

    // `is_inline_delegate` removed alongside the delegate-completion
    // pill — was its only caller. The activity card discriminates on
    // event_type directly via `task.status_changed` /
    // `chat.delegate.status_changed`. If a future surface re-needs the
    // discriminator, add it back as:
    //   fn is_inline_delegate(subscription, message) -> bool {
    //       Self::effective_event_type(subscription, message)
    //           .as_deref()
    //           .and_then(RuntimeAgentEventType::from_str)
    //           == Some(RuntimeAgentEventType::ChatDelegateStatusChanged)
    //   }

    fn is_agent_turn(subscription: &Subscription, message: &ProgressMessage) -> bool {
        Self::effective_event_type(subscription, message)
            .as_deref()
            .and_then(RuntimeAgentEventType::from_str)
            == Some(RuntimeAgentEventType::ChatAgentTurnStatusChanged)
    }

    async fn task_output_files(
        &self,
        session_id: &str,
        task_id: &str,
        _status: &str,
    ) -> Vec<ContentBlockRecord> {
        // Phase 2 — incremental output projection (Gap B). The
        // terminal-only gate (`status == completed/failed/cancelled`)
        // is gone: every `StatusChanged` event now triggers a
        // projection pass. `project_task_outputs_for_chat_session` is
        // idempotent (dedupes by `source_task_id` +
        // `source_task_output_id` against existing file_index rows),
        // so repeated calls during a task's lifetime cost a
        // get_task_outputs lookup but never duplicate-copy a file or
        // duplicate a file_index row. Files generated mid-task
        // therefore surface on the next `running` event the task
        // emits, instead of waiting for terminal status. The
        // `_status` parameter is preserved on the signature for
        // future per-status policy (e.g. skipping projection on
        // `pending`); leading underscore documents that it's
        // currently unused.
        let session = match self.chat_store.get_session(session_id).await {
            Ok(Some(session)) => session,
            Ok(None) => return Vec::new(),
            Err(error) => {
                warn!(
                    "[CHAT-PROGRESS] Failed to resolve session {} for task output enrichment: {}",
                    session_id, error
                );
                return Vec::new();
            },
        };
        project_task_outputs_for_chat_session(self.v3_service.as_ref(), &session, task_id).await
    }

    /// Whether the task is mid-synthesis — terminal status reached but the
    /// final user-facing result is still being produced. Drives the card's
    /// "Preparing final result…" state. Best-effort: a missing task or read
    /// error reads as not-pending (no spurious indicator).
    async fn task_synthesis_pending(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> bool {
        if task_id.is_empty() {
            return false;
        }
        let scope = ScopeRef::system_internal_unauthenticated(
            &principal.to_string(),
            &workspace.to_string(),
        );
        match self.v3_service.get_task(&scope, task_id).await {
            Ok(task) => !task.state.synthesis_pending_executions.is_empty(),
            Err(_) => false,
        }
    }

    /// The synthesizer-authored off-call spoken summary (`VoiceSpeechSummary.tts`)
    /// for a completed task, read from the `voice_speech.json` sidecar. Drives
    /// the frontend's off-call TTS read-out. `None` when the task authored none.
    async fn task_speech_tts(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> Option<String> {
        if task_id.is_empty() {
            return None;
        }
        let scope = ScopeRef::system_internal_unauthenticated(
            &principal.to_string(),
            &workspace.to_string(),
        );
        self.v3_service
            .read_voice_speech_summary(&scope, task_id)
            .await
            .and_then(|speech| speech.tts)
            .filter(|line| !line.trim().is_empty())
    }
}

#[async_trait]
impl ProgressChannel for ChatChannel {
    fn id(&self) -> &str {
        "chat"
    }

    async fn deliver(
        &self,
        subscription: &Subscription,
        message: &ProgressMessage,
    ) -> anyhow::Result<()> {
        let Some(session_id) = subscription.metadata.get("session_id") else {
            anyhow::bail!("chat subscription missing session_id");
        };
        let chat_turn_id = subscription
            .metadata
            .get("chat_turn_id")
            .map(String::as_str);

        // Single visibility gate driven by the master event taxonomy.
        //
        // `Self::effective_event_type` resolves the canonical event_type
        // for the delivery decision, with subscription `chat_delivery_kind`
        // override winning when set (delegate / agent_turn contexts that
        // need to render generic StatusChanged events as their specific
        // chat shape). `chat_render_kind` looks the result up in
        // `realtime_events::GAUI_EVENT_TAXONOMY` and returns the declared
        // `ChatRenderKind`. We drop here when the taxonomy says
        // `Suppress` so the per-`ProgressMessageKind` rendering logic
        // below doesn't even run for events the chat surface doesn't
        // want.
        //
        // Legacy emit sites that haven't set `event_type` or a
        // subscription override fall through to the per-kind dispatch
        // unchanged.
        if let Some(event_type) = Self::effective_event_type(subscription, message) {
            if matches!(chat_render_kind(&event_type), ChatRenderKind::Suppress) {
                return Ok(());
            }
            // HITL events (`hitl.requested` / `hitl.resolved`) are
            // owned by `EscalationListener` for the chat surface — it
            // renders the interactive `EscalationCard` /
            // `EscalationResolved` content variants (with buttons,
            // options, question text) directly to the chat store.
            // Skip here so chat-spawned executions whose chat
            // subscription catches the same event don't render a
            // duplicate `Text` row alongside the listener's
            // interactive card. The taxonomy's `chat_kind: Text` is
            // the *target* shape — when the listener is retired (or
            // chat takes over EscalationCard rendering), this skip
            // drops and the taxonomy flips to `EscalationCard`.
            if event_type.starts_with("hitl.") {
                return Ok(());
            }
        }

        let periodic_updates = subscription
            .metadata
            .get("periodic_updates")
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        match &message.kind {
            ProgressMessageKind::StatusChanged { status, summary } => {
                let task_id = Self::task_message_key(message);
                let summary = if matches!(status.as_str(), "completed" | "failed" | "cancelled") {
                    self.terminal_status_summary(session_id, &task_id, status, summary)
                        .await
                } else {
                    summary.clone()
                };
                // Phase 3.5 — `inline_pack` arm removed. Chat-pack
                // dispatch now creates real V3 tasks (see
                // `dispatch_capability_pack` in chat/service.rs), so
                // every pack call's status changes flow through the
                // standard `TaskStatusUpdate` rendering below — no
                // separate PackProgress card needed.
                let output_files = self.task_output_files(session_id, &task_id, status).await;
                // While synthesis is in flight the status guard holds the
                // visible status at "running"; surface the real synthesis state
                // so the card can render "Preparing final result…" rather than a
                // bare running/completed state.
                let synthesis_pending = self
                    .task_synthesis_pending(
                        &subscription.principal,
                        &subscription.workspace,
                        &task_id,
                    )
                    .await;
                // On the terminal-success card (synthesis has landed), carry the
                // synthesizer-authored off-call spoken summary so the frontend
                // can read it aloud when no live voice call is running (a live
                // call instead speaks `speech_live` over the voice channel).
                let speech_tts = if status == "completed" && !synthesis_pending {
                    self.task_speech_tts(&subscription.principal, &subscription.workspace, &task_id)
                        .await
                } else {
                    None
                };
                let display_label = subscription
                    .metadata
                    .get(CHAT_TASK_DISPLAY_LABEL_KEY)
                    .cloned();
                // Release the task-id chat fan-out only when the
                // terminal status is also content-final. A terminal
                // task with synthesis still pending can still emit the
                // user-facing answer and output-ready signal, so
                // detaching here would hide the most important update.
                if matches!(status.as_str(), "completed" | "failed" | "cancelled")
                    && !task_id.is_empty()
                    && !synthesis_pending
                {
                    self.event_broadcaster
                        .unregister_chat_fanout_for_task(&task_id, session_id);
                }
                self.append_message(
                    session_id,
                    ChatMessageContent::TaskStatusUpdate {
                        task_id,
                        status: status.clone(),
                        display_label: display_label.clone(),
                        summary: summary.clone(),
                        execution_id: Self::execution_message_key(message),
                        ui_thread_id: message.ui_thread_id.clone(),
                        output_files: output_files.clone(),
                        synthesis_pending,
                        speech_tts: speech_tts.clone(),
                    },
                    chat_turn_id,
                )
                .await?;
                // Pill retired: the per-turn `RequestActivityCard`
                // now renders delegate terminal status as a leaf row
                // (see `task.status_changed` / `chat.delegate.status_changed`
                // handlers in RequestActivityCard.svelte::applyEvent).
                // Keeping the `TaskStatusUpdate` card above is enough —
                // the activity card subscribes to the same event with
                // `chat_turn_id` stamped via fanout and shows
                // `<agent> completed` / `<agent> failed` inline.
                if Self::is_agent_turn(subscription, message)
                    && matches!(status.as_str(), "completed" | "failed" | "cancelled")
                {
                    let agent_id = subscription
                        .metadata
                        .get(CHAT_AGENT_TURN_AGENT_ID_KEY)
                        .map(String::as_str)
                        .unwrap_or("agent");
                    let rendered_summary_owned = summary
                        .clone()
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| Self::default_terminal_status_summary(status));
                    let compacted_summary =
                        Self::compact_inline_delegate_summary(status, &rendered_summary_owned);
                    let text = match status.as_str() {
                        "completed" => compacted_summary
                            .map(|summary| format!("Done. {summary}"))
                            .unwrap_or_else(|| format!("Agent '{agent_id}' completed.")),
                        "cancelled" => compacted_summary
                            .map(|summary| format!("Cancelled. {summary}"))
                            .unwrap_or_else(|| format!("Agent '{agent_id}' was cancelled.")),
                        "failed" => compacted_summary
                            .map(|summary| format!("I could not complete that. {summary}"))
                            .unwrap_or_else(|| {
                                format!("Agent '{agent_id}' could not complete that.")
                            }),
                        other => compacted_summary
                            .map(|summary| {
                                format!("Agent '{agent_id}' ended with {other}. {summary}")
                            })
                            .unwrap_or_else(|| format!("Agent '{agent_id}' ended with {other}.")),
                    };
                    self.append_message(
                        session_id,
                        ChatMessageContent::Text {
                            text,
                            plan_reply: None,
                        },
                        chat_turn_id,
                    )
                    .await?;
                }

                // Mirror the status change onto the transport bus so the
                // per-turn `RequestActivityCard` (subscribed via
                // `/events?chat_turn_id=…`) can render a lifecycle row
                // ("Vera completed", "Muse failed") inline. Without this
                // emit, the progress-router path drops the signal into
                // the chat-store as a TaskStatusUpdate card but never
                // touches `events.jsonl` — the activity card never sees
                // it. We only emit when the subscription was registered
                // with a `chat_turn_id` (the delegate/handover paths from
                // chat/service.rs include it; non-chat subscribers don't).
                if let Some(turn_id) = subscription.metadata.get("chat_turn_id") {
                    let event_type = subscription
                        .metadata
                        .get(CHAT_DELIVERY_KIND_KEY)
                        .map(String::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .unwrap_or_else(|| RuntimeAgentEventType::TaskStatusChanged.as_str());
                    let target_agent_id = subscription
                        .metadata
                        .get(CHAT_INLINE_DELEGATE_AGENT_ID_KEY)
                        .or_else(|| subscription.metadata.get(CHAT_AGENT_TURN_AGENT_ID_KEY))
                        .cloned()
                        .or_else(|| message.agent_id.clone())
                        .unwrap_or_default();
                    // `event_id` is stamped here so the per-turn SSE
                    // handler's backfill/live dedupe can match the same
                    // event between the on-disk projection and the live
                    // broadcast. Without it, two delegate status events
                    // landing in the same millisecond would dedupe to
                    // the same composite key (event_type+timestamp+agent)
                    // and the UI would drop one. The id is per logical
                    // event, not per recipient: chat-fanout produces a
                    // primary + N clones with the same id so the per-
                    // turn projection writes exactly one row.
                    let payload = serde_json::json!({
                        "event_id": uuid::Uuid::new_v4().to_string(),
                        "chat_turn_id": turn_id,
                        "task_id": Self::task_message_key(message),
                        "execution_id": Self::execution_message_key(message),
                        "status": status,
                        "summary": summary,
                        "output_files": output_files,
                        "synthesis_pending": synthesis_pending,
                        "speech_tts": speech_tts,
                        "display_label": display_label,
                        "target_agent_id": target_agent_id,
                        "chat_inline_delegate_agent_id": target_agent_id,
                    });
                    // Use the chat scope (not the delegate's) so the
                    // event lands in the chat session's per-scope
                    // events.jsonl, alongside the other turn events.
                    self.event_broadcaster.emit_named(
                        event_type,
                        &target_agent_id,
                        Some(&subscription.principal),
                        Some(&subscription.workspace),
                        payload,
                    );
                }
            },
            ProgressMessageKind::ChildStatusChanged {
                status, summary, ..
            } => {
                self.append_message(
                    session_id,
                    ChatMessageContent::TaskStatusUpdate {
                        task_id: Self::task_message_key(message),
                        status: status.clone(),
                        display_label: subscription
                            .metadata
                            .get(CHAT_TASK_DISPLAY_LABEL_KEY)
                            .cloned(),
                        summary: summary.clone(),
                        execution_id: Self::execution_message_key(message),
                        ui_thread_id: message.ui_thread_id.clone(),
                        output_files: Vec::new(),
                        synthesis_pending: false,
                        speech_tts: None,
                    },
                    chat_turn_id,
                )
                .await?;
            },
            ProgressMessageKind::HandedOver {
                from_agent,
                to_agent,
            } => {
                self.append_message(
                    session_id,
                    ChatMessageContent::TaskStatusUpdate {
                        task_id: Self::task_message_key(message),
                        status: "running".to_string(),
                        display_label: subscription
                            .metadata
                            .get(CHAT_TASK_DISPLAY_LABEL_KEY)
                            .cloned(),
                        summary: Some(format!("Handed over from {from_agent} to {to_agent}")),
                        execution_id: Self::execution_message_key(message),
                        ui_thread_id: message.ui_thread_id.clone(),
                        output_files: Vec::new(),
                        synthesis_pending: false,
                        speech_tts: None,
                    },
                    chat_turn_id,
                )
                .await?;
            },
            ProgressMessageKind::ActionProgress { .. } => {
                if !periodic_updates {
                    return Ok(());
                }
                let tracking_key = Self::tracking_key(message);
                let now = Utc::now().timestamp_millis();
                let should_emit = {
                    let guard = self.last_progress_emit_at.read().await;
                    guard
                        .get(&tracking_key)
                        .map(|last| now - *last >= PROGRESS_UPDATE_COOLDOWN_MS)
                        .unwrap_or(true)
                };
                if !should_emit {
                    return Ok(());
                }
                self.last_progress_emit_at
                    .write()
                    .await
                    .insert(tracking_key, now);

                // Phase 3.5 — `inline_pack` ActionProgress arm removed.
                // Subprocess stderr lines from chat-pack now stamp the
                // V3 task_id and flow into the chat-task-<task_id>-<session_id> activity
                // card; the chat surface still gets a TaskStatusUpdate
                // running card via the path below.
                self.append_message(
                    session_id,
                    ChatMessageContent::TaskStatusUpdate {
                        task_id: Self::task_message_key(message),
                        status: "running".to_string(),
                        display_label: subscription
                            .metadata
                            .get(CHAT_TASK_DISPLAY_LABEL_KEY)
                            .cloned(),
                        summary: Self::progress_summary(message),
                        execution_id: Self::execution_message_key(message),
                        ui_thread_id: message.ui_thread_id.clone(),
                        output_files: Vec::new(),
                        synthesis_pending: false,
                        speech_tts: None,
                    },
                    chat_turn_id,
                )
                .await?;
            },
            ProgressMessageKind::AgentNotification {
                event_type,
                message,
                ..
            } => {
                // Visibility decision is delegated to the chat-surface
                // predicate in `progress_channel_seam::surface_routing`,
                // which is itself a thin policy expressed over the
                // master event taxonomy (`category`, `severity`,
                // `user_relevant`) in `realtime_events`. The chat
                // channel does NOT maintain its own list of event_type
                // strings — adding a new event in the master table is
                // enough; every surface (chat, future WhatsApp /
                // Telegram / push channels) derives its policy from
                // the same source.
                if !chat_surface_renders_agent_event(event_type) {
                    return Ok(());
                }
                self.append_message(
                    session_id,
                    ChatMessageContent::Text {
                        text: message.clone(),
                        plan_reply: None,
                    },
                    chat_turn_id,
                )
                .await?;
            },
        }

        Ok(())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::chat::models::{ChatMessageContent, ContentBlockRecord};
    use crate::magician_v2::chat::storage::ChatStore as _;
    use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};
    use std::{collections::HashMap, sync::Arc, time::Duration};

    use chrono::Utc;

    use crate::magician_v2::chat::models::ContentFileSource;
    use crate::magician_v2::progress_channel_seam::*;
    use crate::magician_v2::{
        artifact_v2::{
            models::{OutputRef, TaskOutputMode},
            CreateTaskInput, ScopeRef,
        },
        chat::models::ChatChannel as SessionOriginChannel,
        progress_channel_seam::types::{
            ProgressSeverity, ProgressSource, Subscription, SubscriptionFilter, SubscriptionSource,
        },
        test_support::build_test_artifact_v2_service,
    };

    /// Stand-in for `ChatStoreSink` in tests. After the single-bus
    /// migration, `ChatChannel::append_message` only emits a
    /// `ChatMessageReceived` event — the actual `chat_store.append_message`
    /// write is owned by `ChatStoreSink` in production. Tests that
    /// assert on `chat_store.get_messages` therefore need to wire up a
    /// minimal subscriber that mirrors the sink's projection.
    /// Returns a `JoinHandle` so the test can keep the drain alive.
    fn spawn_chat_store_drain(
        broadcaster: &Arc<RuntimeTransportBroadcaster>,
        chat_store: Arc<dyn crate::magician_v2::chat::storage::ChatStore>,
    ) -> tokio::task::JoinHandle<()> {
        // Subscribe synchronously before spawning so events emitted in
        // the spawn-to-poll race window land in the receiver buffer
        // instead of vanishing. Same fix as the production sinks.
        let mut rx = broadcaster.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(RuntimeTransportEvent::ChatMessageReceived {
                        session_id,
                        message,
                        ..
                    }) => {
                        let _ = chat_store.append_message(&session_id, message).await;
                    },
                    Ok(_) => {},
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    }

    /// Wait for the in-test chat-store drain to catch up to N messages
    /// before asserting. The drain runs in a spawned task so there is
    /// no synchronous guarantee that the broadcast emit is observable
    /// in chat_store before the next line of the test executes.
    async fn wait_for_message_count(
        chat_store: &Arc<crate::magician_v2::chat::storage::FileChatStore>,
        session_id: &str,
        expected: usize,
    ) -> Vec<crate::magician_v2::chat::models::ChatMessage> {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let messages = {
                use crate::magician_v2::chat::storage::ChatStore as _;
                chat_store
                    .get_messages(session_id, 100)
                    .await
                    .expect("messages")
            };
            if messages.len() >= expected || std::time::Instant::now() >= deadline {
                return messages;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[test]
    fn compact_inline_delegate_summary_omits_generic_terminal_text() {
        assert_eq!(
            ChatChannel::compact_inline_delegate_summary("completed", "Execution completed."),
            None
        );
    }

    #[test]
    fn compact_inline_delegate_summary_keeps_only_useful_details() {
        assert_eq!(
            ChatChannel::compact_inline_delegate_summary(
                "completed",
                "Execution completed: goal_achieved — Located the latest receipts in Gmail. Test results: omitted"
            ),
            Some(
                "goal_achieved — Located the latest receipts in Gmail. Test results: omitted"
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn deliver_preserves_origin_channel_and_chat_turn_for_bot_sessions() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let chat_store = Arc::new(crate::magician_v2::chat::storage::FileChatStore::new(
            temp_dir.path(),
        ));
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let v3_service = build_test_artifact_v2_service(temp_dir.path());
        let progress_channel =
            ChatChannel::new(chat_store.clone(), broadcaster.clone(), v3_service);
        let _drain = spawn_chat_store_drain(&broadcaster, chat_store.clone());

        let session = chat_store
            .get_or_create_active_session(
                "principal-1",
                "workspace-bot",
                "general",
                &SessionOriginChannel::new("telegram", "42"),
                "agent",
            )
            .await
            .expect("chat session");

        let mut metadata = HashMap::new();
        metadata.insert("session_id".to_string(), session.id.clone());
        metadata.insert("chat_turn_id".to_string(), "chat-turn-1".to_string());
        let subscription = Subscription {
            id: "sub-1".to_string(),
            channel_id: "chat".to_string(),
            filter: SubscriptionFilter::TaskId("task-1".to_string()),
            principal: "principal-1".to_string(),
            workspace: "workspace-bot".to_string(),
            min_severity: ProgressSeverity::Trace,
            metadata,
            source: SubscriptionSource::Dynamic,
            retention_secs: 0,
            message_template: None,
            output_severity: None,
            watermark: 0,
            pending_retry: Default::default(),
            created_at: 0,
        };
        let message = ProgressMessage {
            id: "msg-1".to_string(),
            seq: 1,
            log_key: "task:task-1".to_string(),
            source: ProgressSource::Execution,
            event_type: None,
            metadata: Default::default(),
            execution_id: Some("exec-1".to_string()),
            task_id: Some("task-1".to_string()),
            root_task_id: Some("task-1".to_string()),
            root_execution_id: Some("exec-1".to_string()),
            parent_execution_id: None,
            agent_id: Some("delegate-agent".to_string()),
            ui_thread_id: None,
            step_id: None,
            routing_keys: vec!["task/task-1".to_string()],
            principal: "principal-1".to_string(),
            workspace: "workspace-bot".to_string(),
            severity: ProgressSeverity::Info,
            kind: ProgressMessageKind::StatusChanged {
                status: "running".to_string(),
                summary: Some("Execution started".to_string()),
            },
            timestamp: Utc::now().timestamp_millis(),
        };

        let mut receiver = broadcaster.subscribe();
        progress_channel
            .deliver(&subscription, &message)
            .await
            .expect("deliver progress update");

        let stored_messages = wait_for_message_count(&chat_store, &session.id, 1).await;
        assert_eq!(stored_messages.len(), 1);
        assert_eq!(
            stored_messages[0].chat_turn_id.as_deref(),
            Some("chat-turn-1")
        );
        match &stored_messages[0].content {
            ChatMessageContent::TaskStatusUpdate {
                task_id,
                status,
                summary,
                execution_id,
                ui_thread_id,
                output_files,
                ..
            } => {
                assert_eq!(task_id, "task-1");
                assert_eq!(status, "running");
                assert_eq!(summary.as_deref(), Some("Execution started"));
                assert_eq!(execution_id.as_deref(), Some("exec-1"));
                assert_eq!(ui_thread_id, &None);
                assert!(output_files.is_empty());
            },
            other => panic!("unexpected chat message content: {other:?}"),
        }

        let event = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("realtime event timeout")
            .expect("realtime event");
        match event {
            RuntimeTransportEvent::ChatMessageReceived {
                session_id,
                origin_channel,
                ..
            } => {
                assert_eq!(session_id, session.id);
                let origin_channel = origin_channel.expect("origin channel");
                assert_eq!(origin_channel.channel_type, "telegram");
                assert_eq!(origin_channel.address.as_deref(), Some("42"));
            },
            other => panic!("unexpected realtime event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn terminal_status_prefers_message_summary() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let chat_store = Arc::new(crate::magician_v2::chat::storage::FileChatStore::new(
            temp_dir.path(),
        ));
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let v3_service = build_test_artifact_v2_service(temp_dir.path());
        let progress_channel =
            ChatChannel::new(chat_store.clone(), broadcaster.clone(), v3_service);
        let _drain = spawn_chat_store_drain(&broadcaster, chat_store.clone());

        let session = chat_store
            .get_or_create_active_session(
                "principal-1",
                "workspace-a",
                "general",
                &SessionOriginChannel::new("local", "session"),
                "agent",
            )
            .await
            .expect("chat session");

        let mut metadata = HashMap::new();
        metadata.insert("session_id".to_string(), session.id.clone());
        let subscription = Subscription {
            id: "sub-1".to_string(),
            channel_id: "chat".to_string(),
            filter: SubscriptionFilter::TaskId("task-1".to_string()),
            principal: "principal-1".to_string(),
            workspace: "workspace-a".to_string(),
            min_severity: ProgressSeverity::Trace,
            metadata,
            source: SubscriptionSource::Dynamic,
            retention_secs: 0,
            message_template: None,
            output_severity: None,
            watermark: 0,
            pending_retry: Default::default(),
            created_at: 0,
        };
        let projection_message = ProgressMessage {
            id: "msg-projection-1".to_string(),
            seq: 1,
            log_key: "task:task-1".to_string(),
            source: ProgressSource::Execution,
            event_type: None,
            metadata: Default::default(),
            execution_id: Some("exec-root".to_string()),
            task_id: Some("task-1".to_string()),
            root_task_id: Some("task-1".to_string()),
            root_execution_id: Some("exec-root".to_string()),
            parent_execution_id: None,
            agent_id: Some("personal-agent".to_string()),
            ui_thread_id: Some("general".to_string()),
            step_id: None,
            routing_keys: vec!["task/task-1".to_string()],
            principal: "principal-1".to_string(),
            workspace: "workspace-a".to_string(),
            severity: ProgressSeverity::Info,
            kind: ProgressMessageKind::StatusChanged {
                status: "completed".to_string(),
                summary: Some("Projection completion summary".to_string()),
            },
            timestamp: Utc::now().timestamp_millis(),
        };

        progress_channel
            .deliver(&subscription, &projection_message)
            .await
            .expect("deliver");

        let messages = wait_for_message_count(&chat_store, &session.id, 1).await;
        let rendered = messages
            .last()
            .and_then(|message| match &message.content {
                ChatMessageContent::TaskStatusUpdate { summary, .. } => summary.clone(),
                _ => None,
            })
            .expect("task status summary");
        assert_eq!(rendered, "Projection completion summary");
    }

    #[tokio::test]
    async fn terminal_status_replaces_stale_running_summary_with_task_user_output() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let chat_store = Arc::new(crate::magician_v2::chat::storage::FileChatStore::new(
            temp_dir.path(),
        ));
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let v3_service = build_test_artifact_v2_service(temp_dir.path());
        let progress_channel = ChatChannel::new(
            chat_store.clone(),
            broadcaster.clone(),
            Arc::clone(&v3_service),
        );
        let _drain = spawn_chat_store_drain(&broadcaster, chat_store.clone());

        let session = chat_store
            .get_or_create_active_session(
                "principal-1",
                "workspace-a",
                "general",
                &SessionOriginChannel::new("local", "session"),
                "agent",
            )
            .await
            .expect("chat session");

        let task = v3_service
            .create_task(CreateTaskInput {
                principal: "principal-1".to_string(),
                workspace: "workspace-a".to_string(),
                title: "Sales analysis".to_string(),
                description: "Summarize sales".to_string(),
                agent_id: "agent".to_string(),
                goal_id: None,
                ui_thread_id: "general".to_string(),
                priority: None,
                due_date: None,
                tags: Vec::new(),
                created_by: "chat_delegate".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: TaskOutputMode::Accumulate,
                chat_session_id: Some(session.id.clone()),
                lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::Internal,
                sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::Deferred,
            })
            .await
            .expect("create task");
        let outputs_dir = v3_service.workspace().task_outputs_dir(
            "principal-1",
            "workspace-a",
            &task.manifest.task_id,
        );
        tokio::fs::write(
            outputs_dir.join(format!(
                "out_task_user_{}_exec-root.md",
                task.manifest.task_id
            )),
            "Final result: sales were 42 and margin was 7.",
        )
        .await
        .expect("write task user output");

        let mut metadata = HashMap::new();
        metadata.insert("session_id".to_string(), session.id.clone());
        let subscription = Subscription {
            id: "sub-1".to_string(),
            channel_id: "chat".to_string(),
            filter: SubscriptionFilter::TaskId(task.manifest.task_id.clone()),
            principal: "principal-1".to_string(),
            workspace: "workspace-a".to_string(),
            min_severity: ProgressSeverity::Trace,
            metadata,
            source: SubscriptionSource::Dynamic,
            retention_secs: 0,
            message_template: None,
            output_severity: None,
            watermark: 0,
            pending_retry: Default::default(),
            created_at: 0,
        };
        let message = ProgressMessage {
            id: "msg-stale-terminal".to_string(),
            seq: 1,
            log_key: format!("task:{}", task.manifest.task_id),
            source: ProgressSource::Execution,
            event_type: None,
            metadata: Default::default(),
            execution_id: Some("exec-root".to_string()),
            task_id: Some(task.manifest.task_id.clone()),
            root_task_id: Some(task.manifest.task_id.clone()),
            root_execution_id: Some("exec-root".to_string()),
            parent_execution_id: None,
            agent_id: Some("delegate-agent".to_string()),
            ui_thread_id: Some("general".to_string()),
            step_id: None,
            routing_keys: vec![format!("task/{}", task.manifest.task_id)],
            principal: "principal-1".to_string(),
            workspace: "workspace-a".to_string(),
            severity: ProgressSeverity::Info,
            kind: ProgressMessageKind::StatusChanged {
                status: "completed".to_string(),
                summary: Some("Execution is running.".to_string()),
            },
            timestamp: Utc::now().timestamp_millis(),
        };

        progress_channel
            .deliver(&subscription, &message)
            .await
            .expect("deliver");

        let messages = wait_for_message_count(&chat_store, &session.id, 1).await;
        let rendered = messages
            .last()
            .and_then(|message| match &message.content {
                ChatMessageContent::TaskStatusUpdate { summary, .. } => summary.clone(),
                _ => None,
            })
            .expect("task status summary");
        assert_eq!(rendered, "Final result: sales were 42 and margin was 7.");
    }

    #[tokio::test]
    async fn projection_execution_progress_notifications_are_not_rendered_as_chat_text() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let chat_store = Arc::new(crate::magician_v2::chat::storage::FileChatStore::new(
            temp_dir.path(),
        ));
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let v3_service = build_test_artifact_v2_service(temp_dir.path());
        let progress_channel = ChatChannel::new(chat_store.clone(), broadcaster, v3_service);

        let session = chat_store
            .get_or_create_active_session(
                "principal-1",
                "workspace-a",
                "general",
                &SessionOriginChannel::new("local", "session"),
                "agent",
            )
            .await
            .expect("chat session");

        let mut metadata = HashMap::new();
        metadata.insert("session_id".to_string(), session.id.clone());
        let subscription = Subscription {
            id: "sub-1".to_string(),
            channel_id: "chat".to_string(),
            filter: SubscriptionFilter::TaskId("task-1".to_string()),
            principal: "principal-1".to_string(),
            workspace: "workspace-a".to_string(),
            min_severity: ProgressSeverity::Trace,
            metadata,
            source: SubscriptionSource::Dynamic,
            retention_secs: 0,
            message_template: None,
            output_severity: None,
            watermark: 0,
            pending_retry: Default::default(),
            created_at: 0,
        };
        let progress_message = ProgressMessage {
            id: "msg-projection-progress-1".to_string(),
            seq: 1,
            log_key: "task:task-1".to_string(),
            source: ProgressSource::Projection,
            event_type: None,
            metadata: Default::default(),
            execution_id: Some("exec-root".to_string()),
            task_id: Some("task-1".to_string()),
            root_task_id: Some("task-1".to_string()),
            root_execution_id: Some("exec-root".to_string()),
            parent_execution_id: None,
            agent_id: Some("personal-agent".to_string()),
            ui_thread_id: Some("general".to_string()),
            step_id: None,
            routing_keys: vec![
                "task/task-1".to_string(),
                "task/task-1/execution/exec-root".to_string(),
            ],
            principal: "principal-1".to_string(),
            workspace: "workspace-a".to_string(),
            severity: ProgressSeverity::Info,
            kind: ProgressMessageKind::AgentNotification {
                event_type: "execution.progress".to_string(),
                message: "Execution is running.".to_string(),
                entity_key: Some("execution:exec-root".to_string()),
                metadata: serde_json::json!({}),
            },
            timestamp: Utc::now().timestamp_millis(),
        };

        progress_channel
            .deliver(&subscription, &progress_message)
            .await
            .expect("deliver");

        let messages = chat_store
            .get_messages(&session.id, 10)
            .await
            .expect("messages");
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn agentic_stream_notifications_are_suppressed_from_chat_text() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let chat_store = Arc::new(crate::magician_v2::chat::storage::FileChatStore::new(
            temp_dir.path(),
        ));
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let v3_service = build_test_artifact_v2_service(temp_dir.path());
        let progress_channel = ChatChannel::new(chat_store.clone(), broadcaster, v3_service);

        let session = chat_store
            .get_or_create_active_session(
                "principal-1",
                "workspace-a",
                "general",
                &SessionOriginChannel::new("local", "session"),
                "agent",
            )
            .await
            .expect("chat session");

        let mut metadata = HashMap::new();
        metadata.insert("session_id".to_string(), session.id.clone());
        let subscription = Subscription {
            id: "sub-1".to_string(),
            channel_id: "chat".to_string(),
            filter: SubscriptionFilter::TaskId("task-1".to_string()),
            principal: "principal-1".to_string(),
            workspace: "workspace-a".to_string(),
            min_severity: ProgressSeverity::Trace,
            metadata,
            source: SubscriptionSource::Dynamic,
            retention_secs: 0,
            message_template: None,
            output_severity: None,
            watermark: 0,
            pending_retry: Default::default(),
            created_at: 0,
        };

        let suppressed_event_types = [
            "tool.call.started",
            "tool.call.args",
            "tool.call.finished",
            "reasoning.start",
            "reasoning.end",
            "plan.step.started",
            "plan.step.finished",
            "agent.cycle.started",
            "agent.cycle.completed",
        ];

        for (idx, event_type) in suppressed_event_types.iter().enumerate() {
            let progress_message = ProgressMessage {
                id: format!("msg-suppressed-{idx}"),
                seq: idx as u64 + 1,
                log_key: "task:task-1".to_string(),
                source: ProgressSource::Execution,
                event_type: None,
                metadata: Default::default(),
                execution_id: Some("exec-root".to_string()),
                task_id: Some("task-1".to_string()),
                root_task_id: Some("task-1".to_string()),
                root_execution_id: Some("exec-root".to_string()),
                parent_execution_id: None,
                agent_id: Some("personal-agent".to_string()),
                ui_thread_id: Some("general".to_string()),
                step_id: None,
                routing_keys: vec!["task/task-1".to_string()],
                principal: "principal-1".to_string(),
                workspace: "workspace-a".to_string(),
                severity: ProgressSeverity::Info,
                kind: ProgressMessageKind::AgentNotification {
                    event_type: (*event_type).to_string(),
                    message: format!("Agent event: {event_type}"),
                    entity_key: None,
                    metadata: serde_json::json!({}),
                },
                timestamp: Utc::now().timestamp_millis(),
            };
            progress_channel
                .deliver(&subscription, &progress_message)
                .await
                .expect("deliver");
        }

        let messages = chat_store
            .get_messages(&session.id, 100)
            .await
            .expect("messages");
        assert!(
            messages.is_empty(),
            "suppressed agentic-stream notifications must not produce chat Text messages, got {} messages",
            messages.len()
        );
    }

    #[tokio::test]
    async fn legacy_hitl_notifications_are_skipped_by_chat_channel_in_favor_of_attention_surfaces()
    {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let chat_store = Arc::new(crate::magician_v2::chat::storage::FileChatStore::new(
            temp_dir.path(),
        ));
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let v3_service = build_test_artifact_v2_service(temp_dir.path());
        let progress_channel =
            ChatChannel::new(chat_store.clone(), broadcaster.clone(), v3_service);
        let _drain = spawn_chat_store_drain(&broadcaster, chat_store.clone());

        let session = chat_store
            .get_or_create_active_session(
                "principal-1",
                "workspace-a",
                "general",
                &SessionOriginChannel::new("local", "session"),
                "agent",
            )
            .await
            .expect("chat session");

        let mut metadata = HashMap::new();
        metadata.insert("session_id".to_string(), session.id.clone());
        let subscription = Subscription {
            id: "sub-1".to_string(),
            channel_id: "chat".to_string(),
            filter: SubscriptionFilter::TaskId("task-1".to_string()),
            principal: "principal-1".to_string(),
            workspace: "workspace-a".to_string(),
            min_severity: ProgressSeverity::Trace,
            metadata,
            source: SubscriptionSource::Dynamic,
            retention_secs: 0,
            message_template: None,
            output_severity: None,
            watermark: 0,
            pending_retry: Default::default(),
            created_at: 0,
        };

        let progress_message = ProgressMessage {
            id: "msg-approval-1".to_string(),
            seq: 1,
            log_key: "task:task-1".to_string(),
            source: ProgressSource::Execution,
            event_type: None,
            metadata: Default::default(),
            execution_id: Some("exec-root".to_string()),
            task_id: Some("task-1".to_string()),
            root_task_id: Some("task-1".to_string()),
            root_execution_id: Some("exec-root".to_string()),
            parent_execution_id: None,
            agent_id: Some("personal-agent".to_string()),
            ui_thread_id: Some("general".to_string()),
            step_id: None,
            routing_keys: vec!["task/task-1".to_string()],
            principal: "principal-1".to_string(),
            workspace: "workspace-a".to_string(),
            severity: ProgressSeverity::Warning,
            kind: ProgressMessageKind::AgentNotification {
                // Canonical post-collapse taxonomy: every HITL prompt is
                // `hitl.requested`; `source` discriminates downstream.
                event_type: "hitl.requested".to_string(),
                message: "Approval requested before the agent can continue.".to_string(),
                entity_key: None,
                metadata: serde_json::json!({ "source": "approval" }),
            },
            timestamp: Utc::now().timestamp_millis(),
        };

        progress_channel
            .deliver(&subscription, &progress_message)
            .await
            .expect("deliver");

        let messages = chat_store
            .get_messages(&session.id, 10)
            .await
            .expect("messages");
        assert!(
            messages.is_empty(),
            "legacy HITL notifications must not produce plain chat Text rows — \
             the AttentionPill, /attention page, and per-turn activity card own HITL rendering; \
             got {} messages",
            messages.len()
        );
    }

    /// HITL events with canonical outer event_type set MUST be skipped
    /// by the chat channel — `EscalationListener` owns the chat
    /// `Escalation` / `EscalationResolved` content rendering, so the
    /// channel pipeline rendering a duplicate `Text` row for the same
    /// pause would double up on the operator.
    #[tokio::test]
    async fn hitl_events_are_skipped_by_chat_channel_in_favor_of_listener() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let chat_store = Arc::new(crate::magician_v2::chat::storage::FileChatStore::new(
            temp_dir.path(),
        ));
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let v3_service = build_test_artifact_v2_service(temp_dir.path());
        let progress_channel = ChatChannel::new(chat_store.clone(), broadcaster, v3_service);

        let session = chat_store
            .get_or_create_active_session(
                "principal-1",
                "workspace-a",
                "general",
                &SessionOriginChannel::new("local", "session"),
                "agent",
            )
            .await
            .expect("chat session");

        let mut metadata = HashMap::new();
        metadata.insert("session_id".to_string(), session.id.clone());
        let subscription = Subscription {
            id: "sub-1".to_string(),
            channel_id: "chat".to_string(),
            filter: SubscriptionFilter::TaskId("task-1".to_string()),
            principal: "principal-1".to_string(),
            workspace: "workspace-a".to_string(),
            min_severity: ProgressSeverity::Trace,
            metadata,
            source: SubscriptionSource::Dynamic,
            retention_secs: 0,
            message_template: None,
            output_severity: None,
            watermark: 0,
            pending_retry: Default::default(),
            created_at: 0,
        };

        for event_type in ["hitl.requested", "hitl.resolved"] {
            let progress_message = ProgressMessage {
                id: format!("msg-{event_type}"),
                seq: 1,
                log_key: "task:task-1".to_string(),
                source: ProgressSource::Execution,
                // Canonical post-H8: outer event_type matches the inner
                // AgentNotification event_type. The chat channel's HITL
                // skip relies on `effective_event_type`, which prefers
                // the outer one. Mirrors what `normalize.rs::base_message`
                // synthesizes in production.
                event_type: Some(event_type.to_string()),
                metadata: Default::default(),
                execution_id: Some("exec-root".to_string()),
                task_id: Some("task-1".to_string()),
                root_task_id: Some("task-1".to_string()),
                root_execution_id: Some("exec-root".to_string()),
                parent_execution_id: None,
                agent_id: Some("personal-agent".to_string()),
                ui_thread_id: Some("general".to_string()),
                step_id: None,
                routing_keys: vec!["task/task-1".to_string()],
                principal: "principal-1".to_string(),
                workspace: "workspace-a".to_string(),
                severity: ProgressSeverity::Warning,
                kind: ProgressMessageKind::AgentNotification {
                    event_type: event_type.to_string(),
                    message: "ignored".to_string(),
                    entity_key: None,
                    metadata: serde_json::json!({ "source": "approval" }),
                },
                timestamp: Utc::now().timestamp_millis(),
            };
            progress_channel
                .deliver(&subscription, &progress_message)
                .await
                .expect("deliver");
        }

        let messages = chat_store
            .get_messages(&session.id, 10)
            .await
            .expect("messages");
        assert!(
            messages.is_empty(),
            "ChatChannel must not emit chat rows for canonical HITL events — \
             EscalationListener owns that rendering; got {} messages",
            messages.len()
        );
    }

    #[tokio::test]
    async fn terminal_status_includes_primary_and_user_task_outputs() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let v3_service = build_test_artifact_v2_service(temp_dir.path());
        // Production chat storage and Artifact V2 share one workspace. Keep the
        // fixture on that topology so the projection's lifecycle fence can see
        // the session generation marker it is required to protect.
        let chat_store = Arc::new(
            crate::magician_v2::chat::storage::FileChatStore::with_workspace_layout(
                v3_service.workspace().clone(),
            ),
        );
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let progress_channel = ChatChannel::new(
            chat_store.clone(),
            broadcaster.clone(),
            Arc::clone(&v3_service),
        );
        let _drain = spawn_chat_store_drain(&broadcaster, chat_store.clone());

        let session = chat_store
            .get_or_create_active_session(
                "principal-1",
                "workspace-a",
                "general",
                &SessionOriginChannel::new("local", "session"),
                "agent",
            )
            .await
            .expect("chat session");
        let session_path = v3_service.workspace().chat_session_path(
            &session.principal,
            &session.workspace,
            &session.id,
        );
        assert!(
            v3_service
                .workspace()
                .metadata_path(&session_path)
                .await
                .expect("shared session metadata")
                .is_some(),
            "terminal projection must use the same lifecycle workspace as the chat store"
        );

        let task = v3_service
            .create_task(CreateTaskInput {
                principal: "principal-1".to_string(),
                workspace: "workspace-a".to_string(),
                title: "Render gallery".to_string(),
                description: "Produce task output files".to_string(),
                agent_id: "agent".to_string(),
                goal_id: None,
                ui_thread_id: "general".to_string(),
                priority: None,
                due_date: None,
                tags: Vec::new(),
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: TaskOutputMode::Accumulate,
                chat_session_id: None,
                lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::default(),
                sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await
            .expect("create task");

        let scope = ScopeRef::system_internal_unauthenticated(
            &"principal-1".to_string(),
            &"workspace-a".to_string(),
        );
        let outputs_dir = v3_service.workspace().task_outputs_dir(
            &scope.principal(),
            &scope.workspace(),
            &task.manifest.task_id,
        );
        v3_service
            .workspace()
            .create_dir_all_path(outputs_dir.join("reports"))
            .await
            .expect("create reports dir");
        v3_service
            .workspace()
            .write_path(outputs_dir.join("preview.png"), b"png-preview")
            .await
            .expect("write primary output");
        v3_service
            .workspace()
            .write_path(outputs_dir.join("reports/final.pdf"), b"pdf-report")
            .await
            .expect("write secondary output");
        v3_service
            .workspace()
            .write_path(outputs_dir.join("reports/internal.json"), br#"{"ok":true}"#)
            .await
            .expect("write non-user output");

        let mut refs = task.refs.clone();
        refs.outputs = vec![
            OutputRef {
                output_id: "output-secondary".to_string(),
                scope: "task".to_string(),
                audience: "user".to_string(),
                role: "secondary".to_string(),
                relative_path: "reports/final.pdf".to_string(),
                media_type: "application/pdf".to_string(),
                created_at: "2026-04-09T12:00:02Z".to_string(),
                source_execution_id: Some("exec-root".to_string()),
                source_plan_id: None,
                source_output_ids: Vec::new(),
            },
            OutputRef {
                output_id: "output-primary".to_string(),
                scope: "task".to_string(),
                audience: "user".to_string(),
                role: "primary".to_string(),
                relative_path: "outputs/preview.png".to_string(),
                media_type: "image/png".to_string(),
                created_at: "2026-04-09T12:00:01Z".to_string(),
                source_execution_id: Some("exec-root".to_string()),
                source_plan_id: None,
                source_output_ids: Vec::new(),
            },
            OutputRef {
                output_id: "output-agent".to_string(),
                scope: "task".to_string(),
                audience: "agent".to_string(),
                role: "debug".to_string(),
                relative_path: "reports/internal.json".to_string(),
                media_type: "application/json".to_string(),
                created_at: "2026-04-09T12:00:03Z".to_string(),
                source_execution_id: Some("exec-root".to_string()),
                source_plan_id: None,
                source_output_ids: Vec::new(),
            },
        ];
        refs.primary_user_output_id = Some("output-primary".to_string());
        refs.updated_at = Some("2026-04-09T12:00:04Z".to_string());

        let refs_path = v3_service.workspace().task_refs_path(
            &scope.principal(),
            &scope.workspace(),
            &task.manifest.task_id,
        );
        v3_service
            .workspace()
            .write_json_atomic_path(&refs_path, &refs)
            .await
            .expect("persist task refs");

        let mut metadata = HashMap::new();
        metadata.insert("session_id".to_string(), session.id.clone());
        let subscription = Subscription {
            id: "sub-outputs".to_string(),
            channel_id: "chat".to_string(),
            filter: SubscriptionFilter::TaskId(task.manifest.task_id.clone()),
            principal: scope.principal().to_string(),
            workspace: scope.workspace().to_string(),
            min_severity: ProgressSeverity::Trace,
            metadata,
            source: SubscriptionSource::Dynamic,
            retention_secs: 0,
            message_template: None,
            output_severity: None,
            watermark: 0,
            pending_retry: Default::default(),
            created_at: 0,
        };
        let message = ProgressMessage {
            id: "msg-terminal".to_string(),
            seq: 1,
            log_key: format!("task:{}", task.manifest.task_id),
            source: ProgressSource::Execution,
            event_type: None,
            metadata: Default::default(),
            execution_id: Some("exec-root".to_string()),
            task_id: Some(task.manifest.task_id.clone()),
            root_task_id: Some(task.manifest.task_id.clone()),
            root_execution_id: Some("exec-root".to_string()),
            parent_execution_id: None,
            agent_id: Some("agent".to_string()),
            ui_thread_id: Some("general".to_string()),
            step_id: None,
            routing_keys: vec![format!("task/{}", task.manifest.task_id)],
            principal: scope.principal().to_string(),
            workspace: scope.workspace().to_string(),
            severity: ProgressSeverity::Info,
            kind: ProgressMessageKind::StatusChanged {
                status: "completed".to_string(),
                summary: Some("Gallery render finished".to_string()),
            },
            timestamp: Utc::now().timestamp_millis(),
        };

        progress_channel
            .deliver(&subscription, &message)
            .await
            .expect("deliver terminal status");

        let messages = wait_for_message_count(&chat_store, &session.id, 1).await;
        assert_eq!(messages.len(), 1);
        let mut copied_session_paths = Vec::new();

        match &messages[0].content {
            ChatMessageContent::TaskStatusUpdate {
                task_id,
                status,
                summary,
                output_files,
                ..
            } => {
                assert_eq!(task_id, &task.manifest.task_id);
                assert_eq!(status, "completed");
                assert_eq!(summary.as_deref(), Some("Gallery render finished"));
                assert_eq!(output_files.len(), 2);

                match &output_files[0] {
                    ContentBlockRecord::File {
                        source,
                        relative_path,
                        display_name,
                        mime_type,
                        absolute_path,
                        label,
                        size,
                    } => {
                        assert_eq!(source, &ContentFileSource::SessionOutput);
                        assert!(relative_path.starts_with("task_output_preview_"));
                        assert!(relative_path.ends_with(".png"));
                        assert_eq!(display_name, "preview.png");
                        assert_eq!(mime_type, "image/png");
                        let absolute_path = absolute_path
                            .as_deref()
                            .expect("session output absolute path");
                        assert!(absolute_path.contains("/chat_sessions/"));
                        assert!(
                            tokio::fs::try_exists(absolute_path)
                                .await
                                .expect("session output exists check"),
                            "task output should be copied into the chat session before task deletion"
                        );
                        copied_session_paths.push(absolute_path.to_string());
                        assert_eq!(label, &None);
                        assert_eq!(*size, b"png-preview".len() as u64);
                    },
                    other => panic!("unexpected primary output block: {other:?}"),
                }

                match &output_files[1] {
                    ContentBlockRecord::File {
                        source,
                        relative_path,
                        display_name,
                        mime_type,
                        absolute_path,
                        size,
                        ..
                    } => {
                        assert_eq!(source, &ContentFileSource::SessionOutput);
                        assert!(relative_path.starts_with("task_output_final_"));
                        assert!(relative_path.ends_with(".pdf"));
                        assert_eq!(display_name, "final.pdf");
                        assert_eq!(mime_type, "application/pdf");
                        assert_eq!(*size, b"pdf-report".len() as u64);
                        copied_session_paths.push(
                            absolute_path
                                .clone()
                                .expect("secondary session output absolute path"),
                        );
                    },
                    other => panic!("unexpected secondary output block: {other:?}"),
                }
            },
            other => panic!("unexpected chat message content: {other:?}"),
        }

        v3_service
            .archive_task_with_options(&scope, &task.manifest.task_id, true)
            .await
            .expect("delete task after chat delivery");
        for copied_path in copied_session_paths {
            assert!(
                tokio::fs::try_exists(&copied_path)
                    .await
                    .expect("session output still exists check"),
                "chat output card must remain openable after task deletion"
            );
        }
    }
}
