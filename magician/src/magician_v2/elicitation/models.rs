use serde::{Deserialize, Serialize};
use serde_json::Value;

// Re-export UnresolvedInput from strategy::plan as the unified representation
// for unresolved inputs/slots throughout the lifecycle
pub use crate::magician_v2::strategy::plan::UnresolvedInput;

/// Input used to update an unresolved input's value or status.
#[derive(Debug, Clone)]
pub struct UnresolvedInputUpdate {
    pub plan_slot_id: String,
    pub answer: Option<Value>,
    pub status: Option<crate::magician_v2::storage::V2SlotStatus>,
    pub source: UpdateSource,
    pub confidence: Option<f32>,
}

/// Possible sources for who/what created or updated an unresolved input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateSource {
    Outline,
    Planner,
    Executor,
    User,
    System,
}

impl UpdateSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            UpdateSource::Outline => "outline",
            UpdateSource::Planner => "planner",
            UpdateSource::Executor => "executor",
            UpdateSource::User => "user",
            UpdateSource::System => "system",
        }
    }
}

/// Observation checkpoints reported after executing a step.
#[derive(Debug, Clone)]
pub struct ObservationEvent {
    pub step_id: String,
    pub status: ObservationStatus,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationStatus {
    Satisfied,
    RetryRecommended,
    Escalate,
}
