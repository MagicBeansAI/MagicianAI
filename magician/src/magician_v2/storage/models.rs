//! V2 Storage Models - Clean separation from V1
//! Built with query analysis metadata from the ground up

use std::collections::BTreeMap;

pub use runtime_core::{PaginatedResult, PaginationInfo, PaginationParams};
use serde::{Deserialize, Serialize};

use crate::magician_v2::{
    ask_loop::session::ClarificationSession,
    orchestrator::v2_orchestrator::{ProcessingMetadata, RecommendedQuestion},
    state_tracker::{StageContext, StateBundle},
    strategy::{ExplorationResult, ResourceUsage, StrategyFailure, StrategyType},
    // Generic record, generic carrier. The execution record deliberately does
    // NOT name `engagements::EngagementAuthorityRef` — an OPC type on the one
    // record every flow writes left recruiting, support triage and vendor
    // management with no slot but an engagement id they would have had to
    // fabricate.
    work_context::WorkAuthorityRef,
    AnalysisMetadata,
    UnifiedQueryAnalysis,
};

/// Single strategy execution attempt
/// Stores the complete exploration result from one strategy attempt,
/// along with metadata about why it succeeded or failed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyAttempt {
    /// Which strategy was used for this attempt
    pub strategy_type: StrategyType,

    /// Complete exploration result from this attempt
    /// Includes all nodes explored, best path found, confidence scores, etc.
    pub exploration_result: ExplorationResult,

    /// Attempt number (1-based: 1 = first attempt, 2 = first retry, etc.)
    pub attempt_number: u32,

    /// Why this attempt failed (None if it succeeded)
    pub failure_reason: Option<StrategyFailure>,

    /// Resources consumed during this attempt
    pub resources_used: ResourceUsage,

    /// Timestamp when this attempt was made (milliseconds since Unix epoch)
    pub attempted_at: i64,
}

/// Single waiting/scheduling state for execution runtime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum WaitingState {
    /// Initial planning/analysis phase
    #[default]
    Planning,

    /// Planning completed, ready to begin execution
    PlanningComplete,

    /// Ready to execute and claimable by the scheduler
    Runnable,

    /// Currently executing on a scheduler-owned claim
    Executing,

    /// Deferred until an execution-bound durable timer fires. This is not a
    /// user pause: only the exact retry owner may reactivate it.
    Sleeping,

    /// Waiting for delegated child executions to finish
    WaitingChildren,

    /// Waiting for execution-scoped AskUser input
    WaitingUser,

    /// Task completed successfully
    Completed,

    /// Task failed or aborted
    Failed,

    /// Task cancelled by user
    Cancelled,

    /// Task paused by user
    Paused,
}

impl WaitingState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planning => "Planning",
            Self::PlanningComplete => "PlanningComplete",
            Self::Runnable => "Runnable",
            Self::Executing => "Executing",
            Self::Sleeping => "Sleeping",
            Self::WaitingChildren => "WaitingChildren",
            Self::WaitingUser => "WaitingUser",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
            Self::Paused => "Paused",
        }
    }

    /// Check if this is a terminal state
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Check if user input is expected
    pub fn expects_user_input(&self) -> bool {
        matches!(self, Self::WaitingUser)
    }

    /// Check if execution is in progress
    pub fn is_executing(&self) -> bool {
        matches!(self, Self::Executing)
    }

    /// Check if execution is scheduler-runnable.
    pub fn is_runnable(&self) -> bool {
        matches!(self, Self::Runnable)
    }
}

/// Entry mode for a persisted execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEntryMode {
    #[default]
    PlanningBacked,
    Direct,
}

impl ExecutionEntryMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PlanningBacked => "planning_backed",
            Self::Direct => "direct",
        }
    }

    pub fn from_token(token: &str) -> Self {
        match token.trim() {
            "direct" => Self::Direct,
            _ => Self::PlanningBacked,
        }
    }

    pub fn is_direct(&self) -> bool {
        matches!(self, Self::Direct)
    }
}

