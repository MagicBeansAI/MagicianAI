//! Live Thinking Map — ambient session coordinator (Phase 3 runtime wiring).
//!
//! Makes a map "build itself as you speak". This background component
//! subscribes read-only to the unified transport bus and, for each finalized
//! USER utterance that lands in a *registered* chat/voice session, auto-maps it
//! (interpret → apply) against the bound Thinking Map. A meeting/observe/voice
//! session is attached to a map via
//! [`ThinkingMapSessionCoordinator::register`] (driven by the
//! `POST /thinking-maps/{map_id}/sessions` API); every subsequent user turn in
//! that session flows through [`interpret`] under the `continue_thinking`
//! intent and persists the smallest useful set of moves.
//!
//! ## Shape (mirrors [`magician::magician_v2::chat::chat_store_sink::ChatStoreSink`])
//! - Subscribe to the broadcaster **before** spawning (no lost-first-emit race).
//! - One spawned tokio task owns the `broadcast::Receiver` and processes events
//!   serially. `Lagged` logs and continues; `Closed` exits the loop.
//! - Per-event errors in [`ThinkingMapSessionCoordinator::apply`] are logged and
//!   swallowed — a bad utterance never kills the loop.
//!
//! ## Contention / read-only
//! The coordinator NEVER edits the chat/voice/meeting emit paths; it is a pure
//! downstream subscriber of the SAME `ChatMessageReceived` event
//! [`ChatStoreSink`] consumes. It reuses that existing event — no new
//! `RuntimeTransportEvent` variant.
//!
//! ## Determinism note
//! The apply path is runtime (not the deterministic reducer), so it MAY call
//! `Utc::now()` for the injected `applied_at`. The interpreter/reducer core stay
//! clock-free — they receive the injected timestamp we pass here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast::{error::RecvError, Receiver};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::chat::models::{ChatMessageContent, ChatMessageDirection};
use magician::magician_v2::query_analysis::operation_llm_router::global_operation_router;
use magician::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

use super::interpreter::{interpret, InterpretIntent, InterpreterLlm, Utterance};
use super::llm_router_adapter::RouterInterpreterLlm;
use super::store::ThinkingMapStore;

/// Trace label stamped as the model actor's provenance for ambient-mapped
/// operations (`OperationActor::Model { trace_id }`).
const AMBIENT_TRACE_ID: &str = "thinking_map_ambient";

/// The `source_surface` the chat service stamps onto live meeting-transcript
/// lines (`ChatService::persist_meeting_transcript_line`). Those lines are
/// `direction: System` by design (display-only, never dispatch the agent), so
/// the meeting bridge keys on this surface tag instead of the direction.
const MEETING_TRANSCRIPT_SURFACE: &str = "meeting-transcript";

/// Whether a chat turn is worth auto-mapping. Two shapes qualify:
///
/// 1. **`direction: User`** — typed or spoken user turns (chat, dictation, the
///    voice orchestrator's finalized STT turns).
/// 2. **The meeting bridge:** `direction: System` turns whose `source_surface`
///    is [`MEETING_TRANSCRIPT_SURFACE`]. The meeting rail streams every heard
///    turn as a display-only System line (speaker prefixed into the text), so
///    without this a map attached to a meeting's chat session would never see
///    the meeting. The opt-in is the attach itself — an unregistered meeting
///    session is still ignored like all other traffic, and no other System
///    message (status cards, escalation notes, …) carries this surface tag.
///
/// Assistant turns never map: the board captures *the human's* thinking; model
/// content enters only as `model_inferred` output of the interpreter.
fn is_mappable_turn(direction: &ChatMessageDirection, source_surface: Option<&str>) -> bool {
    match direction {
        ChatMessageDirection::User => true,
        ChatMessageDirection::System => source_surface == Some(MEETING_TRANSCRIPT_SURFACE),
        ChatMessageDirection::Assistant => false,
    }
}

