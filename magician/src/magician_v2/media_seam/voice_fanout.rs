//! Voice downstream fan-out — process-local map of
//! `voice_session_id` → downstream sender for the corresponding
//! control-WS actor.
//!
//! The control-WS actor (`api/voice_control_handler::VoiceControlSession`)
//! takes ownership of an `mpsc::UnboundedReceiver` from the
//! orchestrator on session start. External code (today:
//! `ArtifactV2Service::update_task_status`) looks up the sender by
//! voice-session id and pushes a [`VoiceDownstreamMessage`] when a
//! relevant lifecycle event fires (task completion, future HITL
//! prompts, etc.). The actor forwards the message to the frontend
//! over the control WebSocket, which translates it into a
//! `conversation.item.create` + `response.create` so the realtime
//! voice model speaks the update.
//!
//! Why a fanout map instead of subscribing to the runtime event bus:
//!
//!  - Voice notifications need exact-once delivery to one specific
//!    call's WS actor, not broadcast.
//!  - The actor's lifetime is bounded by the WebSocket — the
//!    fanout naturally tracks "is this session still connected"
//!    via sender liveness without polling another data source.
//!  - The dispatcher pattern lets future external signals (e.g.
//!    "your meeting starts in 2 minutes") use the same channel
//!    without inventing a new event bus.
//!
//! The fanout uses `tokio::sync::mpsc::UnboundedSender` rather
//! than Actix `Addr<_>` so the artifact service (and anywhere else
//! outside Actix's actor system) can notify voice sessions without
//! depending on actix-web in the call site.

use std::sync::Arc;

use dashmap::DashMap;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::debug;

/// One message to push downstream to one voice session via the fanout. Serialized
/// to JSON and sent as a text frame to the browser. The frontend
/// transport translates each `kind` into the provider-specific
/// system-message + response trigger.
#[derive(Debug, Clone, Serialize)]
pub struct VoiceDownstreamMessage {
    /// Short discriminator the frontend matches on. Current vocabulary:
    ///   - `task.completed` — payload:
    ///     `{task_id, title, status, summary, verification_state}`
    ///   - `task.awaiting_diff_approval` — payload:
    ///     `{task_id, title, proposal_id, changed_file_count}`
    /// Extend cautiously — every new kind needs a frontend handler in
    /// `lib/media/voice/realtimeVoiceClient.ts` (the data-channel
    /// path that fans frames into the OpenAI realtime session).
    ///
    /// `task.awaiting_diff_approval` is deliberately **server-spoken only**
    /// for now: `VoiceControlSession` renders it and injects it into the
    /// provider itself, so the backend-proxied / cascaded road speaks it with
    /// no frontend change, and the data-channel road ignores an envelope it
    /// does not know. Adding the frontend handler widens it to the remaining
    /// road; nothing here depends on that having happened.
    pub kind: String,
    /// Free-form JSON. Frontend reads kind-specific fields.
    pub payload: Value,
}

/// Process-local map of voice-session id → downstream sender.
#[derive(Default)]
pub struct VoiceDownstreamFanout {
    senders: DashMap<String, mpsc::UnboundedSender<VoiceDownstreamMessage>>,
    /// `chat_session_id` → `voice_session_id`. A terminal task carries its
    /// originating `chat_session_id` (`TaskManifest.chat_session_id`), so this
    /// index routes its completion back to the live voice session regardless of
    /// which tool created it (`create_task`, `delegate_to_agent`,
    /// `orchestrate_pipeline`, …) — covering user-visible AND internal tasks
    /// without tagging the task record.
    chat_to_voice: DashMap<String, String>,
    /// `(principal, workspace)` → `voice_session_id`, for announcements about a
    /// task that carries **no** `chat_session_id`.
    ///
    /// A cockpit VibeDev run is the case this exists for. Its manifest sets
    /// `chat_session_id: None` on purpose — but for sweep safety, not routing:
    /// `POST /v3/tasks` never set one, so binding it now would make months of
    /// existing runs newly deletable. Voice needs a routing key and the sweep
    /// needs the absence of one; the two collided by accident, and a pending
    /// diff on a cockpit run was silently unannounceable as a result.
    ///
    /// Scope-level rather than task-level because that is the widest thing the
    /// caller can honestly claim: this voice call belongs to this principal and
    /// workspace, so a diff waiting anywhere in that scope is theirs to hear
    /// about. It is deliberately **not** a substitute for `chat_to_voice` —
    /// where a chat session exists it is the more precise key and is tried
    /// first.
    scope_to_voice: DashMap<(String, String), String>,
}

impl VoiceDownstreamFanout {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register a session's downstream sender. Replaces any existing
    /// entry — the same voice-session id reconnecting (e.g. a
    /// transient WS drop + reopen) gets the new actor's sender
    /// without leaving the old one stranded.
    pub fn register(
        &self,
        voice_session_id: impl Into<String>,
        sender: mpsc::UnboundedSender<VoiceDownstreamMessage>,
    ) {
        let key = voice_session_id.into();
        debug!("[VOICE-FANOUT] register {key}");
        self.senders.insert(key, sender);
    }