/// Persisted execution run state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRun {
    pub id: String,
    pub principal: String,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_execution_id: Option<String>,
    pub title: Option<String>,
    pub waiting_state: WaitingState,
    pub created_at: i64,
    pub updated_at: i64,
    /// Correlation ID for currently active processing (if any)
    /// Used to track and recover from in-progress operations across page
    /// refreshes
    pub processing_correlation_id: Option<String>,
    /// Current LLM processing stage (query_analysis, slot_extraction, plan_generation, etc.)
    /// Updated in real-time during LLM operations for UI visibility on refresh
    #[serde(default)]
    pub current_stage: Option<String>,
    /// Current LLM provider being used (openai, anthropic, ollama, etc.)
    /// Updated in real-time during LLM operations for UI visibility on refresh
    #[serde(default)]
    pub current_provider: Option<String>,
    /// Escalation trigger if execution is paused due to agent failure escalation.
    /// Set when the pause was triggered by CannotProceed or LoopDetected.
    /// None for normal pauses (max iterations, user input requests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_trigger: Option<String>,
    /// Parent execution ID — set on delegated child executions to link back to the parent.
    #[serde(
        default,
        rename = "parent_execution_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_execution_id: Option<String>,
    /// Child execution IDs — populated on parent executions when delegations are dispatched.
    #[serde(
        default,
        rename = "child_execution_ids",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub child_execution_ids: Vec<String>,
    /// Current active owner for this execution. Non-empty from first persistence.
    pub active_owner_agent_id: String,
    /// Suspended owner chain for same-execution handover.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_stack: Vec<String>,
    /// Active child execution group blocking this parent, if any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_delegation_group: Vec<String>,
    /// Timeout for child executions created by delegation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Inherited owner chain for delegated child executions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegation_chain: Vec<String>,
    /// Authority of the **work** this execution belongs to (§4.2c) — an
    /// engagement, a program, or whichever kind of work is added next.
    ///
    /// Inherited by delegated children verbatim, like `delegation_chain`, and
    /// never caller-settable: a root reads it from the roster that owns the
    /// work, a child copies its parent's.
    ///
    /// Absent is **no authority**, never "unrestricted". `#[serde(default)]`
    /// makes a record without the field read as an unbound run, and every
    /// consumer treats that as unconfined-but-unprivileged rather than
    /// unchecked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_authority: Option<WorkAuthorityRef>,
    /// Exact pre-pause state for precise resume semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_from_state: Option<WaitingState>,
    /// Workflow entry mode used to enforce planning/execution isolation.
    #[serde(default)]
    pub entry_mode: ExecutionEntryMode,
}

/// Storage-layer execution creation contract for V2 execution.
#[derive(Debug, Clone)]
pub struct CreateExecutionParams {
    pub principal: String,
    pub workspace: String,
    pub task_id: Option<String>,
    pub root_execution_id: Option<String>,
    pub title: Option<String>,
    pub active_owner_agent_id: String,
    pub execution_id: Option<String>,
    pub parent_execution_id: Option<String>,
    pub timeout_secs: Option<u64>,
    pub delegation_chain: Vec<String>,
    /// Work authority to persist on the created run. Only the two server-side
    /// write paths set it — a root resolving the work it was asked for, and a
    /// child inheriting its parent's — and every other road leaves it `None`.
    pub work_authority: Option<WorkAuthorityRef>,
    pub waiting_state: WaitingState,
    pub entry_mode: ExecutionEntryMode,
}