/// A registered source session's binding: which map its utterances feed, under
/// which scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBinding {
    pub map_id: String,
    pub principal: String,
    pub workspace: String,
}

/// Ambient coordinator: subscribes to the transport bus and auto-maps user
/// utterances from registered sessions onto their bound Thinking Map.
pub struct ThinkingMapSessionCoordinator {
    broadcaster: Arc<RuntimeTransportBroadcaster>,
    /// Workspace used to build a per-apply [`ThinkingMapStore`].
    workspace: ArtifactV2Workspace,
    /// `source_session_id -> binding`. A plain `std::sync::Mutex<HashMap>`
    /// (registrations are infrequent + the guard is never held across an
    /// `.await`), consistent with the store's own registry pattern.
    registry: Arc<Mutex<HashMap<String, SourceBinding>>>,
}

impl ThinkingMapSessionCoordinator {
    /// Build the coordinator. Returns an `Arc` so the same instance can be
    /// shared to actix `app_data` (for the attach/detach API) AND moved into the
    /// spawned subscription task.
    pub fn new(
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        workspace: ArtifactV2Workspace,
    ) -> Arc<Self> {
        Arc::new(Self {
            broadcaster,
            workspace,
            registry: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    // ── Registry ─────────────────────────────────────────────────────────────

    /// Bind a source (chat/voice) session to a map so its user utterances
    /// auto-map. Overwrites any prior binding for the same session id.
    pub fn register(
        &self,
        source_session_id: impl Into<String>,
        map_id: impl Into<String>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) {
        let session = source_session_id.into();
        let binding = SourceBinding {
            map_id: map_id.into(),
            principal: principal.into(),
            workspace: workspace.into(),
        };
        info!(
            source_session_id = %session,
            map_id = %binding.map_id,
            principal = %binding.principal,
            workspace = %binding.workspace,
            "[thinking-map-coordinator] registered session → map"
        );
        self.lock_registry().insert(session, binding);
    }

    /// Detach a source session. Returns `true` if a binding was present.
    pub fn unregister(&self, source_session_id: &str) -> bool {
        let removed = self.lock_registry().remove(source_session_id).is_some();
        if removed {
            info!(
                source_session_id = %source_session_id,
                "[thinking-map-coordinator] unregistered session"
            );
        }
        removed
    }

    /// Detach every source session bound to one scoped map. Permanent deletion
    /// uses this after durable removal so no later ambient utterance retains a
    /// binding to the missing map.
    pub fn unregister_map(&self, principal: &str, workspace: &str, map_id: &str) -> usize {
        let mut registry = self.lock_registry();
        let before = registry.len();
        registry.retain(|_, binding| {
            binding.principal != principal
                || binding.workspace != workspace
                || binding.map_id != map_id
        });
        let removed = before.saturating_sub(registry.len());
        if removed > 0 {
            info!(
                map_id = %map_id,
                principal = %principal,
                workspace = %workspace,
                removed,
                "[thinking-map-coordinator] unregistered all sessions for map"
            );
        }
        removed
    }

    /// Return the binding for a source session, if registered.
    pub fn bound_map(&self, source_session_id: &str) -> Option<SourceBinding> {
        self.lock_registry().get(source_session_id).cloned()
    }

    /// Resolve the binding for an incoming message, matching the registry
    /// against the chat `session_id` first and then the `presence_session_id`
    /// fallback. The voice path emits `session_id` = the *chat* session id but
    /// stamps the originating *media/voice* session id onto
    /// `presence_session_id`; a client that only knows its media session id (the
    /// iOS `RealtimeVoiceClient`, which never sees the server-derived chat
    /// session id) attaches that id, so we must check it too, while chat/web
    /// clients attach the chat session id directly.
    ///
    /// Returns the binding + the id that matched — that id becomes the
    /// utterance's thread id + trace key. Its own method (not inlined into
    /// [`Self::apply`]) so the fallback is unit-testable without a live LLM
    /// router.
    fn resolve_binding(
        &self,
        session_id: &str,
        presence_session_id: Option<&str>,
    ) -> Option<(SourceBinding, String)> {
        self.bound_map(session_id)
            .map(|binding| (binding, session_id.to_string()))
            .or_else(|| {
                presence_session_id.and_then(|pid| {
                    self.bound_map(pid)
                        .map(|binding| (binding, pid.to_string()))
                })
            })
    }

    /// Number of registered sessions (test/introspection helper).
    pub fn registered_count(&self) -> usize {
        self.lock_registry().len()
    }

    fn lock_registry(&self) -> std::sync::MutexGuard<'_, HashMap<String, SourceBinding>> {
        self.registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ── Subscription loop ────────────────────────────────────────────────────

    /// Subscribe (synchronously, BEFORE spawning) and run the bus loop on a
    /// background task. Subscribing before the spawn buffers any event emitted
    /// between this call and the task being scheduled — matching
    /// [`ChatStoreSink::spawn`]'s no-lost-first-emit contract.
    pub fn spawn(self: Arc<Self>) -> JoinHandle<()> {
        let rx = self.broadcaster.subscribe();
        let me = Arc::clone(&self);
        tokio::spawn(async move { me.run(rx).await })
    }

    async fn run(self: Arc<Self>, mut rx: Receiver<RuntimeTransportEvent>) {
        info!("[thinking-map-coordinator] subscribed to transport bus");
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Err(error) = self.apply(event).await {
                        warn!(
                            error = %error,
                            "[thinking-map-coordinator] failed to auto-map utterance"
                        );
                    }
                },
                Err(RecvError::Lagged(skipped)) => {
                    warn!(
                        skipped,
                        "[thinking-map-coordinator] broadcast lagged — some utterances were not auto-mapped"
                    );
                },
                Err(RecvError::Closed) => {
                    info!("[thinking-map-coordinator] broadcast closed, exiting");
                    break;
                },
            }
        }
    }

