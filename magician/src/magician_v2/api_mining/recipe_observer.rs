//! Structured lifecycle observation for Task Recipe replay.
//!
//! The runner stays independent of Artifact v2. Hosts that own a canonical
//! execution journal install [`EventJournalObserver`]; other replay entry
//! points can omit the observer without changing execution semantics.

use std::collections::HashMap;
use std::sync::Arc;

use magician_event_taxonomy::RuntimeAgentEventType;
use serde_json::{json, Value};

use super::recipe::Transport;
use super::recipe_runner::FailureClass;
use crate::magician_v2::artifact_v2::events::{
    ArtifactV2EventType, CanonicalEventScope, RuntimeCanonicalEventSink,
};

pub enum RecipeEvent<'a> {
    Started {
        recipe_id: &'a str,
        version: u32,
        inputs: &'a HashMap<String, String>,
    },
    StepCompleted {
        step_id: &'a str,
        status: u16,
        duration_ms: u64,
        transport: Transport,
    },
    StepFailed {
        step_id: &'a str,
        class: FailureClass,
        detail: &'a str,
    },
    AuthHealed {
        origin: &'a str,
    },
    TransportDowngraded {
        step_id: &'a str,
        to: Transport,
    },
    ApprovalRequested {
        step_id: &'a str,
        request_id: &'a str,
    },
    ApprovalResolved {
        step_id: &'a str,
        decision: &'a str,
    },
    FallbackHandoff {
        step_id: &'a str,
        class: FailureClass,
        replayed_steps: usize,
    },
    Completed {
        steps: usize,
        duration_ms: u64,
    },
    Recompiled {
        recipe_id: &'a str,
        version: u32,
    },
}

impl RecipeEvent<'_> {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Started { .. } => RuntimeAgentEventType::RecipeReplayStarted.as_str(),
            Self::StepCompleted { .. } => RuntimeAgentEventType::RecipeReplayStepCompleted.as_str(),
            Self::StepFailed { .. } => RuntimeAgentEventType::RecipeReplayStepFailed.as_str(),
            Self::AuthHealed { .. } => RuntimeAgentEventType::RecipeReplayAuthHealed.as_str(),
            Self::TransportDowngraded { .. } => {
                RuntimeAgentEventType::RecipeReplayTransportDowngraded.as_str()
            },
            Self::ApprovalRequested { .. } => {
                RuntimeAgentEventType::RecipeReplayApprovalRequested.as_str()
            },
            Self::ApprovalResolved { .. } => {
                RuntimeAgentEventType::RecipeReplayApprovalResolved.as_str()
            },
            Self::FallbackHandoff { .. } => {
                RuntimeAgentEventType::RecipeReplayFallbackHandoff.as_str()
            },
            Self::Completed { .. } => RuntimeAgentEventType::RecipeReplayCompleted.as_str(),
            Self::Recompiled { .. } => RuntimeAgentEventType::RecipeReplayRecompiled.as_str(),
        }
    }

    fn payload(&self) -> Value {
        match self {
            Self::Started {
                recipe_id,
                version,
                inputs,
            } => {
                // Input values may be credentials or other sensitive task
                // material. The lifecycle journal needs only field names.
                let mut input_names = inputs.keys().collect::<Vec<_>>();
                input_names.sort_unstable();
                json!({
                    "kind": self.kind(),
                    "recipe_id": recipe_id,
                    "version": version,
                    "input_names": input_names,
                })
            },
            Self::StepCompleted {
                step_id,
                status,
                duration_ms,
                transport,
            } => json!({
                "kind": self.kind(),
                "step_id": step_id,
                "status": status,
                "duration_ms": duration_ms,
                "transport": transport,
            }),
            Self::StepFailed {
                step_id,
                class,
                detail: _,
            } => json!({
                "kind": self.kind(),
                "step_id": step_id,
                "class": class,
            }),
            Self::AuthHealed { origin } => json!({
                "kind": self.kind(),
                "origin": origin,
            }),
            Self::TransportDowngraded { step_id, to } => json!({
                "kind": self.kind(),
                "step_id": step_id,
                "to": to,
            }),
            Self::ApprovalRequested {
                step_id,
                request_id,
            } => json!({
                "kind": self.kind(),
                "step_id": step_id,
                "request_id": request_id,
            }),
            Self::ApprovalResolved { step_id, decision } => json!({
                "kind": self.kind(),
                "step_id": step_id,
                "decision": decision,
            }),
            Self::FallbackHandoff {
                step_id,
                class,
                replayed_steps,
            } => json!({
                "kind": self.kind(),
                "step_id": step_id,
                "class": class,
                "replayed_steps": replayed_steps,
            }),
            Self::Completed { steps, duration_ms } => json!({
                "kind": self.kind(),
                "steps": steps,
                "duration_ms": duration_ms,
            }),
            Self::Recompiled { recipe_id, version } => json!({
                "kind": self.kind(),
                "recipe_id": recipe_id,
                "version": version,
            }),
        }
    }
}