    /// Remove a session's entry. Called on
    /// `VoiceOrchestrator::end()` when the control WS tears down.
    /// Best-effort — missing entries (already removed by reconnect)
    /// silently no-op.
    pub fn unregister(&self, voice_session_id: &str) {
        debug!("[VOICE-FANOUT] unregister {voice_session_id}");
        self.senders.remove(voice_session_id);
        self.chat_to_voice
            .retain(|_, vs| vs.as_str() != voice_session_id);
        self.scope_to_voice
            .retain(|_, vs| vs.as_str() != voice_session_id);
    }

    /// Associate a chat session with a live voice session so terminal tasks
    /// that originated from that chat session (by `TaskManifest.chat_session_id`)
    /// route their completion back to it. Called once the voice session binds
    /// its chat session; cleared in `unregister`. Idempotent.
    pub fn link_chat_session(
        &self,
        voice_session_id: impl Into<String>,
        chat_session_id: impl Into<String>,
    ) {
        let vs = voice_session_id.into();
        let cs = chat_session_id.into();
        debug!("[VOICE-FANOUT] link chat {cs} -> voice {vs}");
        self.chat_to_voice.insert(cs, vs);
    }

    /// Associate a scope with a live voice session, so a task in that scope
    /// with no `chat_session_id` can still reach the call. Cleared in
    /// `unregister`. Idempotent; the last call to bind a scope wins, which
    /// matches how `link_chat_session` behaves for a reconnecting session.
    pub fn link_scope(
        &self,
        voice_session_id: impl Into<String>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) {
        let vs = voice_session_id.into();
        let key = (principal.into(), workspace.into());
        debug!(
            "[VOICE-FANOUT] link scope {}/{} -> voice {vs}",
            key.0, key.1
        );
        self.scope_to_voice.insert(key, vs);
    }

    /// Route a message to the live voice session bound to this scope, if any.
    /// The fallback for a task that carries no chat session. Returns `false`
    /// when no call is bound.
    pub fn notify_by_scope(
        &self,
        principal: &str,
        workspace: &str,
        message: VoiceDownstreamMessage,
    ) -> bool {
        let key = (principal.to_string(), workspace.to_string());
        let Some(voice_session_id) = self.scope_to_voice.get(&key).map(|v| v.value().clone())
        else {
            return false;
        };
        self.notify(&voice_session_id, message)
    }

    /// True when a live call is bound to this scope and its channel is open.
    /// Same purpose as `is_chat_session_linked`: let a caller skip building a
    /// message nobody will hear.
    pub fn is_scope_linked(&self, principal: &str, workspace: &str) -> bool {
        let key = (principal.to_string(), workspace.to_string());
        let Some(voice_session_id) = self.scope_to_voice.get(&key).map(|v| v.value().clone())
        else {
            return false;
        };
        self.senders
            .get(&voice_session_id)
            .is_some_and(|entry| !entry.value().is_closed())
    }

    /// Route a message to the live voice session that owns `chat_session_id`
    /// (if any). Used for task-completion announcements so they reach the
    /// originating voice call no matter which tool spawned the task. Returns
    /// `false` when no voice session currently owns that chat session.
    pub fn notify_by_chat_session(
        &self,
        chat_session_id: &str,
        message: VoiceDownstreamMessage,
    ) -> bool {
        let Some(voice_session_id) = self
            .chat_to_voice
            .get(chat_session_id)
            .map(|v| v.value().clone())
        else {
            return false;
        };
        self.notify(&voice_session_id, message)
    }

    /// True when a live voice session currently owns `chat_session_id` and its
    /// downstream channel is still open. Lets callers skip expensive work (e.g.
    /// a synthesis read to build a completion summary) when no call is
    /// listening, without building a message just to have `notify` drop it.
    pub fn is_chat_session_linked(&self, chat_session_id: &str) -> bool {
        let Some(voice_session_id) = self
            .chat_to_voice
            .get(chat_session_id)
            .map(|v| v.value().clone())
        else {
            return false;
        };
        self.senders
            .get(&voice_session_id)
            .is_some_and(|entry| !entry.value().is_closed())
    }

    /// Push a message to the session for `voice_session_id`. Returns
    /// `true` if delivered (or queued in the channel), `false` if no
    /// such session is registered or the receiver was dropped.
    ///
    /// Failure is non-fatal — callers should not retry; the voice
    /// session has either ended or the call dropped. The next time
    /// the user starts a voice call the system message would be
    /// re-derivable from the chat ledger on the next call anyway.
    pub fn notify(&self, voice_session_id: &str, message: VoiceDownstreamMessage) -> bool {
        let Some(entry) = self.senders.get(voice_session_id) else {
            debug!("[VOICE-FANOUT] notify dropped — no session for {voice_session_id}");
            return false;
        };
        match entry.send(message) {
            Ok(()) => true,
            Err(_) => {
                debug!("[VOICE-FANOUT] notify dropped — receiver closed for {voice_session_id}");
                false
            },
        }
    }
}