impl CreateExecutionParams {
    pub fn root(
        principal: impl Into<String>,
        workspace: impl Into<String>,
        title: Option<String>,
        active_owner_agent_id: impl Into<String>,
    ) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
            task_id: None,
            root_execution_id: None,
            title,
            active_owner_agent_id: active_owner_agent_id.into(),
            execution_id: None,
            parent_execution_id: None,
            timeout_secs: None,
            delegation_chain: Vec::new(),
            work_authority: None,
            waiting_state: WaitingState::Planning,
            entry_mode: ExecutionEntryMode::PlanningBacked,
        }
    }

    pub fn with_execution_id(mut self, execution_id: impl Into<String>) -> Self {
        self.execution_id = Some(execution_id.into());
        self
    }

    pub fn with_parent_execution_id(mut self, parent_execution_id: Option<String>) -> Self {
        self.parent_execution_id = parent_execution_id;
        self
    }

    pub fn with_execution_scope(
        mut self,
        task_id: Option<String>,
        root_execution_id: Option<String>,
    ) -> Self {
        self.task_id = task_id;
        self.root_execution_id = root_execution_id;
        self
    }

    pub fn with_timeout_secs(mut self, timeout_secs: Option<u64>) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    pub fn with_delegation_chain(mut self, delegation_chain: Vec<String>) -> Self {
        self.delegation_chain = delegation_chain;
        self
    }

    pub fn with_entry_mode(mut self, entry_mode: ExecutionEntryMode) -> Self {
        self.entry_mode = entry_mode;
        self
    }
}

/// Persisted execution index entry for a single known execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionIndexEntry {
    pub waiting_state: WaitingState,
    pub active_owner_agent_id: String,
}

/// Persisted index of known executions inside the active execution generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionIndex {
    pub schema_version: u32,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entries: BTreeMap<String, ExecutionIndexEntry>,
}

impl Default for ExecutionIndex {
    fn default() -> Self {
        Self {
            schema_version: 1,
            updated_at: chrono::Utc::now().timestamp_millis(),
            entries: BTreeMap::new(),
        }
    }
}

impl ExecutionIndex {
    pub fn insert(
        &mut self,
        execution_id: &str,
        waiting_state: WaitingState,
        active_owner_agent_id: impl Into<String>,
    ) {
        self.entries.insert(
            execution_id.to_string(),
            ExecutionIndexEntry {
                waiting_state,
                active_owner_agent_id: active_owner_agent_id.into(),
            },
        );
        self.updated_at = chrono::Utc::now().timestamp_millis();
    }

    pub fn remove(&mut self, execution_id: &str) {
        self.entries.remove(execution_id);
        self.updated_at = chrono::Utc::now().timestamp_millis();
    }
}

/// V2 Turn with built-in analysis metadata and strategy execution results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V2Turn {
    pub id: String,
    pub execution_id: String,
    pub direction: TurnDirection,
    pub text: String,
    pub in_reply_to_slot_id: Option<String>,
    pub created_at: i64,

    // V2-specific: Query analysis metadata
    pub query_analysis: Option<UnifiedQueryAnalysis>,
    pub analysis_metadata: Option<AnalysisMetadata>,

    // V2-specific: Strategy execution results (complete history of all attempts)
    /// All strategy attempts for this turn (ordered chronologically)
    /// Includes both failed attempts (with escalation) and final successful
    /// attempt Enables frontend to display full exploration history
    /// including escalation chains
    pub strategy_attempts: Vec<StrategyAttempt>,

    /// Processing metadata including timing, retries, resource consumption
    pub processing_metadata: Option<ProcessingMetadata>,

    // V2-specific: Clarification and question management
    /// Recommended questions from elicitation that should be asked
    /// Persisted to enable restoration after page refresh or server restart
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_questions: Option<Vec<RecommendedQuestion>>,

    // Phase 5: pending_clarification field removed - now tracked in ClarificationSession via session_manager
    // Questions are managed through AskLoopApi and persisted via ClarificationSessionManager
    /// Enriched query generated after collecting all answers from a batch
    /// Contains the consolidated, rewritten query that incorporates user responses
    /// Used by orchestrator during resume to bypass stale elicitation results
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enriched_query: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TurnDirection {
    Inbound,  // User message
    Outbound, // Assistant response
}

/// V2 Slot Status - tracks parameter elicitation state
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum V2SlotStatus {
    /// Waiting for user response
    Pending,

    /// User provided answer
    Answered,

    /// User chose to skip this parameter
    Skipped,
}

