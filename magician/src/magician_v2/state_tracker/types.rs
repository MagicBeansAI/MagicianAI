use std::{collections::HashMap, fmt};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::magician_v2::{
    confidence::ConfidenceSummary, slot_graph::SlotRecord, strategy::plan::PlanGraph,
};

pub const STAGE_METADATA_KEY: &str = "stage_context";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum StageContext {
    PlanningBootstrap,
    PlanningIteration,
    ExecutionCycle,
    FollowUp,
    #[default]
    Unknown,
}

impl StageContext {
    pub fn as_str(&self) -> &'static str {
        match self {
            StageContext::PlanningBootstrap => "planning_bootstrap",
            StageContext::PlanningIteration => "planning_iteration",
            StageContext::ExecutionCycle => "execution_cycle",
            StageContext::FollowUp => "follow_up",
            StageContext::Unknown => "unknown",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "planning_bootstrap" => StageContext::PlanningBootstrap,
            "planning_iteration" => StageContext::PlanningIteration,
            "execution_cycle" => StageContext::ExecutionCycle,
            "follow_up" => StageContext::FollowUp,
            _ => StageContext::Unknown,
        }
    }
}

impl fmt::Display for StageContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateBundle {
    pub state_id: String,
    pub workflow_id: String,
    pub current_state: WorkflowState,
    pub llm_reasoning: Option<String>,
    #[serde(default)]
    pub observations: Vec<ObservationAsset>,
    #[serde(default)]
    pub slot_deltas: Vec<SlotDelta>,
    // NOTE: atomic_plan was removed from StateBundle to eliminate redundant storage.
    // The plan is now stored ONLY in turn storage (strategy_attempts[last].exploration_result.plan).
    // Use AskLoopApi::get_plan_for_execution() to fetch the plan when needed.
    pub confidence: ConfidenceScore,
    pub budget: BudgetState,
    #[serde(default)]
    pub stage_context: StageContext,
    #[serde(default)]
    pub completed_stages: Vec<StageCheckpoint>,
    #[serde(default)]
    pub failed_stage: Option<FailedStageInfo>,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageCheckpoint {
    pub stage_name: String,
    #[serde(default)]
    pub stage_context: StageContext,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub completed_at: DateTime<Utc>,
    #[serde(default)]
    pub outputs: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedStageInfo {
    pub stage_name: String,
    #[serde(default)]
    pub stage_context: StageContext,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub failed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    Observe,
    Hypothesize,
    Act,
    Verify,
    Clarify,
    Pause,
    Complete,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationAsset {
    pub asset_id: String,
    pub asset_type: AssetType,
    pub content: AssetContent,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetType {
    Screenshot,
    DomSummary,
    Transcript,
    ToolOutput,
    UserMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum AssetContent {
    Image(Vec<u8>),
    Text(String),
    Json(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotDelta {
    pub slot_id: String,
    pub operation: DeltaOperation,
    pub previous_value: Option<serde_json::Value>,
    pub new_value: serde_json::Value,
    #[serde(default)]
    pub stage: StageContext,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaOperation {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfidenceScore {
    pub overall: f64,
    #[serde(default)]
    pub per_slot: HashMap<String, f64>,
    pub slope: Option<f64>,
    #[serde(default)]
    pub history: Vec<(i64, f64)>,
    #[serde(default)]
    pub slot_records: HashMap<String, SlotRecord>,
    #[serde(default)]
    pub summary: Option<ConfidenceSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BudgetState {
    pub remaining: f64,
    pub initial: f64,
    #[serde(default)]
    pub spent: Vec<BudgetSpend>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetSpend {
    pub amount: f64,
    pub reason: String,
    #[serde(default)]
    pub stage: StageContext,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub timestamp: DateTime<Utc>,
}

impl Default for BudgetSpend {
    fn default() -> Self {
        Self {
            amount: 0.0,
            reason: String::new(),
            stage: StageContext::default(),
            timestamp: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomicPlanSnapshot {
    pub plan: PlanGraph,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub generated_at: DateTime<Utc>,
}

impl AtomicPlanSnapshot {
    pub fn new(plan: PlanGraph) -> Self {
        Self {
            plan,
            generated_at: Utc::now(),
        }
    }
}