    /// Handle one bus event. Only `ChatMessageReceived` carrying a *mappable*
    /// turn (see [`is_mappable_turn`]) with extractable plain text, in a
    /// *registered* session, is auto-mapped. Every other case is a silent no-op
    /// (`Ok(())`).
    async fn apply(&self, event: RuntimeTransportEvent) -> anyhow::Result<()> {
        // Only chat messages carry utterances worth mapping.
        let RuntimeTransportEvent::ChatMessageReceived {
            session_id,
            message,
            ..
        } = event
        else {
            return Ok(());
        };
        if !is_mappable_turn(&message.direction, message.source_surface.as_deref()) {
            return Ok(());
        }

        // Unregistered session ⇒ ignore (the vast majority of chat traffic).
        let Some((binding, matched_session_id)) =
            self.resolve_binding(&session_id, message.presence_session_id.as_deref())
        else {
            return Ok(());
        };

        // Plain text only. Non-text content (tool results, attachments, status
        // cards) and empty/whitespace text add nothing structural to a map.
        let ChatMessageContent::Text { text, .. } = &message.content else {
            return Ok(());
        };
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let text = text.to_string();

        // The interpreter needs a live LLM. The router is a process-global set
        // at startup; absent (pre-startup) ⇒ skip this utterance.
        let Some(router) = global_operation_router() else {
            debug!(
                source_session_id = %matched_session_id,
                "[thinking-map-coordinator] no operation router yet — skipping utterance"
            );
            return Ok(());
        };
        let adapter = RouterInterpreterLlm::new(
            router,
            Some(self.broadcaster.clone()),
            binding.principal.clone(),
            binding.workspace.clone(),
        );

        self.map_utterance(&binding, &matched_session_id, &message.id, &text, &adapter)
            .await
    }