pub trait RunObserver: Send + Sync {
    fn observe(&self, event: RecipeEvent<'_>);
}

/// Non-blocking bridge to the task's canonical event journal.
///
/// The runtime sink provides bounded ordering and persistence. Recipe replay
/// never waits on disk I/O merely to publish operator-facing telemetry.
pub struct EventJournalObserver {
    sink: Arc<dyn RuntimeCanonicalEventSink>,
    scope: CanonicalEventScope,
}

impl EventJournalObserver {
    pub fn new(sink: Arc<dyn RuntimeCanonicalEventSink>, scope: CanonicalEventScope) -> Self {
        Self { sink, scope }
    }
}

impl RunObserver for EventJournalObserver {
    fn observe(&self, event: RecipeEvent<'_>) {
        let kind = event.kind();
        let payload = event.payload();
        tracing::info!(
            target: "magician::api_mining::recipe",
            kind,
            task_id = %self.scope.task_id,
            execution_id = %self.scope.execution_id,
            "Task Recipe lifecycle transition"
        );
        self.sink.emit(
            self.scope.clone(),
            ArtifactV2EventType::RecipeReplay,
            payload,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_recipe_event_uses_its_registered_typed_identifier() {
        use RuntimeAgentEventType as EventType;

        let inputs = HashMap::from([("query".into(), "private-input".into())]);
        let events = [
            (
                RecipeEvent::Started {
                    recipe_id: "r",
                    version: 1,
                    inputs: &inputs,
                },
                EventType::RecipeReplayStarted,
            ),
            (
                RecipeEvent::StepCompleted {
                    step_id: "s",
                    status: 200,
                    duration_ms: 1,
                    transport: Transport::Reqwest,
                },
                EventType::RecipeReplayStepCompleted,
            ),
            (
                RecipeEvent::StepFailed {
                    step_id: "s",
                    class: FailureClass::Network,
                    detail: "private-error",
                },
                EventType::RecipeReplayStepFailed,
            ),
            (
                RecipeEvent::AuthHealed {
                    origin: "https://example.com",
                },
                EventType::RecipeReplayAuthHealed,
            ),
            (
                RecipeEvent::TransportDowngraded {
                    step_id: "s",
                    to: Transport::InPageFetch,
                },
                EventType::RecipeReplayTransportDowngraded,
            ),
            (
                RecipeEvent::ApprovalRequested {
                    step_id: "s",
                    request_id: "request",
                },
                EventType::RecipeReplayApprovalRequested,
            ),
            (
                RecipeEvent::ApprovalResolved {
                    step_id: "s",
                    decision: "deny",
                },
                EventType::RecipeReplayApprovalResolved,
            ),
            (
                RecipeEvent::FallbackHandoff {
                    step_id: "s",
                    class: FailureClass::Network,
                    replayed_steps: 1,
                },
                EventType::RecipeReplayFallbackHandoff,
            ),
            (
                RecipeEvent::Completed {
                    steps: 1,
                    duration_ms: 1,
                },
                EventType::RecipeReplayCompleted,
            ),
            (
                RecipeEvent::Recompiled {
                    recipe_id: "r",
                    version: 2,
                },
                EventType::RecipeReplayRecompiled,
            ),
        ];
        for (event, expected) in events {
            assert!(EventType::ALL.contains(&expected));
            assert_eq!(event.kind(), expected.as_str());
            assert!(magician_event_taxonomy::lookup_agent_event_taxonomy(event.kind()).is_some());
            let payload = event.payload();
            assert_eq!(payload["kind"].as_str(), Some(expected.as_str()));
            let rendered = payload.to_string();
            assert!(!rendered.contains("private-input"));
            assert!(!rendered.contains("private-error"));
        }
    }
}