/// Convenience helper for the task-completion notifier — builds a
/// `task.completed` downstream message with the standard payload
/// shape the frontend expects.
///
/// `verification_state` is the task's durable `verification_state` projection
/// (`ArtifactV2Service::verification_state_for_task`), carried as its
/// `as_str()` spelling. It decides which — if any — verification sentence the
/// spoken announcement carries; `"unknown"` is the overwhelmingly common
/// value and deliberately produces none. The key is always present so the
/// renderer never has to guess whether the producer knew, and an older
/// producer that omits it lands on the same `"unknown"` default.
pub fn task_completed_message(
    task_id: &str,
    title: &str,
    status: &str,
    summary: &str,
    verification_state: &str,
) -> VoiceDownstreamMessage {
    VoiceDownstreamMessage {
        kind: "task.completed".to_string(),
        payload: serde_json::json!({
            "task_id": task_id,
            "title": title,
            "status": status,
            "summary": summary,
            "verification_state": verification_state,
        }),
    }
}

/// One streamed `<speech>` segment from a `delegate_to_chat`
/// background run. Each chunk is a complete speech-tag body — the
/// chat-LLM produced and closed a `<speech>...</speech>` block, the
/// streaming extractor on the orchestrator emitted it. The frontend
/// injects each chunk into the realtime session as a system message
/// + response.create so the voice model speaks it.
///
/// `sequence` is monotonically increasing per dispatch so the
/// frontend can detect drops / out-of-order delivery (currently the
/// fanout is FIFO over a single mpsc, so this is informational; the
/// counter is here for future multi-path delivery).
///
/// `call_id` correlates back to the original `delegate_to_chat` tool
/// call so frontend / observability can attribute the chunk to the
/// right dispatch.
pub fn delegate_to_chat_chunk_message(
    call_id: &str,
    sequence: u64,
    text: &str,
) -> VoiceDownstreamMessage {
    VoiceDownstreamMessage {
        kind: "delegate_to_chat.chunk".to_string(),
        payload: serde_json::json!({
            "call_id": call_id,
            "sequence": sequence,
            "text": text,
        }),
    }
}

/// Sentinel marking the end of a `delegate_to_chat` background run.
/// `chunk_count` and `success` let the frontend reason about whether
/// the user heard the full answer (chunk_count > 0 && success) or
/// whether the call failed mid-stream (success == false).
pub fn delegate_to_chat_done_message(
    call_id: &str,
    chunk_count: u64,
    success: bool,
    error: Option<&str>,
) -> VoiceDownstreamMessage {
    VoiceDownstreamMessage {
        kind: "delegate_to_chat.done".to_string(),
        payload: serde_json::json!({
            "call_id": call_id,
            "chunk_count": chunk_count,
            "success": success,
            "error": error,
        }),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::media_seam::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn register_and_notify_roundtrip() {
        let registry = VoiceDownstreamFanout::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        registry.register("vs-1", tx);
        let delivered = registry.notify(
            "vs-1",
            task_completed_message("t-1", "Send email", "completed", "Sent.", "unknown"),
        );
        assert!(delivered);
        let msg = rx.recv().await.expect("should receive message");
        assert_eq!(msg.kind, "task.completed");
    }

    #[test]
    fn notify_unknown_session_returns_false() {
        let registry = VoiceDownstreamFanout::new();
        let delivered = registry.notify(
            "vs-missing",
            task_completed_message("t-1", "x", "completed", "y", "unknown"),
        );
        assert!(!delivered);
    }

    #[tokio::test]
    async fn unregister_drops_subsequent_notifications() {
        let registry = VoiceDownstreamFanout::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        registry.register("vs-1", tx);
        registry.unregister("vs-1");
        let delivered = registry.notify(
            "vs-1",
            task_completed_message("t-1", "x", "completed", "y", "unknown"),
        );
        assert!(!delivered);
    }

    #[tokio::test]
    async fn notify_after_receiver_drop_returns_false() {
        let registry = VoiceDownstreamFanout::new();
        let (tx, rx) = mpsc::unbounded_channel();
        registry.register("vs-1", tx);
        drop(rx);
        let delivered = registry.notify(
            "vs-1",
            task_completed_message("t-1", "x", "completed", "y", "unknown"),
        );
        assert!(!delivered);
    }
}

pub const MEDIA_SESSION_HEARTBEAT: &str = "media.session.heartbeat";
pub const MEDIA_SESSION_REGISTERED: &str = "media.session.registered";
