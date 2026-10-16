//! Per-chat-turn event store + live broadcast.
//!
//! This sink is the **single filter applier** for "events that belong
//! to a chat turn." Both readers (REST refresh, live SSE) consume what
//! the sink projects — neither re-applies the predicate, so drift
//! between live and refresh is structurally impossible.
//!
//! Pipeline:
//!   transport bus
//!         │ (every event)
//!         ▼
//!   ChatTurnEventSink::apply()
//!         │
//!         │  extract_chat_turn_meta() — the ONLY place this
//!         │  "is this chat-bound?" decision is made
//!         │
//!         ├── persistence ─► <scope>/ui/chat_turn_events/<cid>.jsonl
//!         │                    │
//!         │                    └── REST `/api/.../turns/<cid>/events`
//!         │                        reads this file
//!         │
//!         └── live broadcast ─► tokio::sync::broadcast::Sender<(String, Value)>
//!                              │
//!                              └── SSE `/api/.../turns/<cid>/events/stream`
//!                                  subscribes via `sink.subscribe_live()`,
//!                                  filters by the requested chat_turn_id,
//!                                  forwards as NDJSON
//!
//! The transport bus is no longer the source for the chat activity
//! card's live updates — it's strictly upstream of the sink. The
//! generic `/api/magician/v3/events` SSE endpoint goes back to serving
//! debug/observability surfaces with no chat coupling.

use std::sync::Arc;

use tokio::sync::broadcast::{self, error::RecvError, Receiver};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::realtime_events::{
    ChatFanoutResolver, RuntimeTransportBroadcaster, RuntimeTransportEvent,
};

/// Bounded broadcast capacity for live subscribers. The channel is
/// shared across all chat-turn SSE consumers, so a single bursty
/// turn (e.g. a long LLM stream firing reasoning deltas faster than
/// any one subscriber drains) can push the channel head past the
/// slowest subscriber's position, causing **other turns' subscribers
/// to also receive `Lagged`** even though their own turn is idle.
///
/// 8192 absorbs a multi-minute stream of fine-grained reasoning
/// events without forcing the broadcaster to drop. If lag warnings
/// start firing under realistic load the next move is per-turn
/// channels (a `DashMap<turn_id, broadcast::Sender>` populated
/// lazily) so one busy turn cannot starve others.
const LIVE_BROADCAST_CAPACITY: usize = 8192;

/// Tuple shipped to live subscribers:
/// `(chat_turn_id, principal, workspace, serialized_event_json)`.
///
/// `principal` and `workspace` travel alongside the payload so the
/// SSE handler can enforce scope on every delivered row, even though
/// it already scope-checked the session at subscribe time. Defense
/// in depth — without this, a chat_turn_id collision across tenants
/// (UUIDs are not guaranteed unique outside their generator) would
/// leak events across scopes via the single shared broadcast channel.
pub type LiveTurnEventTuple = (String, String, String, String);

pub struct ChatTurnEventSink {
    workspace: ArtifactV2Workspace,
    live_tx: broadcast::Sender<LiveTurnEventTuple>,
}

impl ChatTurnEventSink {
    /// Construct the sink as a long-lived `Arc`. Returned handle is
    /// shared between the bus-loop task (created by `spawn`) and the
    /// HTTP handler that subscribes for live SSE.
    pub fn new(workspace: ArtifactV2Workspace) -> Arc<Self> {
        let (live_tx, _) = broadcast::channel(LIVE_BROADCAST_CAPACITY);
        Arc::new(Self { workspace, live_tx })
    }

    /// Spawn the bus-subscription loop. Returns the `JoinHandle` for
    /// shutdown signalling. The sink itself outlives the task (held
    /// by the HTTP handler via `Arc`); dropping the JoinHandle just
    /// detaches the loop, which exits cleanly when the broadcaster's
    /// senders all drop.
    ///
    /// Subscribes synchronously **before** spawning the future so any
    /// event emitted in the window between this call returning and
    /// the runtime polling the spawned task lands in the receiver's
    /// buffer instead of being dropped. Calling `subscribe()` inside
    /// the future leaves a race where the first burst of a fast turn
    /// can vanish.
    pub fn spawn(self: Arc<Self>, broadcaster: Arc<RuntimeTransportBroadcaster>) -> JoinHandle<()> {
        let rx = broadcaster.subscribe();
        // Hold a cycle-safe resolver (registry-map Arcs only), NOT the
        // broadcaster itself — pinning the broadcaster here would keep its
        // `broadcast::Sender` alive and stop `run` from ever observing a
        // closed channel on shutdown.
        let resolver = broadcaster.chat_fanout_resolver();
        tokio::spawn(async move { self.run(rx, resolver).await })
    }