    /// The testable inner map step: load → interpret → apply, given an injected
    /// [`InterpreterLlm`]. Separated from [`Self::apply`] (which builds the
    /// production [`RouterInterpreterLlm`] from the global router) so unit tests
    /// can drive a fake LLM against a real tempdir store.
    ///
    /// The produced envelope's `idempotency_key` is `interp:<message_id>`, so a
    /// re-delivered bus event (same chat message id) is a no-op idempotent
    /// replay at the store layer.
    pub async fn map_utterance(
        &self,
        binding: &SourceBinding,
        source_session_id: &str,
        message_id: &str,
        text: &str,
        llm: &dyn InterpreterLlm,
    ) -> anyhow::Result<()> {
        let store = ThinkingMapStore::new(self.workspace.clone());

        // Map permanently gone mid-session ⇒ ignore this utterance. We leave
        // the binding in place; a subsequent utterance simply no-ops the same
        // way (the permanent-delete handler proactively clears bindings).
        let Some(map) = store
            .load_map(&binding.principal, &binding.workspace, &binding.map_id)
            .await?
        else {
            debug!(
                source_session_id = %source_session_id,
                map_id = %binding.map_id,
                "[thinking-map-coordinator] bound map not found — skipping utterance"
            );
            return Ok(());
        };
        if map.lifecycle == crate::thinking_map::MapLifecycle::Deleted {
            debug!(
                source_session_id = %source_session_id,
                map_id = %binding.map_id,
                "[thinking-map-coordinator] bound map is deleted — detaching session"
            );
            self.unregister(source_session_id);
            return Ok(());
        }

        let now = chrono::Utc::now().to_rfc3339();
        let utterance = Utterance {
            utterance_id: message_id.to_string(),
            text: text.to_string(),
            thread_id: Some(source_session_id.to_string()),
            timestamp: Some(now.clone()),
        };

        // Ambient = smallest-set per utterance (continue_thinking intent).
        // No progress narration: nobody triggered this interpretation, and
        // clients deliberately ignore progress for an utterance they did not
        // start — emitting stages here would be noise every client filters.
        let Some(envelope) = interpret(
            &map,
            &utterance,
            InterpretIntent::ContinueThinking,
            llm,
            &now,
            Some(AMBIENT_TRACE_ID.to_string()),
            &super::interpreter::NoProgress,
        )
        .await?
        else {
            // Valid zero-move interpretation (chit-chat / filler). Nothing to do.
            return Ok(());
        };

        let outcome = store
            .apply_and_persist(
                &binding.principal,
                &binding.workspace,
                &binding.map_id,
                &envelope,
                &now,
            )
            .await?;
        // Push a change notice so open clients (web sharedPoll / iOS listen
        // poll) refresh immediately instead of waiting out a poll interval.
        // Idempotent replays emit nothing — the state didn't change.
        if let crate::thinking_map::ApplyOutcome::Applied {
            resulting_revision, ..
        } = &outcome
        {
            self.broadcaster
                .emit_transport_only(RuntimeTransportEvent::ThinkingMapUpdated {
                    map_id: binding.map_id.clone(),
                    principal: binding.principal.clone(),
                    workspace: binding.workspace.clone(),
                    revision: *resulting_revision,
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
        }
        debug!(
            source_session_id = %source_session_id,
            map_id = %binding.map_id,
            message_id = %message_id,
            "[thinking-map-coordinator] auto-mapped utterance onto map"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thinking_map::models::{ThinkingMap, ThinkingMapSource};
    use async_trait::async_trait;
    use tempfile::TempDir;

    const TS: &str = "2026-07-19T00:00:00Z";

    /// A fake InterpreterLlm that always returns the same canned operations JSON
    /// (the same technique the interpreter's own tests use).
    struct FakeLlm(String);
    #[async_trait]
    impl InterpreterLlm for FakeLlm {
        async fn complete(&self, _system: &str, _user: &str) -> anyhow::Result<String> {
            Ok(self.0.clone())
        }
    }

    fn coordinator() -> (TempDir, Arc<ThinkingMapSessionCoordinator>) {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(64));
        let coord = ThinkingMapSessionCoordinator::new(broadcaster, workspace);
        (tmp, coord)
    }

    async fn create_map(coord: &ThinkingMapSessionCoordinator, map_id: &str) {
        let store = ThinkingMapStore::new(coord.workspace.clone());
        let map = ThinkingMap::new(
            map_id.to_string(),
            "anonymous",
            "default",
            "Test map",
            ThinkingMapSource::Solo,
            TS,
        );
        store.create_map(&map).await.expect("create map");
    }

    fn add_node_json(label: &str) -> String {
        format!(
            r#"{{"operations":[{{"op":"add_node","temp_id":"n1","kind":"idea","label":"{label}"}}]}}"#
        )
    }

    #[test]
    fn register_unregister_bound_map() {
        let (_tmp, coord) = coordinator();
        assert!(coord.bound_map("sess-1").is_none());
        assert_eq!(coord.registered_count(), 0);

        coord.register("sess-1", "map-1", "anonymous", "default");
        let binding = coord.bound_map("sess-1").expect("bound");
        assert_eq!(binding.map_id, "map-1");
        assert_eq!(binding.principal, "anonymous");
        assert_eq!(binding.workspace, "default");
        assert_eq!(coord.registered_count(), 1);

        // Re-register overwrites.
        coord.register("sess-1", "map-2", "anonymous", "default");
        assert_eq!(coord.bound_map("sess-1").unwrap().map_id, "map-2");
        assert_eq!(coord.registered_count(), 1);

        assert!(coord.unregister("sess-1"));
        assert!(!coord.unregister("sess-1")); // second time: nothing to remove
        assert!(coord.bound_map("sess-1").is_none());
    }

    #[test]
    fn unregister_map_removes_only_matching_scoped_bindings() {
        let (_tmp, coord) = coordinator();
        coord.register("one", "map-1", "anonymous", "default");
        coord.register("two", "map-1", "anonymous", "default");
        coord.register("other-map", "map-2", "anonymous", "default");
        coord.register("other-scope", "map-1", "someone", "default");

        assert_eq!(coord.unregister_map("anonymous", "default", "map-1"), 2);
        assert!(coord.bound_map("one").is_none());
        assert!(coord.bound_map("two").is_none());
        assert!(coord.bound_map("other-map").is_some());
        assert!(coord.bound_map("other-scope").is_some());
    }

    #[tokio::test]
    async fn map_utterance_adds_model_inferred_node() {
        let (_tmp, coord) = coordinator();
        create_map(&coord, "map-1").await;
        coord.register("sess-1", "map-1", "anonymous", "default");
        let binding = coord.bound_map("sess-1").unwrap();

        let llm = FakeLlm(add_node_json("Ship v1"));
        coord
            .map_utterance(&binding, "sess-1", "msg-1", "let's ship v1", &llm)
            .await
            .expect("map_utterance");

        // The map gained exactly one model-inferred node.
        let store = ThinkingMapStore::new(coord.workspace.clone());
        let map = store
            .load_map("anonymous", "default", "map-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(map.revision, 1);
        assert_eq!(map.nodes.len(), 1);
        let node = map.nodes.values().next().unwrap();
        assert_eq!(node.label, "Ship v1");
        assert_eq!(
            node.assertion_origin,
            crate::thinking_map::AssertionOrigin::ModelInferred
        );
        // Provenance cites the source session as the thread + the message id.
        let sr = node.source_refs.first().expect("source ref");
        assert_eq!(sr.utterance_id, Some("msg-1".to_string()));
        assert_eq!(sr.thread_id, Some("sess-1".to_string()));
    }

    #[tokio::test]
    async fn map_utterance_is_idempotent_by_message_id() {
        let (_tmp, coord) = coordinator();
        create_map(&coord, "map-1").await;
        coord.register("sess-1", "map-1", "anonymous", "default");
        let binding = coord.bound_map("sess-1").unwrap();
        let llm = FakeLlm(add_node_json("Ship v1"));

        // First delivery applies.
        coord
            .map_utterance(&binding, "sess-1", "msg-1", "let's ship v1", &llm)
            .await
            .unwrap();
        // Re-delivery of the SAME message id → idempotent replay, no second node.
        coord
            .map_utterance(&binding, "sess-1", "msg-1", "let's ship v1", &llm)
            .await
            .unwrap();

        let store = ThinkingMapStore::new(coord.workspace.clone());
        let map = store
            .load_map("anonymous", "default", "map-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(map.revision, 1, "re-delivery must not advance the revision");
        assert_eq!(map.nodes.len(), 1, "re-delivery must not add a second node");
    }

    #[tokio::test]
    async fn map_utterance_zero_move_is_noop() {
        let (_tmp, coord) = coordinator();
        create_map(&coord, "map-1").await;
        coord.register("sess-1", "map-1", "anonymous", "default");
        let binding = coord.bound_map("sess-1").unwrap();

        // Interpreter returns an empty operations list (chit-chat).
        let llm = FakeLlm(r#"{"operations":[]}"#.to_string());
        coord
            .map_utterance(&binding, "sess-1", "msg-1", "uhh, so, anyway", &llm)
            .await
            .expect("map_utterance");

        let store = ThinkingMapStore::new(coord.workspace.clone());
        let map = store
            .load_map("anonymous", "default", "map-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(map.revision, 0);
        assert!(map.nodes.is_empty());
    }

    #[tokio::test]
    async fn map_utterance_missing_map_is_noop() {
        let (_tmp, coord) = coordinator();
        // Bind a map id that was never created on disk.
        coord.register("sess-1", "ghost", "anonymous", "default");
        let binding = coord.bound_map("sess-1").unwrap();
        let llm = FakeLlm(add_node_json("Ship v1"));

        // Must not error — a deleted/absent bound map is a silent skip.
        coord
            .map_utterance(&binding, "sess-1", "msg-1", "hello", &llm)
            .await
            .expect("missing map is a no-op, not an error");
    }

    #[tokio::test]
    async fn map_utterance_soft_deleted_map_detaches_without_calling_interpreter_path() {
        let (_tmp, coord) = coordinator();
        let store = ThinkingMapStore::new(coord.workspace.clone());
        let mut map = ThinkingMap::new(
            "map-1".to_string(),
            "anonymous",
            "default",
            "Deleted map",
            ThinkingMapSource::Solo,
            TS,
        );
        map.lifecycle = crate::thinking_map::MapLifecycle::Deleted;
        store.create_map(&map).await.unwrap();
        coord.register("sess-1", "map-1", "anonymous", "default");
        let binding = coord.bound_map("sess-1").unwrap();

        coord
            .map_utterance(
                &binding,
                "sess-1",
                "msg-1",
                "should never be mapped",
                &FakeLlm(add_node_json("Should not exist")),
            )
            .await
            .unwrap();

        assert!(coord.bound_map("sess-1").is_none());
        let reloaded = store
            .load_map("anonymous", "default", "map-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.revision, 0);
        assert!(reloaded.nodes.is_empty());
    }

    #[tokio::test]
    async fn apply_ignores_unregistered_session() {
        // Drive `apply` directly with a ChatMessageReceived for an unregistered
        // session — it must no-op without ever touching the LLM/store. (The
        // global router is unset in tests, so if `apply` reached the LLM step it
        // would still no-op, but the binding gate short-circuits first.)
        use magician::magician_v2::chat::models::{ChatMessage, ChatMessageContent};

        let (_tmp, coord) = coordinator();
        create_map(&coord, "map-1").await;
        // NOTE: no register() call — "sess-unknown" is not bound.

        let event = RuntimeTransportEvent::ChatMessageReceived {
            session_id: "sess-unknown".to_string(),
            message: ChatMessage {
                id: "msg-1".to_string(),
                session_id: "sess-unknown".to_string(),
                direction: ChatMessageDirection::User,
                content: ChatMessageContent::Text {
                    text: "hello".to_string(),
                    plan_reply: None,
                },
                created_at: 0,
                chat_turn_id: None,
                source_surface: None,
                presence_session_id: None,
                presentation: None,
                voice_origin: None,
                context_origin: None,
                speech_segments: None,
            },
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            origin_channel: None,
            timestamp: 0,
        };
        coord
            .apply(event)
            .await
            .expect("unregistered session no-ops");

        // Map unchanged.
        let store = ThinkingMapStore::new(coord.workspace.clone());
        let map = store
            .load_map("anonymous", "default", "map-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(map.revision, 0);
    }

    #[tokio::test]
    async fn apply_ignores_assistant_direction() {
        use magician::magician_v2::chat::models::{ChatMessage, ChatMessageContent};

        let (_tmp, coord) = coordinator();
        create_map(&coord, "map-1").await;
        coord.register("sess-1", "map-1", "anonymous", "default");

        // An assistant message in a REGISTERED session must still be ignored.
        let event = RuntimeTransportEvent::ChatMessageReceived {
            session_id: "sess-1".to_string(),
            message: ChatMessage {
                id: "msg-a".to_string(),
                session_id: "sess-1".to_string(),
                direction: ChatMessageDirection::Assistant,
                content: ChatMessageContent::Text {
                    text: "sure, here's my reply".to_string(),
                    plan_reply: None,
                },
                created_at: 0,
                chat_turn_id: None,
                source_surface: None,
                presence_session_id: None,
                presentation: None,
                voice_origin: None,
                context_origin: None,
                speech_segments: None,
            },
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            origin_channel: None,
            timestamp: 0,
        };
        coord.apply(event).await.expect("assistant message no-ops");

        let store = ThinkingMapStore::new(coord.workspace.clone());
        let map = store
            .load_map("anonymous", "default", "map-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(map.revision, 0);
    }

    #[test]
    fn is_mappable_turn_gate() {
        use ChatMessageDirection as D;
        // User turns always map.
        assert!(is_mappable_turn(&D::User, None));
        assert!(is_mappable_turn(&D::User, Some("voice")));
        // The meeting bridge: System maps ONLY with the meeting-transcript tag.
        assert!(is_mappable_turn(
            &D::System,
            Some(MEETING_TRANSCRIPT_SURFACE)
        ));
        assert!(!is_mappable_turn(&D::System, None));
        assert!(!is_mappable_turn(&D::System, Some("web")));
        // Assistant turns never map, whatever the surface.
        assert!(!is_mappable_turn(&D::Assistant, None));
        assert!(!is_mappable_turn(
            &D::Assistant,
            Some(MEETING_TRANSCRIPT_SURFACE)
        ));
    }

    #[tokio::test]
    async fn apply_accepts_meeting_transcript_system_turn() {
        // A System turn tagged `meeting-transcript` in a REGISTERED session must
        // pass the direction gate. With no global router in tests, passing the
        // gate means reaching the router step and skipping there (still Ok, map
        // untouched) — whereas a failed gate short-circuits before the binding
        // lookup. We can't observe the router skip directly, so this asserts the
        // CONTRAST: the same event with the tag stripped is provably gated (same
        // as `apply_ignores_assistant_direction`), while the gate logic itself is
        // pinned by `is_mappable_turn_gate`.
        use magician::magician_v2::chat::models::{ChatMessage, ChatMessageContent};

        let (_tmp, coord) = coordinator();
        create_map(&coord, "map-1").await;
        coord.register("meet-sess", "map-1", "anonymous", "default");

        let event = |surface: Option<&str>| RuntimeTransportEvent::ChatMessageReceived {
            session_id: "meet-sess".to_string(),
            message: ChatMessage {
                id: "msg-m1".to_string(),
                session_id: "meet-sess".to_string(),
                direction: ChatMessageDirection::System,
                content: ChatMessageContent::Text {
                    text: "Alice: we should launch the beta next week".to_string(),
                    plan_reply: None,
                },
                created_at: 0,
                chat_turn_id: None,
                source_surface: surface.map(str::to_string),
                presence_session_id: None,
                presentation: None,
                voice_origin: None,
                context_origin: None,
                speech_segments: None,
            },
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            origin_channel: None,
            timestamp: 0,
        };

        // Both are Ok (per-event errors are swallowed by design); neither can
        // mutate the map without a router. The behavioral difference under a
        // live router is pinned by the pure gate test above.
        coord
            .apply(event(Some(MEETING_TRANSCRIPT_SURFACE)))
            .await
            .expect("meeting-transcript System turn is accepted by the gate");
        coord
            .apply(event(Some("status-card")))
            .await
            .expect("other System turns still no-op");

        let store = ThinkingMapStore::new(coord.workspace.clone());
        let map = store
            .load_map("anonymous", "default", "map-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(map.revision, 0, "no router in tests ⇒ nothing applied");
    }

    #[tokio::test]
    async fn map_utterance_maps_meeting_transcript_text() {
        // The post-gate path is direction-agnostic: drive map_utterance with a
        // meeting-style speaker-prefixed line and assert it lands on the map
        // exactly like a voice/chat utterance would.
        let (_tmp, coord) = coordinator();
        create_map(&coord, "map-1").await;
        coord.register("meet-sess", "map-1", "anonymous", "default");
        let binding = coord.bound_map("meet-sess").unwrap();

        let llm = FakeLlm(add_node_json("Launch beta next week"));
        coord
            .map_utterance(
                &binding,
                "meet-sess",
                "msg-m1",
                "Alice: we should launch the beta next week",
                &llm,
            )
            .await
            .expect("map_utterance");

        let store = ThinkingMapStore::new(coord.workspace.clone());
        let map = store
            .load_map("anonymous", "default", "map-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(map.revision, 1);
        assert_eq!(map.nodes.len(), 1);
        let node = map.nodes.values().next().unwrap();
        assert_eq!(node.label, "Launch beta next week");
        let sr = node.source_refs.first().expect("source ref");
        assert_eq!(sr.thread_id, Some("meet-sess".to_string()));
    }

    #[test]
    fn resolve_binding_matches_presence_session_id_fallback() {
        let (_tmp, coord) = coordinator();
        // Attach under the MEDIA/voice session id — what the iOS
        // `RealtimeVoiceClient` knows (it never sees the chat session id the
        // voice orchestrator derives server-side).
        coord.register("media-sess", "map-1", "anonymous", "default");

        // Direct match on the registered id (the chat/web attach path).
        let (binding, matched) = coord
            .resolve_binding("media-sess", None)
            .expect("direct match on the registered id");
        assert_eq!(binding.map_id, "map-1");
        assert_eq!(matched, "media-sess");

        // A voice-originated event: `session_id` = the (unregistered) CHAT
        // session id, `presence_session_id` = the registered MEDIA id ⇒ matched
        // via the presence fallback, threaded under the media id.
        let (binding, matched) = coord
            .resolve_binding("chat-sess-xyz", Some("media-sess"))
            .expect("match via the presence_session_id fallback");
        assert_eq!(binding.map_id, "map-1");
        assert_eq!(
            matched, "media-sess",
            "the fallback threads under the id that matched (the media id)"
        );

        // Neither id registered ⇒ no match.
        assert!(coord
            .resolve_binding("chat-sess-xyz", Some("other-media"))
            .is_none());
        assert!(coord.resolve_binding("chat-sess-xyz", None).is_none());
    }
}
