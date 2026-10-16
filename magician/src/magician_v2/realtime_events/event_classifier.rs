//! Shared classification of serialized transport events.
//!
//! One home for the rules that `/v3/events` (`magician-api::events_api`) and
//! the realtime WebSocket both apply to a live `RuntimeTransportEvent`:
//!
//! - the canonical event type (`AgentEvent` envelopes unwrap to
//!   `data.event.event_type`),
//! - the operator taxonomy (`category` / `user_relevant`), looked up on that
//!   unwrapped type first and falling back to the variant's own taxonomy,
//! - the correlation `refs` a client filters on.
//!
//! Everything reads the serialized `serde_json::Value` form so the two
//! surfaces classify the exact bytes they send.

use serde::Serialize;
use serde_json::Value;

use super::{lookup_agent_event_taxonomy, EventCategory, EventTaxonomy, RuntimeTransportEvent};

/// The canonical `event_type` of a serialized `RuntimeTransportEvent`. For
/// `AgentEvent` envelopes the outer tag is always `"AgentEvent"` and the
/// meaningful type lives at `data.event.event_type`; other variants carry it
/// at the outer level (the serde tag).
pub fn transport_event_type_of(value: &Value) -> String {
    let outer = value
        .get("event_type")
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    if outer == "AgentEvent" {
        if let Some(inner) = value
            .pointer("/data/event/event_type")
            .and_then(|t| t.as_str())
        {
            return inner.to_string();
        }
    }
    outer.to_string()
}

/// The `agent_id` `/v3/events` filters on: `data.event.agent_id` for
/// `AgentEvent` envelopes, else a top-level `agent_id` when present.
pub fn transport_event_agent_id_of(value: &Value) -> Option<String> {
    if let Some(inner) = value
        .pointer("/data/event/agent_id")
        .and_then(|a| a.as_str())
    {
        return Some(inner.to_string());
    }
    value
        .get("agent_id")
        .and_then(|a| a.as_str())
        .map(ToOwned::to_owned)
}

/// The serde wire token of an [`EventCategory`] — the token `/v3/events`
/// accepts in `?category=`. Exhaustive, so a new category fails to compile
/// here until it is given a token.
pub fn event_category_wire_token(category: EventCategory) -> &'static str {
    match category {
        EventCategory::Pipeline => "pipeline",
        EventCategory::Plan => "plan",
        EventCategory::Tool => "tool",
        EventCategory::Slot => "slot",
        EventCategory::Clarification => "clarification",
        EventCategory::Execution => "execution",
        EventCategory::Llm => "llm",
        EventCategory::Agentic => "agentic",
        EventCategory::Hitl => "hitl",
        EventCategory::Agent => "agent",
        EventCategory::Task => "task",
        EventCategory::Feed => "feed",
        EventCategory::Observability => "observability",
        EventCategory::Media => "media",
        EventCategory::Activity => "activity",
    }
}

/// The taxonomy one live event classifies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportEventClassification {
    pub taxonomy: EventTaxonomy,
    /// `true` when the unwrapped event type is in the shared taxonomy table.
    /// `/v3/events` soft-passes taxonomy filters for unrecognized types (a
    /// new emit site is never silently dropped under a category filter); the
    /// taxonomy then is the variant's own catch-all.
    pub recognized: bool,
}

impl TransportEventClassification {
    /// The `/v3/events` category tokens this event matches.
    pub fn categories(&self) -> Vec<&'static str> {
        vec![event_category_wire_token(self.taxonomy.category)]
    }

    pub fn user_relevant(&self) -> bool {
        self.taxonomy.user_relevant
    }
}

/// Classify a live event exactly as the `/v3/events` live tail does:
///   1. the unwrapped event type in the shared table (AGUI envelope types
///      such as `tool.call.*`, and every enum variant name);
///   2. otherwise the variant's `taxonomy()` (for `AgentEvent`, the
///      catch-all `Agent / Info / false`), marked unrecognized.
///
/// `value` must be `serde_json::to_value(event)`.
pub fn classify_transport_event(
    event: &RuntimeTransportEvent,
    value: &Value,
) -> TransportEventClassification {
    match lookup_agent_event_taxonomy(&transport_event_type_of(value)) {
        Some(taxonomy) => TransportEventClassification {
            taxonomy,
            recognized: true,
        },
        None => TransportEventClassification {
            taxonomy: event.taxonomy(),
            recognized: false,
        },
    }
}

/// Correlation ids a realtime client filters on, pulled from wherever the
/// event carries them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TransportEventRefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Places a ref can live in a serialized event, most specific first: the
/// `AgentEvent` payload, the `AgentEvent` envelope, the variant's own fields,
/// then a nested chat `message` (for `ChatMessageReceived`).
const REF_CONTAINERS: &[&str] = &[
    "/data/event/payload",
    "/data/event",
    "/data",
    "/data/message",
];

fn first_ref(value: &Value, keys: &[&str]) -> Option<String> {
    for container in REF_CONTAINERS {
        let Some(object) = value.pointer(container).and_then(Value::as_object) else {
            continue;
        };
        for key in keys {
            if let Some(found) = object
                .get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|found| !found.is_empty())
            {
                return Some(found.to_string());
            }
        }
    }
    None
}