    /// Subscribe for live event tuples. Used by the chat-turn SSE
    /// endpoint; consumers filter by `(chat_turn_id, principal,
    /// workspace)` and forward.
    pub fn subscribe_live(&self) -> broadcast::Receiver<LiveTurnEventTuple> {
        self.live_tx.subscribe()
    }

    /// `apply()` is single-task by construction — only the loop in
    /// `run()` calls it. Per-chat-turn JSONL writes therefore never
    /// interleave, even though the file is opened in `append` mode
    /// per call. Do NOT call `apply` from a parallel task; that would
    /// require a per-path mutex to keep the file parseable.
    async fn run(
        self: Arc<Self>,
        mut rx: Receiver<RuntimeTransportEvent>,
        resolver: ChatFanoutResolver,
    ) {
        info!("[chat-turn-event-sink] subscribed to transport bus");
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Err(error) = self.apply(event, &resolver).await {
                        warn!(
                            error = %error,
                            "[chat-turn-event-sink] failed to project event"
                        );
                    }
                },
                Err(RecvError::Lagged(skipped)) => {
                    warn!(
                        skipped,
                        "[chat-turn-event-sink] broadcast lagged — activity card may show gaps"
                    );
                },
                Err(RecvError::Closed) => {
                    info!("[chat-turn-event-sink] broadcast closed, exiting");
                    break;
                },
            }
        }
    }

    async fn apply(
        &self,
        event: RuntimeTransportEvent,
        resolver: &ChatFanoutResolver,
    ) -> anyhow::Result<()> {
        // App owner notification context carries display-only task/execution
        // ids. The generic recursive fanout resolver below would otherwise
        // mistake those nested values for routing authority and persist the
        // full prompt into a per-chat-turn JSONL file.
        if crate::magician_v2::realtime_events::is_app_owner_notification_transport_event(&event) {
            return Ok(());
        }
        // We rely on the serialized form to find both the id and the
        // scope (the variants are wildly heterogeneous — payload,
        // envelope, nested message — and a centralized "where does
        // chat_turn_id live" lookup would mean a giant match keeping
        // pace with every new variant. Pulling from serialized JSON
        // is robust to that drift.)
        let serialized =
            serde_json::to_string(&event).map_err(|e| anyhow::anyhow!("serialize event: {}", e))?;

        // Primary path: the event already carries a chat_turn_id.
        if let Some(meta) = extract_chat_turn_meta(&serialized) {
            self.persist_and_broadcast(
                &meta.chat_turn_id,
                &meta.principal,
                &meta.workspace,
                &serialized,
            )
            .await?;
            return Ok(());
        }

        // Recovery path: the event has no chat_turn_id. Inner-loop
        // progress events (typed `Agentic*` / tool / llm transport
        // variants emitted via `emit` / `emit_transport_only`) bypass the
        // `emit_scoped_or_unscoped` chat fan-out re-stamp, so a delegated
        // task the chat SUBSCRIBED to would otherwise be silently dropped
        // here — which is exactly why a subscribed run showed no live
        // updates. If the event's task (directly via `task_id`, or via
        // its `execution_id`'s registered scope) has a chat fan-out,
        // recover the turn id(s), re-stamp into the payload, and route as
        // usual. Gated on `has_fanout()` so the common no-subscription
        // path stays a single atomic check.
        if !resolver.has_fanout() {
            return Ok(());
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&serialized) else {
            return Ok(());
        };
        let task_id = find_string_field(&value, "task_id");
        let execution_id = find_string_field(&value, "execution_id");
        if task_id.is_none() && execution_id.is_none() {
            return Ok(());
        }
        for (chat_turn_id, principal, workspace) in
            resolver.resolve_turns(task_id.as_deref(), execution_id.as_deref())
        {
            // Re-stamp the recovered turn id + scope into the payload so
            // consumers that re-filter by these fields stay consistent
            // with the file/stream routing.
            let stamped = stamp_chat_scope(&value, &chat_turn_id, &principal, &workspace);
            self.persist_and_broadcast(&chat_turn_id, &principal, &workspace, &stamped)
                .await?;
        }
        Ok(())
    }

    /// Ship one already-routed event to live SSE subscribers, then append
    /// it to the per-turn JSONL.
    ///
    /// The live broadcast happens BEFORE the file write so a transient
    /// disk failure (ENOSPC / EIO) doesn't silently drop the event from
    /// the live tail as well. The persisted file is the authoritative
    /// source for refresh-view; the broadcast is a UX optimization.
    async fn persist_and_broadcast(
        &self,
        chat_turn_id: &str,
        principal: &str,
        workspace: &str,
        serialized: &str,
    ) -> anyhow::Result<()> {
        let _ = self.live_tx.send((
            chat_turn_id.to_owned(),
            principal.to_owned(),
            workspace.to_owned(),
            serialized.to_owned(),
        ));
        let path = self
            .workspace
            .chat_turn_events_path(principal, workspace, chat_turn_id);
        let mut line = serialized.as_bytes().to_vec();
        line.push(b'\n');
        self.workspace.append_path(&path, &line).await?;
        Ok(())
    }
}

