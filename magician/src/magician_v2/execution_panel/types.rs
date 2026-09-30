use serde::{Deserialize, Serialize};

use crate::magician_v2::{
    artifact_v2::models::OutputRef,
    execution::AgenticExecutionSummaryRecord,
    feed::FeedItem,
    hitl::HitlOpenTarget,
    progress_channel_seam::ProgressSeverity,
    storage::{
        TaskExecutionLinkedInputRecord, TaskExecutionStepRecord, TaskPriority, TaskStatus, TaskTag,
        WaitingState,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPanelTab {
    Plan,
    Run,
    Output,
    Debug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelOverview {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    pub principal: String,
    pub workspace: String,
    pub ui_thread_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<TaskPriority>,
    pub assigned_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_agent_id: Option<String>,
    pub has_plan: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step: Option<u32>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecutionPanelRunState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responsibility: Option<ExecutionPanelResponsibilityState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_questions: Vec<ExecutionPanelClarificationQuestion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs_attention: Vec<ExecutionPanelAttentionItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_activity: Vec<FeedItem>,
    /// Full seq-ordered humanized event log for the selected execution
    /// (chronological, oldest→newest). Powers the tab-less deep-work
    /// feed. `recent_activity` remains the capped (6) newest-first
    /// summary subset for the legacy Run tab. Empty for tasks with no
    /// events.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activity_log: Vec<FeedItem>,
    /// One entry per delegated child contributing to `activity_log`, so the
    /// panel can render each delegation as its own collapsible group with a
    /// rollup instead of an unattributed interleave. Ordered oldest-started
    /// first; empty when this run delegated nothing.
    ///
    /// Group an entry by its `metadata.execution_id`, which every `FeedItem`
    /// carries — not by `agent_id`, since one agent may be delegated to more
    /// than once and each of those is its own run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegations: Vec<ExecutionPanelDelegationGroup>,
}

/// One delegated child execution whose events appear in the parent's
/// `activity_log`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelDelegationGroup {
    pub execution_id: String,
    pub agent_id: String,
    /// The child's own terminal/live status, not the parent's.
    pub status: String,
    /// How many `activity_log` entries belong to this child — the "13 steps"
    /// in a collapsed header.
    pub entry_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_execution_id: Option<String>,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelOutputResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelRecentRun {
    pub execution_id: String,
    pub started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completion_artifact_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Bounded, UI-safe metadata for one artifact persisted by the selected
/// execution. The artifact payload itself can contain arbitrarily large tool
/// results and stays in durable storage; the panel only needs enough identity
/// and location information to explain and, when file-backed, open it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelArtifactRef {
    pub artifact_id: String,
    pub artifact_type: String,
    pub content_type: String,
    pub produced_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Path relative to the task root. `None` means this is a structured
    /// artifact rather than a file and therefore has no file affordances.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecutionPanelOutputState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ExecutionPanelOutputResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliveries: Vec<FeedItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_runs: Vec<ExecutionPanelRecentRun>,
    /// Outputs written directly by the selected execution. These are kept
    /// separate from task-level outputs so a historical run never inherits a
    /// later run's promoted deliverables in the UI.
    #[serde(default)]
    pub selected_execution_outputs: Vec<OutputRef>,
    /// Outputs collected from work delegated by the selected execution.
    #[serde(default)]
    pub selected_child_outputs: Vec<OutputRef>,
    /// `None` means the artifact index could not be read (or this payload
    /// predates the field); `Some([])` is a successfully-read empty index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_execution_artifacts: Option<Vec<ExecutionPanelArtifactRef>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelExecutionContext {
    pub execution_id: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_chain_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_inputs: Vec<TaskExecutionLinkedInputRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub step_statuses: Vec<TaskExecutionStepRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelShellLine {
    pub text: String,
    pub stream: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelShellEntry {
    pub step_id: String,
    pub step_index: u32,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<ExecutionPanelShellLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub is_complete: bool,
    pub started_at: i64,
    pub execution_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelTimelineEntry {
    pub id: String,
    pub timestamp: i64,
    pub severity: ProgressSeverity,
    pub title: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelObservation {
    pub observation_id: String,
    pub captured_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub has_screenshot: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelClarificationOption {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelClarificationSubmission {
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_state_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelClarificationQuestion {
    pub id: String,
    pub question_text: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_snippets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_slots: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_slot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ExecutionPanelClarificationOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission: Option<ExecutionPanelClarificationSubmission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hitl_request: Option<HitlOpenTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelAttentionItem {
    #[serde(flatten)]
    pub item: FeedItem,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hitl_request: Option<HitlOpenTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelResponsibilityChild {
    pub execution_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub waiting_state: WaitingState,
    pub active_owner_agent_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegation_chain: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_summary: Option<AgenticExecutionSummaryRecord>,
    pub is_blocking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelResponsibilityState {
    pub execution_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_execution_id: Option<String>,
    pub waiting_state: WaitingState,
    pub active_owner_agent_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_stack: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_chain: Vec<String>,
    pub handover_active: bool,
    pub waiting_on_children: bool,
    pub active_child_count: usize,
    pub historical_child_count: usize,
    pub responsibility_summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_from_state: Option<WaitingState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_summary: Option<AgenticExecutionSummaryRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_children: Vec<ExecutionPanelResponsibilityChild>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelTaskplanDocument {
    pub execution_id: String,
    pub markdown: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelDebugState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_execution: Option<ExecutionPanelExecutionContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taskplan: Option<ExecutionPanelTaskplanDocument>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timeline: Vec<ExecutionPanelTimelineEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observations: Vec<ExecutionPanelObservation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shell_entries: Vec<ExecutionPanelShellEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_error_message: Option<String>,
    pub history_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<TaskTag>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPanelState {
    pub default_tab: ExecutionPanelTab,
    pub overview: ExecutionPanelOverview,
    pub run: ExecutionPanelRunState,
    pub output: ExecutionPanelOutputState,
    pub debug: ExecutionPanelDebugState,
}