/// V2 Slot - represents a parameter that needs to be collected from the user
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V2Slot {
    /// Unique slot identifier
    pub id: String,

    /// Parent execution ID
    pub execution_id: String,

    /// Parameter name (e.g., "hostname", "port", "timeout")
    pub name: String,

    /// JSON schema for validation
    pub schema_json: serde_json::Value,

    /// Whether this parameter is required
    pub required: bool,

    /// Current status
    pub status: V2SlotStatus,

    /// Which turn asked for this parameter
    pub asked_turn_id: Option<String>,

    /// User's answer (if provided)
    pub answer: Option<serde_json::Value>,

    /// When this slot was created
    pub created_at: i64,

    /// When this slot was last updated
    pub updated_at: i64,
}

/// Complete V2 conversation document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRunDocument {
    pub execution: ExecutionRun,
    /// Monotonic generation changed only by waiting-state transitions. This is
    /// separate from `execution.updated_at`, which also changes for unrelated
    /// metadata and therefore cannot safely detect status ABA.
    #[serde(default)]
    pub status_revision: u64,
    pub turns: Vec<V2Turn>,
    pub slots: Vec<V2Slot>,
    #[serde(default)]
    pub states: Vec<StateBundle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clarification_session: Option<ClarificationSession>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clarification_history: Vec<ClarificationHistoryEntry>,
}

/// Execution summary for listings (minimal info)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSummary {
    pub id: String,
    pub principal: String,
    pub workspace: String,
    pub title: Option<String>,
    pub waiting_state: WaitingState,
    pub created_at: i64,
    pub updated_at: i64,
    pub processing_correlation_id: Option<String>,
    #[serde(default)]
    pub current_stage: Option<String>,
    #[serde(default)]
    pub current_provider: Option<String>,
    pub active_owner_agent_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_stack: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_delegation_group: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_from_state: Option<WaitingState>,
    pub turn_count: usize,
    pub pending_slots_count: usize,
    /// Escalation trigger if execution is paused due to agent failure escalation.
    /// Enriched at the API layer from the FullPauseStore, not persisted in the summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_trigger: Option<String>,
}

/// Audit entry capturing a clarification question that was completed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationHistoryEntry {
    pub question_id: String,
    pub question_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_slot_id: Option<String>,
    pub stage: StageContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slot_updates: Vec<ClarificationHistorySlotUpdate>,
}

/// Slot update associated with a clarification history entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationHistorySlotUpdate {
    pub slot_id: String,
    pub slot_type: crate::magician_v2::slot_graph::SlotType,
    pub value: serde_json::Value,
    pub confidence: f64,
    pub received_at: i64,
}

impl From<&ExecutionRunDocument> for ExecutionSummary {
    fn from(doc: &ExecutionRunDocument) -> Self {
        let pending_slots_count = doc
            .slots
            .iter()
            .filter(|s| s.status == V2SlotStatus::Pending)
            .count();

        Self {
            id: doc.execution.id.clone(),
            principal: doc.execution.principal.clone(),
            workspace: doc.execution.workspace.clone(),
            title: doc.execution.title.clone(),
            waiting_state: doc.execution.waiting_state.clone(),
            created_at: doc.execution.created_at,
            updated_at: doc.execution.updated_at,
            processing_correlation_id: doc.execution.processing_correlation_id.clone(),
            current_stage: doc.execution.current_stage.clone(),
            current_provider: doc.execution.current_provider.clone(),
            active_owner_agent_id: doc.execution.active_owner_agent_id.clone(),
            owner_stack: doc.execution.owner_stack.clone(),
            active_delegation_group: doc.execution.active_delegation_group.clone(),
            paused_from_state: doc.execution.paused_from_state.clone(),
            turn_count: doc.turns.len(),
            pending_slots_count,
            escalation_trigger: None,
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod waiting_state_tests {
    use super::WaitingState;

    #[test]
    fn sleeping_roundtrips_as_a_distinct_nonterminal_non_user_state() {
        let encoded = serde_json::to_string(&WaitingState::Sleeping).expect("serialize");
        let decoded: WaitingState = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, WaitingState::Sleeping);
        assert!(!decoded.is_terminal());
        assert!(!decoded.expects_user_input());
        assert!(!decoded.is_executing());
        assert!(!decoded.is_runnable());
    }
}