struct ChatTurnMeta {
    chat_turn_id: String,
    principal: String,
    workspace: String,
}

/// Extracts `(chat_turn_id, principal, workspace)` from a serialized
/// RuntimeTransportEvent JSON line. Returns `None` when any field is
/// missing — the event is then ignored (e.g., internal heartbeats,
/// unscoped diagnostics, events that aren't chat-bound).
///
/// Scope (principal/workspace) is read from **known envelope
/// locations only** — never a deep scan. `RuntimeTransportEvent`
/// serializes with `#[serde(tag = "event_type", content = "data")]`
/// so every variant's payload lives under `data.*`, and the scope
/// fields land in one of these places depending on the variant:
///   * `data.principal` / `data.workspace` — variants that name the
///     fields directly (`MessageProcessingStarted`,
///     `ChatMessageReceived`, …)
///   * `data.event.principal` / `data.event.workspace` —
///     `AgentEvent { event: AgentEventEnvelope }`
///   * `data.message.principal` / `data.message.workspace` —
///     `ProgressEvent { message: ProgressMessage }`
///
/// Why no deep scan: an `AgentEvent` envelope for a delegated
/// sub-agent stamps the SUB-AGENT'S scope at the envelope, but its
/// inner payload may also carry a `principal` / `workspace` field
/// from some context object. A deep scan walking JSON object
/// children in non-deterministic order could pick up either one,
/// silently misrouting the per-turn file (e.g. writing to
/// `system/system/ui/chat_turn_events/<turn>.jsonl` when the chat
/// session expects `owner/personal/...`). Restricting to envelope
/// locations makes the routing canonical and matches the scope the
/// persistence sink (`transport_log::route_event_to_scope`) uses for
/// the same event.
///
/// `chat_turn_id` is still extracted via a deep scan because it's a
/// payload property, not envelope metadata — it can land anywhere
/// inside `data.event.payload`, `metadata`, or hand-crafted
/// `serde_json::json!({...})` blobs.
fn extract_chat_turn_meta(serialized: &str) -> Option<ChatTurnMeta> {
    let value: serde_json::Value = serde_json::from_str(serialized).ok()?;
    let principal = envelope_scope_field(&value, "principal")?;
    let workspace = envelope_scope_field(&value, "workspace")?;
    let chat_turn_id = find_string_field(&value, "chat_turn_id")?;
    Some(ChatTurnMeta {
        chat_turn_id,
        principal,
        workspace,
    })
}

/// Reads `name` from the envelope locations enumerated in
/// `extract_chat_turn_meta`. All variants of `RuntimeTransportEvent`
/// nest their payload under `data` (serde tag/content), so the
/// `data.*` lookups are the load-bearing ones. The top-level fallback
/// covers any future variant that hoists scope onto the outer object.
fn envelope_scope_field(value: &serde_json::Value, name: &str) -> Option<String> {
    if let Some(s) = non_empty_string(value.get(name)) {
        return Some(s);
    }
    let data = value.get("data")?;
    if let Some(s) = non_empty_string(data.get(name)) {
        return Some(s);
    }
    if let Some(event) = data.get("event") {
        if let Some(s) = non_empty_string(event.get(name)) {
            return Some(s);
        }
    }
    if let Some(message) = data.get("message") {
        if let Some(s) = non_empty_string(message.get(name)) {
            return Some(s);
        }
    }
    None
}