/// Extract `{task_id, execution_id, chat_turn_id, agent_id, session_id}` from
/// a serialized event. `session_id` also accepts `chat_session_id`, the name
/// AgentEvent payloads and the LLM/thinking-mode variants use for the chat
/// session.
///
/// `value` must be `serde_json::to_value(event)`.
pub fn extract_transport_event_refs(value: &Value) -> TransportEventRefs {
    TransportEventRefs {
        task_id: first_ref(value, &["task_id"]),
        execution_id: first_ref(value, &["execution_id"]),
        chat_turn_id: first_ref(value, &["chat_turn_id"]),
        agent_id: first_ref(value, &["agent_id"]),
        session_id: first_ref(value, &["session_id", "chat_session_id"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::realtime_events::{AgentEventEnvelope, EventSeverity};

    fn value_of(event: &RuntimeTransportEvent) -> Value {
        serde_json::to_value(event).expect("event serializes")
    }

    #[test]
    fn every_category_token_is_its_serde_wire_name() {
        for category in [
            EventCategory::Pipeline,
            EventCategory::Plan,
            EventCategory::Tool,
            EventCategory::Slot,
            EventCategory::Clarification,
            EventCategory::Execution,
            EventCategory::Llm,
            EventCategory::Agentic,
            EventCategory::Hitl,
            EventCategory::Agent,
            EventCategory::Task,
            EventCategory::Feed,
            EventCategory::Observability,
            EventCategory::Media,
            EventCategory::Activity,
        ] {
            assert_eq!(
                serde_json::to_value(category).expect("category serializes"),
                Value::String(event_category_wire_token(category).to_string())
            );
        }
    }

    #[test]
    fn agent_event_classifies_on_its_inner_type() {
        let event = RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "tool.call.started",
                "agent-1",
                serde_json::json!({"execution_id": "exec-1"}),
            ),
        };
        let value = value_of(&event);
        assert_eq!(transport_event_type_of(&value), "tool.call.started");
        let classification = classify_transport_event(&event, &value);
        assert!(classification.recognized);
        assert_eq!(
            Some(classification.taxonomy),
            lookup_agent_event_taxonomy("tool.call.started")
        );
    }

    #[test]
    fn unknown_agent_event_type_falls_back_to_the_variant_taxonomy() {
        let event = RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "never.registered.event",
                "agent-1",
                serde_json::json!({}),
            ),
        };
        let value = value_of(&event);
        let classification = classify_transport_event(&event, &value);
        assert!(!classification.recognized);
        assert_eq!(classification.taxonomy, event.taxonomy());
        assert_eq!(classification.taxonomy.category, EventCategory::Agent);
        assert_eq!(classification.taxonomy.severity, EventSeverity::Info);
        assert!(!classification.user_relevant());
        assert_eq!(classification.categories(), vec!["agent"]);
    }

    #[test]
    fn plain_variant_classifies_on_its_variant_name() {
        let event = RuntimeTransportEvent::Heartbeat { timestamp: 1 };
        let value = value_of(&event);
        assert_eq!(transport_event_type_of(&value), "Heartbeat");
        let classification = classify_transport_event(&event, &value);
        assert_eq!(classification.taxonomy, event.taxonomy());
    }

    #[test]
    fn refs_come_from_the_agent_event_payload_and_envelope() {
        let event = RuntimeTransportEvent::AgentEvent {
            event: AgentEventEnvelope::new(
                "reasoning.content",
                "agent-7",
                serde_json::json!({
                    "task_id": "task-1",
                    "execution_id": "exec-1",
                    "chat_turn_id": "turn-1",
                    "chat_session_id": "session-1",
                }),
            ),
        };
        let refs = extract_transport_event_refs(&value_of(&event));
        assert_eq!(
            refs,
            TransportEventRefs {
                task_id: Some("task-1".to_string()),
                execution_id: Some("exec-1".to_string()),
                chat_turn_id: Some("turn-1".to_string()),
                agent_id: Some("agent-7".to_string()),
                session_id: Some("session-1".to_string()),
            }
        );
    }

    #[test]
    fn refs_come_from_variant_fields() {
        let event = RuntimeTransportEvent::AgentCycleCompleted {
            principal: None,
            workspace: None,
            agent_id: "agent-1".to_string(),
            goal_id: "goal-1".to_string(),
            cycle_id: "cycle-1".to_string(),
            execution_id: Some("exec-9".to_string()),
            outcome: "success".to_string(),
            iterations_used: 1,
            timestamp: 1,
        };
        let refs = extract_transport_event_refs(&value_of(&event));
        assert_eq!(refs.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(refs.execution_id.as_deref(), Some("exec-9"));
        assert_eq!(refs.task_id, None);
        assert_eq!(
            serde_json::to_value(&refs).expect("refs serialize"),
            serde_json::json!({"agent_id": "agent-1", "execution_id": "exec-9"})
        );
    }

    #[test]
    fn heartbeat_has_no_refs() {
        let refs = extract_transport_event_refs(&value_of(&RuntimeTransportEvent::Heartbeat {
            timestamp: 1,
        }));
        assert_eq!(refs, TransportEventRefs::default());
        assert_eq!(
            serde_json::to_value(&refs).expect("refs serialize"),
            serde_json::json!({})
        );
    }
}
