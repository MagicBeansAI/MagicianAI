//! Emitter helper for [`AgentUpdate`] events.
//!
//! Publishes `AgentUpdate` on the shared [`RuntimeTransportBroadcaster`] with
//! wire `event_type = "agent.update"`. Subscribers (WebSocket clients,
//! JSONL journal) filter on that event type.
//!
//! This is the only sanctioned way to emit `AgentUpdate` events from
//! application code. Direct construction of the envelope here keeps the wire
//! contract in one place.

use std::sync::Arc;

use serde_json::json;

use super::agent_update::AgentUpdate;
use crate::magician_v2::realtime_events::{AgentEventEnvelope, RuntimeTransportBroadcaster};

/// Wire event type for agent update events.
pub const AGENT_UPDATE_EVENT_TYPE: &str = "agent.update";

/// Publish an [`AgentUpdate`] on the shared broadcaster.
///
/// `principal` and `workspace` are placed on the outer envelope for
/// scope-based routing (WebSocket visibility filter, per-workspace JSONL
/// journal). They are NOT duplicated into the `AgentUpdate` payload — the
/// payload's `workspace_id` is the in-body copy; the envelope's `workspace`
/// is the routing key.
/// Build the `AgentUpdate` envelope without sending it, so a caller holding a
/// phase address can journal it instead.
///
/// # Census sites 20 and 21
///
/// `execution::agentic::run_loop::phases::outbox`'s *WHAT IS NOT JOURNALLED*
/// listed two emissions through this function —
/// `capture_pack_action_artifacts_projection`'s `ArtifactCreateFailed` and its
/// `ArtifactCreated` — as reaching a transport and recording nothing. What
/// they were owed was this split: [`publish_agent_update`] below built its
/// envelope and sent it in one call, so there was no envelope for a caller to
/// take.
///
/// Site 21 sits inside the same `if let (Some(sink), Some(scope))` block as
/// census site 8, whose canonical `ArtifactCreated` write is REFUSED because it
/// is already durable and a replay would append a second record for one
/// artifact. **That refusal does not cover this envelope.** The two answers
/// differ because the destinations do: site 8 goes to the execution's
/// `events.jsonl`, while this one is an ephemeral operator-feed send that a
/// crash loses outright.
///
/// `None` on a serialization failure, which is the same outcome the emitting
/// form has always had — it logs and drops rather than panicking, because
/// emission must not crash the emitter path.
pub fn agent_update_envelope(
    principal: Option<&str>,
    workspace: Option<&str>,
    update: &AgentUpdate,
) -> Option<AgentEventEnvelope> {
    let payload = match serde_json::to_value(update) {
        Ok(v) => v,
        Err(err) => {
            // Serialization of a typed AgentUpdate should be infallible. Log
            // and drop rather than panic — emission must not crash the
            // emitter path.
            tracing::error!(
                error = %err,
                update_id = %update.id,
                "failed to serialize AgentUpdate; dropping event"
            );
            return None;
        },
    };

    let agent_id = update.agent_id.clone();
    Some(match (principal, workspace) {
        (Some(p), Some(w)) => {
            AgentEventEnvelope::new_scoped(AGENT_UPDATE_EVENT_TYPE, &agent_id, p, w, payload)
        },
        _ => AgentEventEnvelope::new(AGENT_UPDATE_EVENT_TYPE, &agent_id, payload),
    })
}

pub fn publish_agent_update(
    broadcaster: &Arc<RuntimeTransportBroadcaster>,
    principal: Option<&str>,
    workspace: Option<&str>,
    update: AgentUpdate,
) {
    if let Some(envelope) = agent_update_envelope(principal, workspace, &update) {
        broadcaster.emit_agent_transport_event(envelope);
    }
}

/// Convenience: build an envelope payload directly from an `AgentUpdate`
/// without emitting. Useful in tests and for synthetic fixtures.
pub fn envelope_for(update: &AgentUpdate) -> serde_json::Value {
    serde_json::to_value(update).unwrap_or_else(|_| json!({}))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::agent_update::{AgentUpdateKind, CycleOutcome};
    use crate::magician_v2::realtime_events::RuntimeTransportEvent;

    #[tokio::test]
    async fn publish_sends_envelope_with_expected_event_type() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut rx = broadcaster.subscribe();

        let update = AgentUpdate::new(
            "ws_a".to_string(),
            "cfo".to_string(),
            AgentUpdateKind::CycleCompleted {
                outcome: CycleOutcome::Succeeded,
                duration_ms: 1234,
            },
        );
        let update_id = update.id.clone();

        publish_agent_update(&broadcaster, Some("alpha"), Some("ws_a"), update);

        let event = rx.try_recv().expect("event should be delivered");
        match event {
            RuntimeTransportEvent::AgentEvent { event } => {
                assert_eq!(event.event_type, AGENT_UPDATE_EVENT_TYPE);
                assert_eq!(event.agent_id, "cfo");
                assert_eq!(event.principal.as_deref(), Some("alpha"));
                assert_eq!(event.workspace.as_deref(), Some("ws_a"));
                let kind = event
                    .payload
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .expect("payload has kind");
                assert_eq!(kind, "cycle_completed");
                let id = event
                    .payload
                    .get("id")
                    .and_then(|v| v.as_str())
                    .expect("payload has id");
                assert_eq!(id, update_id);
            },
            other => panic!("expected AgentEvent, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn publish_without_scope_uses_unscoped_envelope() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut rx = broadcaster.subscribe();

        let update = AgentUpdate::new(
            "ws_a".to_string(),
            "cfo".to_string(),
            AgentUpdateKind::AgentResumed,
        );
        publish_agent_update(&broadcaster, None, None, update);

        let event = rx.try_recv().expect("event should be delivered");
        match event {
            RuntimeTransportEvent::AgentEvent { event } => {
                assert_eq!(event.event_type, AGENT_UPDATE_EVENT_TYPE);
                assert_eq!(event.principal, None);
                assert_eq!(event.workspace, None);
            },
            other => panic!("expected AgentEvent, got {:?}", other),
        }
    }
}