fn non_empty_string(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn find_string_field(value: &serde_json::Value, name: &str) -> Option<String> {
    use serde_json::Value::*;
    let mut stack = vec![value];
    while let Some(node) = stack.pop() {
        match node {
            Object(map) => {
                if let Some(String(s)) = map.get(name) {
                    if !s.is_empty() {
                        return Some(s.clone());
                    }
                }
                for (_, child) in map {
                    stack.push(child);
                }
            },
            Array(items) => {
                for child in items {
                    stack.push(child);
                }
            },
            _ => {},
        }
    }
    None
}

/// Re-stamp the chat routing fields (`chat_turn_id`, `principal`,
/// `workspace`) onto a serialized event recovered via the fan-out
/// registry, so both the live tuple AND the persisted line resolve to
/// the subscribed chat turn + scope on a later refresh-view read (typed
/// inner-loop variants may carry no scope fields at all).
/// `RuntimeTransportEvent` serializes with
/// `#[serde(tag = "event_type", content = "data")]`, so the fields are
/// placed inside the `data` object — the envelope/deep-scan locations
/// `extract_chat_turn_meta` reads; falls back to the top-level object for
/// any future tagless shape.
fn stamp_chat_scope(
    value: &serde_json::Value,
    chat_turn_id: &str,
    principal: &str,
    workspace: &str,
) -> String {
    let mut v = value.clone();
    if let Some(data) = v.get_mut("data").and_then(|d| d.as_object_mut()) {
        insert_chat_scope(data, chat_turn_id, principal, workspace);
    } else if let Some(obj) = v.as_object_mut() {
        insert_chat_scope(obj, chat_turn_id, principal, workspace);
    }
    v.to_string()
}

fn insert_chat_scope(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    chat_turn_id: &str,
    principal: &str,
    workspace: &str,
) {
    obj.insert(
        "chat_turn_id".to_string(),
        serde_json::Value::String(chat_turn_id.to_owned()),
    );
    obj.insert(
        "principal".to_string(),
        serde_json::Value::String(principal.to_owned()),
    );
    obj.insert(
        "workspace".to_string(),
        serde_json::Value::String(workspace.to_owned()),
    );
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    // `RuntimeTransportEvent` serializes with `tag = "event_type",
    // content = "data"`, so every variant's body lives under `data.*`.
    // These tests pin that contract: a scope extractor that forgets
    // the `data` wrapper rejects every event and the activity card
    // silently shows zero. (That regression took down the live
    // activity tail once already.)

    #[test]
    fn extracts_scope_from_agent_event_envelope() {
        let serialized = serde_json::json!({
            "event_type": "AgentEvent",
            "data": {
                "event": {
                    "event_type": "tool.call.started",
                    "agent_id": "personal-assistant",
                    "principal": "owner",
                    "workspace": "personal",
                    "payload": {
                        "event_id": "evt-1",
                        "chat_turn_id": "turn-abc",
                        "timestamp_ms": 1700000000000_i64,
                    },
                    "timestamp": 1700000000000_i64,
                }
            }
        })
        .to_string();
        let meta = extract_chat_turn_meta(&serialized).expect("agent event scope");
        assert_eq!(meta.principal, "owner");
        assert_eq!(meta.workspace, "personal");
        assert_eq!(meta.chat_turn_id, "turn-abc");
    }

    #[test]
    fn extracts_scope_from_progress_event_message() {
        let serialized = serde_json::json!({
            "event_type": "ProgressEvent",
            "data": {
                "message": {
                    "id": "msg-1",
                    "principal": "owner",
                    "workspace": "personal",
                    "metadata": { "chat_turn_id": "turn-xyz" },
                },
                "timestamp": 1700000000000_i64,
            }
        })
        .to_string();
        let meta = extract_chat_turn_meta(&serialized).expect("progress event scope");
        assert_eq!(meta.principal, "owner");
        assert_eq!(meta.workspace, "personal");
        assert_eq!(meta.chat_turn_id, "turn-xyz");
    }

    #[test]
    fn rejects_event_with_no_chat_turn_id() {
        let serialized = serde_json::json!({
            "event_type": "AgentEvent",
            "data": {
                "event": {
                    "event_type": "system.heartbeat",
                    "agent_id": "__system__",
                    "principal": "owner",
                    "workspace": "personal",
                    "payload": {},
                    "timestamp": 0_i64,
                }
            }
        })
        .to_string();
        assert!(extract_chat_turn_meta(&serialized).is_none());
    }

    #[test]
    fn rejects_event_with_empty_scope() {
        let serialized = serde_json::json!({
            "event_type": "AgentEvent",
            "data": {
                "event": {
                    "event_type": "x",
                    "agent_id": "a",
                    "principal": "",
                    "workspace": "",
                    "payload": { "chat_turn_id": "t" },
                    "timestamp": 0_i64,
                }
            }
        })
        .to_string();
        assert!(extract_chat_turn_meta(&serialized).is_none());
    }
}
