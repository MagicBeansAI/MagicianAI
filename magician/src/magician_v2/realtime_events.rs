//! Real-time Event Broadcasting for V2 Message Processing
//!
//! Provides WebSocket event streaming for live updates during V2 strategy
//! execution, including progress tracking, cancellation support, and state
//! synchronization.

mod service_health;
pub use service_health::ServiceFailure;

use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{Read, Seek, Write},
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex, RwLock,
    },
};

use dashmap::DashMap;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;
use tracing::{debug, error, warn};
use uuid::Uuid;

use crate::magician_v2::feed::{FeedItem, FeedItemPatch};
use crate::magician_v2::json_traversal::{
    clone_json_iteratively, discard_json_iteratively, inspect_json_bounded,
    json_bytes_nesting_is_bounded, json_bytes_nodes_are_bounded, write_json,
    MAX_RETAINED_JSON_DEPTH,
};
use crate::magician_v2::{
    agents::storage::{AgentStorage, FileLockGuard},
    artifact_v2::{
        events::attach_projected_source_identity,
        map_v2_realtime_event,
        workspace::{
            ensure_private_workspace_file_path_identity,
            validate_private_workspace_file_path_identity, ArtifactV2Workspace,
            WORKSPACE_DESTINATION_CAS_CONFLICT_DETAIL,
        },
        ArtifactV2Error, CanonicalEventScope, RuntimeCanonicalEventReceipt,
        RuntimeCanonicalEventSink,
    },
    chat::presentation::{StructuredResponseDropReason, StructuredResponseV1},
    database_owners::{host_database_path, DatabaseOwner},
    execution_panel::ExecutionPanelState,
};

use super::{
    ask_loop::metrics::ClarificationMetricsSnapshot,
    state_tracker::StageContext,
    storage::V2ConversationStore,
    strategy::{ExplorationResult, StrategyType},
};

/// Summary for the active clarification batch
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationBatchSnapshot {
    pub batch_id: String,
    pub total: usize,
    pub answered: usize,
}

/// Slot-level confidence changes included with clarification telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceSlotDelta {
    pub slot_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<f64>,
    pub updated: f64,
}

/// Default outcome for DomChangeDetected event.
fn default_outcome() -> String {
    "success".to_string()
}

/// `skip_serializing_if` helper — omits the field from the wire format
/// when it carries the default `0` value. Keeps the payload tight for
/// the common (non-refinement) case.
fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// Stable local correlation attached to high-level LLM response facts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmEventCorrelation {
    pub schema_version: u16,
    pub trace_id: String,
    pub llm_call_id: String,
    /// The runtime activity span this call was issued under, when it was
    /// issued under one.
    ///
    /// **This is the join key, and it has to travel here.** The canonical
    /// lifecycle recorder rebuilds an `LlmTraceContext` from this correlation
    /// and nothing else (`llm_trace_activation::context_from_correlation`), so
    /// a field missing here is a field the durable `LlmCallCompleted` for every
    /// successful call cannot have — and `LlmCallCompleted` is where price and
    /// tokens live. Without it the retrospective parquet join and the live
    /// `ActivityCost` event both have a cost and no owner.
    ///
    /// `None` is ordinary: a call issued outside any instrumented span has no
    /// activity to belong to. Inventing one would attach spend to unrelated
    /// work, which is worse than reporting none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_relation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_decision_id: Option<String>,
    pub scope_resolution: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_id: Option<String>,
    /// Transport truth for provider conversation projection. This is separate
    /// from the stable logical iteration identity used by cross-surface joins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_projection_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_availability: Option<magicllm::types::UsageAvailability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_message_id: Option<String>,
    pub workload_class: String,
    pub call_role: String,
    #[serde(default)]
    pub provider_attempt_count: u32,
    #[serde(default)]
    pub response_reused: bool,
}

impl From<&magicllm::LlmTraceReceipt> for LlmEventCorrelation {
    fn from(receipt: &magicllm::LlmTraceReceipt) -> Self {
        let context = &receipt.context;
        Self {
            schema_version: 1,
            trace_id: context.trace_id.clone(),
            llm_call_id: context.llm_call_id.clone(),
            // Copied, never re-derived. `current_activity_id()` read here would
            // name whatever span happens to be current at the *event* boundary,
            // which is not the span that issued the call — the receipt is built
            // where the response lands, often on a worker task. The context is
            // the only witness to where the work was submitted from.
            activity_id: context.activity_id.clone(),
            provider_attempt_id: receipt.provider_attempt_id.clone(),
            dispatch_job_id: receipt.dispatch_job_id.clone(),
            parent_call_id: context.parent_call_id.clone(),
            parent_relation: context
                .parent_relation
                .map(|value| value.as_str().to_string()),
            retry_group_id: context.retry_group_id.clone(),
            route_decision_id: context.route_decision_id.clone(),
            scope_resolution: context.scope_resolution.as_str().to_string(),
            task_id: context.task_id.clone(),
            root_execution_id: context.root_execution_id.clone(),
            execution_id: context.execution_id.clone(),
            plan_id: context.plan_id.clone(),
            step_id: context.step_id.clone(),
            iteration_id: context.iteration_id.clone(),
            prompt_projection_mode: None,
            usage_availability: None,
            chat_session_id: context.chat_session_id.clone(),
            chat_turn_id: context.chat_turn_id.clone(),
            user_message_id: context.user_message_id.clone(),
            workload_class: context.workload_class.as_str().to_string(),
            call_role: context.call_role.as_str().to_string(),
            provider_attempt_count: receipt.provider_attempt_count,
            response_reused: receipt.response_reused,
        }
    }
}

impl LlmEventCorrelation {
    pub fn direct(
        principal: impl Into<String>,
        workspace: impl Into<String>,
        workload_class: magicllm::LlmWorkloadClass,
    ) -> Self {
        let receipt = magicllm::LlmTraceReceipt::direct(magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new(principal, workspace),
            workload_class,
        ));
        Self::from(&receipt)
    }

    /// Identify an externally executed AI run whose aggregate usage is
    /// observable but whose physical provider calls are opaque. The zero
    /// attempt count is intentional: canonical provider-attempt facts must
    /// never invent one call for an unknown number of inner calls.
    pub fn external_aggregate(
        principal: impl Into<String>,
        workspace: impl Into<String>,
        workload_class: magicllm::LlmWorkloadClass,
    ) -> Self {
        let receipt = magicllm::LlmTraceReceipt::direct_with_attempt_count(
            magicllm::LlmTraceContext::new(
                magicllm::LlmScope::new(principal, workspace),
                workload_class,
            ),
            0,
        );
        Self::from(&receipt)
    }

    /// Preserve an exact provider/dispatch receipt while binding legacy or
    /// system-default calls to the authoritative product scope at the event
    /// boundary. A receipt already bound to a different real scope is
    /// rejected so an identifier can never be joined across tenants.
    pub fn scoped(
        receipt: &magicllm::LlmTraceReceipt,
        principal: impl Into<String>,
        workspace: impl Into<String>,
        workload_class: magicllm::LlmWorkloadClass,
    ) -> Option<Self> {
        let principal = principal.into();
        let workspace = workspace.into();
        let mut receipt = receipt.clone();
        let desired = magicllm::LlmScope::new(principal, workspace);
        let context = &mut receipt.context;
        let can_rebind = matches!(
            context.scope_resolution,
            magicllm::LlmScopeResolution::LegacyDefault
                | magicllm::LlmScopeResolution::SystemDefault
        );
        if context.scope != desired && !can_rebind {
            return None;
        }
        context.scope = desired;
        if can_rebind {
            context.scope_resolution = magicllm::LlmScopeResolution::Explicit;
        }
        context.workload_class = workload_class;
        Some(Self::from(&receipt))
    }

    pub fn with_chat_turn(
        mut self,
        chat_session_id: impl Into<String>,
        chat_turn_id: impl Into<String>,
    ) -> Self {
        self.chat_session_id = Some(chat_session_id.into());
        self.chat_turn_id = Some(chat_turn_id.into());
        self
    }

    pub fn with_execution(
        mut self,
        root_execution_id: impl Into<String>,
        execution_id: impl Into<String>,
        plan_id: Option<String>,
        step_id: Option<String>,
        iteration_id: Option<String>,
    ) -> Self {
        self.root_execution_id = Some(root_execution_id.into());
        self.execution_id = Some(execution_id.into());
        self.plan_id = plan_id;
        self.step_id = step_id;
        self.iteration_id = iteration_id;
        self
    }
}

/// Runtime transport events for live WebSocket streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "data")]
pub enum RuntimeTransportEvent {
    /// Versioned, content-free memory classification evidence; call ids join
    /// the existing Decision Model ledger. Missing reference/accounting is explicit.
    DecisionAccountingGap {
        principal: String,
        workspace: String,
        operation: String,
        request_id: String,
        reason: String,
        timestamp: i64,
    },
    DecisionShadowAgreement {
        principal: String,
        workspace: String,
        record: Box<serde_json::Value>,
        timestamp: i64,
    },
    /// Message processing started
    MessageProcessingStarted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        turn_id: String,
        correlation_id: String,
        timestamp: i64,
    },

    /// Query analysis completed
    QueryAnalysisCompleted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        complexity_score: f64,
        intent: String,
        categories: Vec<String>,
        timestamp: i64,
    },

    /// Strategy selected for execution
    StrategySelected {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        strategy: StrategyType,
        confidence: f64,
        reason: String,
        timestamp: i64,
    },

    /// Exploration progress update
    ExplorationProgress {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        nodes_explored: usize,
        current_depth: usize,
        best_score: f64,
        current_task: String,
        progress_percent: f64,
        timestamp: i64,
    },

    // NOTE: ToolMatching event removed - superseded by ToolMatchingTierStarted/Completed
    /// Execution started (plan handoff to executor)
    ExecutionStarted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        steps_total: usize,
        timestamp: i64,
    },

    /// Execution step started
    ExecutionStepStarted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_index: usize,
        step_id: String,
        steps_total: usize,
        timestamp: i64,
    },

    /// Execution step completed
    ExecutionStepCompleted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_index: usize,
        step_id: String,
        success: bool,
        timestamp: i64,
    },

    /// Execution paused (e.g., waiting for JIT clarification)
    ExecutionPaused {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_index: usize,
        step_id: String,
        reason: String,
        timestamp: i64,
    },

    /// Execution resumed (after pause or restart)
    ExecutionResumed {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_index: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        step_id: Option<String>,
        mode: String,
        timestamp: i64,
    },

    /// Execution failed
    ExecutionFailed {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_index: usize,
        step_id: String,
        error: String,
        timestamp: i64,
    },

    /// Execution cancelled by user
    ExecutionCancelled {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_index: usize,
        timestamp: i64,
    },

    /// Inflight request resent on resume
    ExecutionInflightResent {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_index: usize,
        step_id: String,
        request_id: String,
        attempt_count: u32,
        timestamp: i64,
    },

    /// Inflight request dropped (non-retryable)
    ExecutionInflightDropped {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_index: usize,
        step_id: String,
        request_id: String,
        timestamp: i64,
    },

    /// Execution completed
    ExecutionCompleted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        steps_total: usize,
        success: bool,
        timestamp: i64,
    },

    /// Execution restore failed (version mismatch, expired, timeout, etc.)
    ExecutionRestoreFailed {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        timestamp: i64,
    },

    /// Message processing completed
    MessageCompleted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        turn_id: String,
        correlation_id: String,
        response: String,
        exploration_summary: Option<ExplorationSummary>,
        timestamp: i64,
    },

    /// Execution status changed (for UI to update without refresh)
    ExecutionStatusChanged {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        root_execution_id: Option<String>,
        previous_status: String,
        new_status: String,
        /// Optional reason for the status change
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        timestamp: i64,
    },

    /// Responsibility state changed for an execution or one of its active children.
    ExecutionResponsibilityChanged {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_execution_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        root_execution_id: Option<String>,
        waiting_state: String,
        active_owner_agent_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        owner_stack: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        active_delegation_group: Vec<String>,
        timestamp: i64,
    },

    // NOTE: ProcessingCancelled event removed - ExecutionCancelled covers cancellation
    /// Processing error occurred
    ProcessingError {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        error_message: String,
        error_type: String,
        timestamp: i64,
    },

    /// LLM analysis started
    LLMAnalysisStarted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        provider: String, // "openai", "anthropic", "ollama"
        stage: String, // "query_analysis", "slot_extraction", "plan_generation", "elicitation", etc.
        query_length: usize,
        timestamp: i64,
    },

    /// LLM analysis completed successfully
    LLMAnalysisCompleted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        provider: String,
        stage: String,
        response_length: usize,
        duration_ms: u64,
        timestamp: i64,
    },

    /// LLM analysis failed
    LLMAnalysisFailed {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        provider: String,
        stage: String,
        error_type: String, // "api_key_missing", "timeout", "invalid_response", "network_error"
        error_message: String,
        timestamp: i64,
    },
    // `ClarificationQueued` / `ClarificationResponseReceived` removed in
    // H7.2 (2026-05-11). All emit sites were retired in H6.2; canonical
    // `HitlRequested` / `HitlResolved` envelopes with `source:
    // "clarification"` are the wire shape going forward.
    /// Clarification session snapshot (queue metrics & batch progress)
    ClarificationSessionSnapshot {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        state: String,
        total_questions: usize,
        waiting_on_user: usize,
        queued: usize,
        answered: usize,
        cancelled: usize,
        pending_question_ids: Vec<String>,
        active_batch: Option<ClarificationBatchSnapshot>,
        #[serde(skip_serializing_if = "Option::is_none")]
        last_question_asked_at: Option<i64>,
        timestamp: i64,
    },
    /// Confidence telemetry emitted after each clarification answer
    ClarificationConfidenceSnapshot {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        question_id: String,
        trigger: String,
        overall_confidence: f64,
        unresolved_count: usize,
        slot_deltas: Vec<ConfidenceSlotDelta>,
        question_created_at: i64,
        answered_at: i64,
        timestamp: i64,
    },
    /// Slot graph diff emitted when cached slots change due to clarification/resume
    SlotGraphDiff {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        source: String,
        inserted: usize,
        updated: usize,
        removed: usize,
        total_slots: usize,
        timestamp: i64,
    },
    /// Workflow resumed after a pause
    WorkflowResumed {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        question_id: Option<String>,
        resume_mode: String,
        answered_count: usize,
        pending_count: usize,
        timestamp: i64,
    },

    /// Structured telemetry describing which stage resumed and what was reused
    WorkflowStageResumed {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        stage_name: String,
        stage_context: String,
        attempt: u32,
        reused_checkpoint: bool,
        checkpoint_hash: Option<String>,
        reused_stages: Vec<String>,
        timestamp: i64,
    },

    /// Workflow resume failed
    WorkflowResumeFailed {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        question_id: Option<String>,
        error: String,
        timestamp: i64,
    },
    /// Observability alerts for critical backend conditions
    ObservabilityAlert {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        alert_type: String,
        details: Value,
        timestamp: i64,
    },
    /// Aggregate clarification metrics for dashboards
    ClarificationMetricsSnapshot {
        total_sessions_started: u64,
        total_sessions_completed: u64,
        active_sessions: u64,
        avg_session_duration_ms: Option<f64>,
        avg_questions_per_session: Option<f64>,
        guardrail_timeouts: u64,
        guardrail_question_caps: u64,
        guardrail_round_caps: u64,
        timestamp: i64,
    },

    /// Atomic plan generation started (Phase 1: Outline)
    AtomicPlanOutlineStarted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        total_atomic_tools: usize,
        timestamp: i64,
    },

    /// Atomic plan outline completed
    AtomicPlanOutlineCompleted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        goals_count: usize,
        confidence: f64,
        timestamp: i64,
    },

    /// Atomic plan expansion started (Phase 2: Steps)
    AtomicPlanExpansionStarted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        goals_from_outline: usize,
        timestamp: i64,
    },

    /// Atomic plan generated/updated (incremental)
    AtomicPlanGenerated {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        turn_id: String,
        plan_graph: crate::magician_v2::strategy::PlanGraph,
        validation_status: String, // "validating", "valid", "retrying"
        attempt_number: u32,
        timestamp: i64,
    },

    /// Tool matching tier started
    ToolMatchingTierStarted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        tier_number: u8, // 0-4
        tier_name: String, /* "Category Pre-Filter", "Rule-Based", "Semantic", "Candidate
                          * Selection", "LLM Evaluation" */
        description: String,
        timestamp: i64,
    },

    /// Tool matching tier completed
    ToolMatchingTierCompleted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        tier_number: u8,
        tier_name: String,
        candidates_count: usize,
        duration_ms: u64,
        top_candidates: Vec<TierCandidate>,
        timestamp: i64,
    },

    // NOTE: CategoryFuzzyMatching event removed - superseded by ExplorationProgress
    /// Slot extraction started
    SlotExtractionStarted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        message_length: usize,
        timestamp: i64,
    },

    /// Individual slot extracted
    SlotExtracted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        slot_id: String,
        slot_type: String,
        confidence: f64,
        timestamp: i64,
    },

    /// Enrichment pipeline started
    SlotEnrichmentStarted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        total_slots: usize,
        enricher_count: usize,
        timestamp: i64,
    },

    /// Enrichment pipeline completed
    SlotEnrichmentCompleted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        total_slots: usize,
        slots_changed: usize,
        invocations: usize,
        errors_count: usize,
        timestamp: i64,
    },

    /// Slot confidence updated
    SlotConfidenceUpdated {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        slot_id: String,
        old_confidence: f64,
        new_confidence: f64,
        source: String, // "LlmPrimary", "UserReply", "DeterministicCheck", etc.
        timestamp: i64,
    },

    /// Clarified task generation completed
    ClarifiedTaskReady {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        correlation_id: String,
        clarified_task: String,
        objectives_count: usize,
        constraints_count: usize,
        confidence: f64,
        timestamp: i64,
    },

    /// Keep-alive / heartbeat
    Heartbeat { timestamp: i64 },

    /// Carries a `ProgressMessage` across the transport bus. The
    /// progress router subscribes to this variant and feeds the
    /// existing channel-fanout pipeline (subscription matching,
    /// circuit breaker, retry, etc.) — the bus becomes the *only*
    /// rail producers touch; the router is just one of N bus
    /// consumers downstream of it. Replaces the legacy direct
    /// `ProgressRouter::publish_message(ProgressMessage)` calls so
    /// every producer is on the single rail.
    ProgressEvent {
        message: super::progress_channel_seam::types::ProgressMessage,
        timestamp: i64,
    },

    // Pipeline Lifecycle Events (M7)
    /// Planning pipeline started
    PipelineStarted {
        workflow_id: String,
        chain_id: String,
        max_iterations: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// Planning pipeline step started — emitted before each pipeline-system-agent
    /// invocation so subscribers can measure per-step latency without inferring it
    /// from the previous `PipelineStepCompleted`.
    PipelineStepStarted {
        workflow_id: String,
        step_id: String,
        agent_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// Planning pipeline step completed
    PipelineStepCompleted {
        workflow_id: String,
        step_id: String,
        agent_id: String,
        outcome_kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// Planning pipeline completed successfully
    PipelineCompleted {
        workflow_id: String,
        chain_id: String,
        steps_executed: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// Planning pipeline failed
    PipelineFailed {
        workflow_id: String,
        chain_id: String,
        reason: String,
        steps_executed: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    // Progressive Elicitation Events
    /// Parameter inference attempted
    ParameterInferenceAttempted {
        execution_id: String,
        parameter_name: String,
        priority: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// Parameter successfully inferred
    ParameterInferred {
        execution_id: String,
        parameter_name: String,
        inferred_value: serde_json::Value,
        confidence: f64,
        method: String, // "LLMBased", "RuleBased", "Historical", "Default", "AutoFill"
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// Parameter inference failed (low confidence or error)
    ParameterInferenceFailed {
        execution_id: String,
        parameter_name: String,
        confidence: f64,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// Parameter discovery started
    ParameterDiscoveryAttempted {
        execution_id: String,
        parameter_name: String,
        discovery_method: String, // "WebSearch", "FilesystemSearch", "APIQuery", etc.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// Parameter discovered successfully
    ParameterDiscovered {
        execution_id: String,
        parameter_name: String,
        discovered_value: serde_json::Value,
        confidence: f64,
        discovery_method: String,
        external_actions_performed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// Parameter discovery failed
    ParameterDiscoveryFailed {
        execution_id: String,
        parameter_name: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// Overall parameter resolution progress
    ParameterResolutionProgress {
        execution_id: String,
        total_parameters: usize,
        resolved_count: usize,
        inferred_count: usize,
        discovered_count: usize,
        deferred_count: usize,
        remaining_count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// Chat message received (new message in a chat session)
    ChatMessageReceived {
        session_id: String,
        message: super::chat::models::ChatMessage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin_channel: Option<super::chat::models::ChatChannel>,
        /// Required for chronological sort across the unified event
        /// stream. Set at emission time (ms epoch) — the inner
        /// `message.created_at` is `DateTime<Utc>` and not always
        /// reliable as an event-arrival timestamp (e.g., backfilled
        /// messages).
        #[serde(default)]
        timestamp: i64,
    },

    /// A Live Thinking Map changed server-side — any successfully applied
    /// envelope: owner `/operations`, `PATCH`, `/interpret`, a consolidation
    /// decision, `/restore`, or an ambient auto-mapped utterance. This is a
    /// lightweight change NOTICE only (no map payload): clients re-fetch the
    /// authoritative map via `GET /thinking-maps/{id}` on receipt. Pushing the
    /// revision, not the state, keeps the wire small and preserves the single
    /// server-authoritative fetch path (the map document can be large).
    ThinkingMapUpdated {
        map_id: String,
        principal: String,
        workspace: String,
        /// The map revision AFTER the applied envelope.
        revision: u64,
        timestamp: i64,
    },

    /// An owner-triggered map interpretation moved to a new pipeline stage.
    /// Interpretation takes seconds (the LLM call dominates), and without this
    /// every client showed a spinner and one unchanging line for the whole of
    /// it. Best-effort narration only — never load-bearing: a client that
    /// misses one just keeps its current line, and the interpretation's
    /// *result* still arrives as `ThinkingMapUpdated` + the HTTP response.
    ///
    /// `utterance_id` is what lets a client ignore progress for an
    /// interpretation it did not start — the bus is shared, and two maps
    /// thinking at once must not drive each other's strip.
    ThinkingMapInterpretProgress {
        map_id: String,
        principal: String,
        workspace: String,
        /// The interpretation this narrates (minted by the interpret handler).
        utterance_id: String,
        /// Closed vocabulary bound to the interpreter's real steps:
        /// `preparing`, `loading_context`, `facilitating`, `parsing`,
        /// `shaping`, `idle`. Clients must tolerate unknown stages (keep the
        /// current line) so the vocabulary can grow without breaking them.
        stage: String,
        /// Optional server-worded detail. Currently never set — clients own
        /// their stage labels — but carried so a future stage can say
        /// something only the server knows without a wire change.
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// On `preparing`: how many live thoughts the facilitator is reading.
        #[serde(skip_serializing_if = "Option::is_none")]
        node_count: Option<usize>,
        timestamp: i64,
    },
    // ============================================================
    // LLM Observability Events (for agentic execution tracing)
    // ============================================================
    // NOTE: ObservationCaptured, ObservationFailed events have been removed.
    // Agentic execution uses AgenticPageUnderstanding for observation visibility.
    /// LLM request sent
    LLMRequestSent {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: Option<String>,
        step_index: Option<usize>,
        /// What capability is being requested
        capability: String,
        /// Brief description of the request
        request_summary: String,
        /// Input token estimate (if available)
        input_tokens_estimate: Option<usize>,
        /// Budget remaining before this call
        budget_remaining: f64,
        timestamp: i64,
    },

    /// LLM response received
    LLMResponseReceived {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        /// Stable local identity and dispatch/attempt lineage. Optional only
        /// for deserializing historical events; all new production emitters
        /// must populate it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        correlation: Option<LlmEventCorrelation>,
        plan_id: String,
        step_id: Option<String>,
        step_index: Option<usize>,
        /// What capability was requested
        capability: String,
        /// Success or failure
        success: bool,
        /// Brief summary of the decision/result
        decision_summary: String,
        /// Cost of this LLM call in USD (0.0 if pricing table has no entry)
        cost: f64,
        /// Latency in milliseconds
        latency_ms: u64,
        /// Error message if failed
        error: Option<String>,
        /// Provider name, lowercase (e.g. "anthropic", "minimax", "openai")
        #[serde(default)]
        provider: String,
        /// Effective model used by the provider
        #[serde(default)]
        model: String,
        /// Whether the provider returned an authoritative usage object for
        /// this response. Zero-valued token buckets are meaningful only when
        /// this is true; historical events default to false so analytics does
        /// not reinterpret missing usage as a real zero-token call.
        #[serde(default)]
        usage_reported: bool,
        /// Total prompt tokens (includes cache_read + cache_creation on Anthropic-style providers)
        #[serde(default)]
        input_tokens: u32,
        /// Completion / output tokens
        #[serde(default)]
        output_tokens: u32,
        /// Reasoning / thinking tokens when reported by the provider
        #[serde(default)]
        reasoning_tokens: u32,
        /// Provider-emitted reasoning / chain-of-thought summary text, when the
        /// profile requested one. Lets aggregate analytics inspect the model's
        /// stated reasoning, not just its token count. `None` when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_summary: Option<String>,
        /// Cache-read tokens (charged at discount)
        #[serde(default)]
        cache_read_tokens: u32,
        /// Cache-creation / cache-write tokens (charged at premium on Anthropic/Minimax)
        #[serde(default)]
        cache_creation_tokens: u32,
        /// Realtime (voice) only: the AUDIO-modality portion of the token counts —
        /// audio uncached-input / output / cached-input. `input_tokens` etc. carry
        /// the audio+text total, so the text portion is `total - audio`. Lets
        /// analytics separate audio vs text spend and lets the repricer recompute
        /// realtime cost against the (audio+text) realtime rate shape. `None` for
        /// non-realtime calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio_input_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio_output_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio_cached_tokens: Option<u32>,
        /// Provider-executed web searches this call ran (the
        /// `server_web_search` lane). Each bills per call on top of tokens,
        /// so canonical cost recomputation must add the per-search charges
        /// or every searched call flags a false `producer_cost_mismatch`
        /// capture gap and under-counts spend. Zero for ordinary calls.
        #[serde(default)]
        search_calls: u32,
        /// Time-to-first-token in ms — measured from LLM request dispatch to the
        /// first text token observed via the streaming consumer. `None` when the
        /// call was non-streaming, when the response was tool-call-only (no text),
        /// or when the caller didn't instrument first-token timing. Decouples
        /// "thinking time" (TTFT) from "answer length" (latency_ms - ttft_ms).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttft_ms: Option<u64>,
        /// Task id that triggered this LLM call (None for chat-inline / system calls)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        /// Agent id executing this LLM call (current owner, not the original delegator)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        /// Delegated-agent id when execution is running as a delegate (None for primary)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delegated_agent_id: Option<String>,
        /// Chat session id when running inline from chat (None for autonomous)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat_session_id: Option<String>,
        /// Typed LLM operation name (e.g. "agentic_decision", "agentic_decision_som",
        /// "agentic_decision_text", "slot_extraction", "vision_targeting"). Free-form
        /// string to keep this event variant cross-crate-import-free.
        #[serde(default)]
        operation: String,
        /// Selected LLM profile name (e.g. "gptterra-responses-toolsany")
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        /// Attempt number for this decision call. 1-based; > 1 means retry.
        #[serde(default)]
        attempt: u32,
        /// Response shape: "tool_call" | "text" | "streaming" | "reasoning_only" | "error"
        #[serde(default)]
        response_kind: String,
        /// Start of the LLM call (Unix ms). 0 when not measured.
        #[serde(default)]
        started_at_ms: i64,
        timestamp: i64,
    },

    /// Adaptive profile escalated from fast → thinking variant. Emitted
    /// when the LLM called `request_thinking_mode` on an adaptive profile.
    /// UI surfaces this as a "Thinking mode" chip on the chat-input until
    /// the next user message ends the turn. `chat_turn_id` carries the
    /// per-turn correlation id so the UI can scope the chip to one turn.
    ThinkingModeActivated {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat_session_id: Option<String>,
        chat_turn_id: String,
        adaptive_profile: String,
        fast_profile: String,
        thinking_profile: String,
        /// Optional one-liner the LLM passed in as `reason`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        timestamp: i64,
    },

    /// Adaptive profile reverted to fast mode at turn end. Emitted by
    /// the chat-inline runtime when a thinking-escalated turn finishes
    /// so the UI can clear the chip.
    ThinkingModeCompleted {
        execution_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat_session_id: Option<String>,
        chat_turn_id: String,
        adaptive_profile: String,
        timestamp: i64,
    },

    // NOTE: ActionDispatched, ActionResultEvent events have been removed.
    // Agentic execution uses AgenticActionExecuted for action visibility.
    /// Inference attempted for parameter resolution
    InferenceAttempted {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: Option<String>,
        /// Parameter being inferred
        parameter: String,
        /// Inferred value (JSON serialized)
        inferred_value: Option<serde_json::Value>,
        /// Confidence in the inference (0-1)
        confidence: f64,
        /// Reasoning for the inference
        reason: String,
        /// Whether inference was accepted (confidence >= threshold)
        accepted: bool,
        timestamp: i64,
    },

    // NOTE: ValidationResult, ExecutabilityCheck, PageStageDetected events have been removed.
    // Agentic execution uses AgenticDecisionMade for decision visibility and
    // AgenticPageUnderstanding for page stage detection.

    // ============================================================
    // Agentic Execution Events (observe-decide-execute loop)
    // ============================================================
    /// Agentic execution started for a step
    AgenticExecutionStarted {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        /// Goal being pursued
        goal: String,
        /// Success criteria
        success_criteria: String,
        /// Maximum iterations allowed
        max_iterations: usize,
        /// Hint action provided by planner (if any)
        #[serde(skip_serializing_if = "Option::is_none")]
        hint_action: Option<String>,
        /// Agent ID — `Some(id)` for agent-scoped cycles, `None` for orchestrator-driven.
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        timestamp: i64,
    },

    /// Agentic iteration started (observe phase)
    AgenticIterationStarted {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        /// Iteration number (1-indexed)
        iteration: usize,
        /// Environment type being observed
        environment_type: String, // "browser", "filesystem", "http", "shell"
        timestamp: i64,
    },

    /// Agentic iteration completed — emitted at the end of each iteration regardless of
    /// outcome (action executed, decision rejected, fall-through). Pairs with
    /// `AgenticIterationStarted` so subscribers can measure per-iteration duration without
    /// inferring it from the next `AgenticIterationStarted`.
    AgenticIterationCompleted {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        iteration: usize,
        /// Wall-clock duration of this iteration in milliseconds.
        duration_ms: u64,
        /// Outcome category for the iteration: "action_executed" | "decision_rejected"
        /// | "loop_continue" | "terminal" | "paused" | "cancelled".
        outcome: String,
        timestamp: i64,
    },

    /// Agentic loop detected N consecutive iterations without a successful tool call.
    /// Observability-only; no behavior change is triggered by this event.
    AgenticStepStuckWarning {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        iteration: usize,
        /// True when this originates from the pre-loop capability PREFLIGHT blocker
        /// path (a soft admission warning), NOT the genuine in-loop no-progress stuck
        /// detector. Consumers that treat this event as a stuck signal must ignore
        /// preflight ones. `#[serde(default)]` = false for back-compat with events
        /// serialized before this field existed.
        #[serde(default)]
        is_preflight: bool,
        /// Number of consecutive iterations with no `result.success == true`.
        consecutive_count: usize,
        /// Up to the last 3 decision `action_summary` strings, oldest first.
        recent_actions: Vec<String>,
        timestamp: i64,
    },

    /// Multimodal page understanding completed
    AgenticPageUnderstanding {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        /// Observation ID for screenshot fetch
        observation_id: String,
        /// Iteration number
        iteration: usize,
        /// Detected page stage
        page_stage: String,
        /// Number of interactive elements found
        element_count: usize,
        /// Whether page appears loading
        appears_loading: bool,
        /// Current URL
        url: Option<String>,
        /// Confidence in understanding (0-1)
        confidence: f64,
        /// Has screenshot available
        has_screenshot: bool,
        timestamp: i64,
    },

    /// Agentic decision made (decide phase)
    AgenticDecisionMade {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        /// Iteration number
        iteration: usize,
        /// Decision type
        decision_type: String, // "execute", "goal_reached", "cannot_proceed", "need_user_input", etc.
        /// Action to execute (if decision_type == "execute")
        action_summary: Option<String>,
        /// Reasoning for the decision
        reasoning: String,
        /// Confidence in decision (0-1)
        confidence: f64,
        /// LLM thinking/chain-of-thought (always present for LLM decisions)
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking: Option<String>,
        /// Evidence (for goal_reached, cannot_proceed)
        #[serde(skip_serializing_if = "Option::is_none")]
        evidence: Option<String>,
        /// Tool/action name (e.g. "click", "navigate", "type")
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        /// Action type category (e.g. "browser", "file", "http", "bash")
        #[serde(skip_serializing_if = "Option::is_none")]
        action_type: Option<String>,
        /// Target element ID (SoM)
        #[serde(skip_serializing_if = "Option::is_none")]
        element_id: Option<u32>,
        /// Number of candidates in Execute decisions
        #[serde(skip_serializing_if = "Option::is_none")]
        candidates_count: Option<usize>,
        /// Structured decision data as JSON
        #[serde(skip_serializing_if = "Option::is_none")]
        raw_decision: Option<serde_json::Value>,
        timestamp: i64,
    },

    /// Agentic action executed (execute phase)
    AgenticActionExecuted {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        /// Iteration number
        iteration: usize,
        /// Action type (click, type, navigate, etc.)
        action_type: String,
        /// Target (selector or description)
        target: String,
        /// Success or failure
        success: bool,
        /// Latency in milliseconds
        latency_ms: u64,
        /// Error message if failed
        error: Option<String>,
        timestamp: i64,
    },

    /// Legacy event retained for old clients that listened for coordinate fallback.
    /// Current browser automation returns failures to the visible agent loop instead.
    AgenticClickFallbackUsed {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        /// Iteration number
        iteration: usize,
        /// Original selector that failed
        original_selector: String,
        /// Error from original click attempt
        original_error: String,
        /// Fallback coordinates used (x, y in CSS pixels)
        coordinates: (f64, f64),
        /// Whether the fallback succeeded
        fallback_success: bool,
        /// Error from fallback if it failed
        fallback_error: Option<String>,
        /// Total latency including fallback
        latency_ms: u64,
        timestamp: i64,
    },

    /// Agentic execution completed
    AgenticExecutionCompleted {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        /// Outcome type: "success", "failed", "max_iterations_reached", "loop_detected",
        /// "waiting_for_user", "waiting_for_confirmation", "budget_exhausted", "cannot_proceed"
        outcome: String,
        /// Total iterations used
        iterations_used: usize,
        /// Final artifacts produced
        artifacts: Vec<String>,
        /// Total duration in milliseconds
        duration_ms: u64,
        /// Summary of what was accomplished
        summary: String,
        timestamp: i64,
        // --- Loop detection details (only present when outcome = "loop_detected") ---
        /// Type of loop: "state_loop", "action_cycle", "no_progress"
        #[serde(skip_serializing_if = "Option::is_none")]
        loop_detection_type: Option<String>,
        /// The repeated action signature
        #[serde(skip_serializing_if = "Option::is_none")]
        loop_repeated_action: Option<String>,
        /// Human-readable recommendation for breaking the loop
        #[serde(skip_serializing_if = "Option::is_none")]
        loop_recommendation: Option<String>,
        /// For action cycles: the pattern of actions (e.g., ["click #btn", "navigate /page"])
        #[serde(skip_serializing_if = "Option::is_none")]
        loop_cycle_pattern: Option<Vec<String>>,
        /// Similarity score (for state_loop and no_progress)
        #[serde(skip_serializing_if = "Option::is_none")]
        loop_similarity: Option<f64>,
        // --- Budget exhaustion details (only present when outcome = "budget_exhausted") ---
        /// Which budget dimension was exceeded
        #[serde(skip_serializing_if = "Option::is_none")]
        budget_dimension: Option<String>, // "time", "cost", "iterations", "llm_calls", "actions"
        /// Budget usage details (JSON object with used/limit)
        #[serde(skip_serializing_if = "Option::is_none")]
        budget_details: Option<String>,
        // --- Cannot proceed details (only present when outcome = "cannot_proceed") ---
        /// Reason why the agent cannot proceed
        #[serde(skip_serializing_if = "Option::is_none")]
        cannot_proceed_reason: Option<String>,
        // --- Multi-pass refinement (tactical pattern T1) ---
        /// `0` for the original execution; positive when this completion
        /// event represents a refinement pass. UI listeners use this to
        /// recognise an intermediate completion (refinement_pass_index=0
        /// with a partial outcome that will be refined) followed by a
        /// final completion (refinement_pass_index>0). When two events
        /// arrive for the same execution_id, the higher-index one is
        /// authoritative.
        #[serde(default, skip_serializing_if = "is_zero_u32")]
        refinement_pass_index: u32,
        /// `true` when the runtime will commission a refinement pass
        /// after this event (only meaningful on
        /// `refinement_pass_index = 0` events). Lets the UI mark the
        /// current state as "intermediate — refining" rather than
        /// "completed".
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        refinement_pending: bool,
        /// Structured yield payload — present when the LLM terminated
        /// via `Decision::Yield`. Carries `summary` / `completed[]` /
        /// `open[]` / `blockers[]` / `next_step_hint` plus a
        /// `disposition` string (`completed` / `partial_success` /
        /// `failed` / `retry_transient`). UI surfaces use this to
        /// render a partial-progress card with the structured fields
        /// directly, without re-parsing the partial_findings.md
        /// artifact. None for legacy GoalReached / CannotProceed /
        /// NeedUserInput terminations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        yield_payload:
            Option<crate::magician_v2::execution::execution_summary::YieldPayloadSummary>,
    },

    /// V3 task-scoped planning started for a task plan.
    V3PlanningStarted {
        principal: String,
        workspace: String,
        task_id: String,
        task_title: String,
        agent_id: String,
        plan_id: String,
        ui_thread_id: String,
        timestamp: i64,
    },

    /// V3 task-scoped planning phase progress.
    V3PlanningProgress {
        principal: String,
        workspace: String,
        task_id: String,
        task_title: String,
        agent_id: String,
        plan_id: String,
        ui_thread_id: String,
        phase: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        timestamp: i64,
    },

    // `V3PlanningClarificationNeeded` / `V3PlanningClarificationResolved`
    // removed in H7.3 (2026-05-11). Emit sites retired in H6.3; canonical
    // `HitlRequested` / `HitlResolved` envelopes with `source:
    // "clarification"` are the wire shape going forward.
    /// V3 task-scoped planning completed and produced a reviewable draft.
    V3PlanningCompleted {
        principal: String,
        workspace: String,
        task_id: String,
        task_title: String,
        agent_id: String,
        plan_id: String,
        ui_thread_id: String,
        timestamp: i64,
    },

    /// V3 task-scoped planning failed.
    V3PlanningFailed {
        principal: String,
        workspace: String,
        task_id: String,
        task_title: String,
        agent_id: String,
        plan_id: String,
        ui_thread_id: String,
        error: String,
        timestamp: i64,
    },

    /// Taskplan step started — synthesized backfill paired with `AgenticStepCompleted` /
    /// `AgenticStepFailed`. The decision LLM has no explicit "step start" signal; the runtime
    /// emits this immediately before the matching end event so subscribers (chat, V3 canonical
    /// event triggers, observability) see a paired started/finished lifecycle without re-parsing
    /// the markdown plan body. Tracked via `started_step_ids: HashSet<String>` so each step
    /// emits exactly one `AgenticStepStarted` per execution.
    AgenticStepStarted {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        iteration: usize,
        timestamp: i64,
    },

    /// Taskplan step completed — LLM signals step objective achieved (V3 canonical event trigger)
    AgenticStepCompleted {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        iteration: usize,
        timestamp: i64,
    },

    /// Taskplan step failed — LLM signals step is blocked/impossible (V3 canonical event trigger)
    AgenticStepFailed {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        iteration: usize,
        timestamp: i64,
    },

    /// Agentic execution paused waiting for user confirmation of a destructive action
    ///
    /// This event is emitted when the agentic loop needs user confirmation before
    /// executing a potentially dangerous action (delete, navigate to new domain, etc.)
    ///
    /// # Phase H5.5 — lifecycle vs HITL split
    ///
    /// Two distinct semantics ride on this one variant today:
    ///
    /// 1. **Lifecycle marker** — "the agent reached the
    ///    `WaitingForConfirmation` state on iteration N." Surfaces:
    ///    operator ticker (`EventStreamCard`), execution-panel
    ///    timeline, `transport_log`. **Fields needed:**
    ///    `execution_id`, `plan_id`, `step_id`, `iteration`,
    ///    `pause_state_id`, `agent_id`/`goal_id`/`cycle_id`,
    ///    `principal`/`workspace`, `timestamp`.
    ///
    /// 2. **HITL request payload** — "a human needs to answer this
    ///    confirmation." Surfaces: `pendingHitlStore` (resolved via
    ///    canonical `HitlRequested`), `AttentionPromptModal` (driven
    ///    by `respondToHitl(canonical envelope)`). **Fields:**
    ///    `action_summary`, `reason`, `action_type`.
    ///
    /// `emit_hitl_requested_for_agentic_confirmation` already dual-
    /// emits the canonical `HitlRequested { source: "agentic", ... }`
    /// carrying the HITL fields in `input_schema`. When H6.5 drops
    /// this legacy emit, the canonical envelope is the sole carrier
    /// of `action_summary` / `reason` / `action_type`; lifecycle
    /// surfaces only ever needed the slim fields above.
    ///
    /// **H7 target shape** (after legacy emit retires): keep the
    /// lifecycle fields, drop `action_summary` / `reason` /
    /// `action_type` — the canonical envelope already carries them.
    /// Lifecycle marker — slimmed in H7.4 (2026-05-12). HITL payload
    /// (`action_summary` / `reason` / `action_type`) was retired here;
    /// the canonical `HitlRequested { source: "agentic" }` envelope
    /// emitted by `emit_hitl_requested_for_agentic_confirmation` is the
    /// sole carrier of the human-response payload going forward.
    AgenticWaitingForConfirmation {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        /// Current iteration when paused
        iteration: usize,
        /// Pause state ID for resume routing
        #[serde(skip_serializing_if = "Option::is_none")]
        pause_state_id: Option<String>,
        // Agent routing (TRUE_AGENTS Phase 0)
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        goal_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cycle_id: Option<String>,
        timestamp: i64,
    },

    /// Agentic execution paused waiting for user input
    ///
    /// This event is emitted when the agentic loop needs user input to proceed.
    /// The frontend should display a prompt to the user based on the input_type.
    ///
    /// # Phase H5.5 — lifecycle vs HITL split
    ///
    /// Same dual-semantic structure as `AgenticWaitingForConfirmation`:
    ///
    /// 1. **Lifecycle marker** — "the agent reached `WaitingForUser`
    ///    on iteration N." Fields: `execution_id`, `plan_id`,
    ///    `step_id`, `iteration`, `pause_state_id`, scope, agent
    ///    routing, `is_retry` / `retry_count` (retry attempt history
    ///    is lifecycle-flavored: it describes pause-state evolution,
    ///    not the question itself), `escalation_trigger` (lifecycle
    ///    cause). `timestamp`.
    ///
    /// 2. **HITL request payload** — covered by the canonical
    ///    `HitlRequested { source: "agentic", input_schema, prompt,
    ///    hint, ... }` dual-emitted from
    ///    `emit_hitl_requested_for_agentic_pause`. Fields:
    ///    `question`, `input_type`, `hint`, `options`,
    ///    `input_schema`, `previous_answer`, `retry_reason`.
    ///
    /// **H7 target shape:** keep the lifecycle fields, drop the
    /// HITL-only ones (`question`, `input_type`, `hint`, `options`,
    /// `input_schema`, `previous_answer`, `retry_reason`).
    /// `is_retry` / `retry_count` stay — they encode pause-state
    /// evolution which is lifecycle, not the question's payload.
    /// Lifecycle marker — slimmed in H7.4 (2026-05-12). HITL payload
    /// (`question` / `input_type` / `hint` / `options` / `input_schema`
    /// / `previous_answer` / `retry_reason`) was retired here; the
    /// canonical `HitlRequested { source: "agentic" }` envelope emitted
    /// by `emit_hitl_requested_for_agentic_pause` is the sole carrier of
    /// the human-response payload. `is_retry` / `retry_count` /
    /// `escalation_trigger` stay because they describe pause-state
    /// evolution, not the question itself.
    AgenticWaitingForUser {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        /// Current iteration when paused
        iteration: usize,
        /// Pause state ID for resume routing
        #[serde(skip_serializing_if = "Option::is_none")]
        pause_state_id: Option<String>,
        /// Correlation id shared with the sibling `hitl.requested` +
        /// `hitl.resolved` rows so the attention page can pair BOTH request
        /// halves against one resolution. Set only for diff_approval pauses
        /// (the `ccp-<uuid>` proposal/approval id); `None` for all other
        /// pause kinds, whose `input.requested` id space is the pause
        /// storage_key and needs no separate correlation.
        #[serde(skip_serializing_if = "Option::is_none")]
        correlation_id: Option<String>,
        /// Whether this is a retry (re-asking after invalid/insufficient answer)
        #[serde(skip_serializing_if = "Option::is_none")]
        is_retry: Option<bool>,
        /// Number of previous attempts (0 = first ask)
        #[serde(skip_serializing_if = "Option::is_none")]
        retry_count: Option<usize>,
        // Agent routing (TRUE_AGENTS Phase 0)
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        goal_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cycle_id: Option<String>,
        /// Set when this pause was triggered by failure escalation (on_failure: ask_user).
        /// Values: "cannot_proceed", "loop_detected". None for normal user input requests.
        #[serde(skip_serializing_if = "Option::is_none")]
        escalation_trigger: Option<String>,
        timestamp: i64,
    },

    /// Agentic execution reached max iterations and paused.
    ///
    /// # Phase H5.5 — already in the target slim-lifecycle shape
    ///
    /// This variant is the reference for what `AgenticWaitingForUser`
    /// / `AgenticWaitingForConfirmation` should look like after H7
    /// retires their HITL-payload fields: only lifecycle data
    /// (`execution_id`, `iterations_used`, `pause_state_id`, scope,
    /// agent routing, `timestamp`). The matching
    /// `RuntimeTransportEvent::HitlRequested { source: "agentic",
    /// input_schema: { pause_kind: "max_iterations", ... } }` carries
    /// the human-response payload; this lifecycle marker carries the
    /// observability state.
    AgenticMaxIterationsReached {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        step_id: String,
        iterations_used: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        pause_state_id: Option<String>,
        // Agent routing (TRUE_AGENTS Phase 0)
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        goal_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cycle_id: Option<String>,
        timestamp: i64,
    },

    /// DOM change detected via SSE backchannel from magicutor (GD-B01).
    /// Emitted when browser actions cause DOM mutations, enabling live selector
    /// highlighting and real-time page change visibility.
    DomChangeDetected {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        /// Correlation ID for SSE connection
        correlation_id: String,
        /// Total DOM changes detected
        total_changes: u32,
        /// Nodes added
        nodes_added: u32,
        /// Nodes removed
        nodes_removed: u32,
        /// Significant signals (e.g., "navigation_occurred", "popup_opened")
        signals: Vec<String>,
        /// URL before the change
        initial_url: Option<String>,
        /// URL after the change
        final_url: Option<String>,
        /// Type of browser action that triggered this impact (e.g., "Click", "Type")
        #[serde(default)]
        action_type: Option<String>,
        /// CSS selector of the target element (if element-targeting action)
        #[serde(default)]
        selector: Option<String>,
        /// Outcome of the action: "success" or "failed"
        #[serde(default = "default_outcome")]
        outcome: String,
        timestamp: i64,
    },

    /// Agentic execution resumed after user input
    AgenticResumed {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pause_state_id: Option<String>,
        plan_id: String,
        step_id: String,
        /// Iteration we're resuming from
        resumed_from_iteration: usize,
        /// Type of input that was provided
        input_type: String,
        /// Whether the user provided input or aborted
        user_responded: bool,
        // Agent routing (TRUE_AGENTS Phase 0)
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        goal_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cycle_id: Option<String>,
        timestamp: i64,
    },

    // ====================================================================
    // Agent Lifecycle Events (TRUE_AGENTS Phase 0 — skeletal)
    // ====================================================================
    /// An agent cycle has started (an agent began working on a goal).
    AgentCycleStarted {
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        agent_id: String,
        goal_id: String,
        cycle_id: String,
        execution_id: Option<String>,
        goal: String,
        timestamp: i64,
    },

    /// An agent cycle has completed (success, failure, or pause).
    AgentCycleCompleted {
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        agent_id: String,
        goal_id: String,
        cycle_id: String,
        execution_id: Option<String>,
        outcome: String, // "success", "failure", "paused", "cancelled"
        iterations_used: usize,
        timestamp: i64,
    },

    /// An agent has been triggered (e.g. by a schedule, webhook, or user request).
    AgentTriggered {
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        agent_id: String,
        goal_id: String,
        trigger: String, // "manual", "schedule", "webhook", "event"
        timestamp: i64,
    },

    /// Generic agent event envelope for extensible agent/workflow events.
    /// Existing typed variants remain for backward compatibility.
    AgentEvent { event: AgentEventEnvelope },

    // ============================================================
    // Sub-goal Lifecycle Events (TRUE_AGENTS Phase 5)
    // Serialized as PascalCase event_type (e.g. "SubGoalRequested"),
    // consistent with all other variants in this enum.
    // ============================================================
    /// A sub-goal was requested (SpawnSubGoal decision)
    SubGoalRequested {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        parent_step_id: String,
        sub_goal: String,
        budget_iterations: usize,
        depth: usize,
        timestamp: i64,
    },

    /// A sub-goal execution completed
    SubGoalOutcome {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        plan_id: String,
        parent_step_id: String,
        sub_goal: String,
        outcome: String,
        iterations_used: usize,
        duration_ms: u64,
        timestamp: i64,
    },

    // ============================================================
    // Shell Output Streaming Events
    // ============================================================
    /// Streaming shell output chunk from `execute_bash_action`.
    ///
    /// Chunks are batched every 100ms and broadcast as a side-effect.
    /// The final chunk carries `is_final: true` and the process exit code.
    ShellOutputChunk {
        execution_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        step_id: String,
        step_index: usize,
        /// The shell command (populated on the first chunk only, empty after)
        command: String,
        /// Which output stream: "stdout" or "stderr"
        stream: String,
        /// Batched output lines (may contain multiple \n-separated lines)
        data: String,
        /// Incremental counter per step (0-indexed)
        sequence: u32,
        /// True on the last chunk (process exited)
        is_final: bool,
        /// Process exit code (only on is_final=true)
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        timestamp: i64,
    },

    // ============================================================
    // Interactive PTY chunk (Developer Mode — live xterm.js streaming)
    // ============================================================
    /// One chunk of stdout/stderr bytes from a live PTY session managed
    /// by `interactive_process::start_session`. Broadcast alongside the
    /// session's internal `output_buffer` write so the UI's xterm.js
    /// pane sees every byte without waiting for the agent's next
    /// `op=read` call.
    ///
    /// Bytes are emitted base64-encoded for SSE/JSON safety because the
    /// PTY stream may contain arbitrary binary (cursor escapes, alt-screen
    /// sequences, OSC queries, etc.) that breaks plain-string JSON. The
    /// frontend xterm pane base64-decodes before calling `term.write()`.
    ///
    /// See `docs/plans/2026-05-13-developer-mode-workbench.md` Phase 1.
    InteractivePtyChunk {
        session_id: String,
        principal: String,
        workspace: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        ui_thread_id: Option<String>,
        /// Optional program name (e.g. "claude", "codex") for UI tab labels.
        #[serde(skip_serializing_if = "Option::is_none")]
        program: Option<String>,
        /// Absolute byte offset where this chunk starts in the
        /// session's non-draining replay stream.
        offset_start: u64,
        /// Absolute byte offset immediately after this chunk.
        offset_end: u64,
        /// Base64-encoded raw bytes from the PTY master. Decode before
        /// `term.write()`.
        bytes_b64: String,
        /// Wall-clock timestamp when the bytes were read off the PTY.
        timestamp_ms: i64,
    },

    // ============================================================
    // UserRequest Events (central request/response service)
    // ============================================================
    // `UserRequestPending` / `UserRequestResolved` were removed in H7.1
    // (2026-05-11). All emit sites + the typed-variant consumers were
    // retired in H6.1; the canonical `HitlRequested` / `HitlResolved`
    // envelopes with `source: "user_request"` are the wire shape going
    // forward.
    /// Canonical HITL request event — Phase H2 of the HITL standardization
    /// plan (see `docs/archive/plans/2026-05-10-execution-panel-canvas-redesign.md`).
    ///
    /// Emitted alongside legacy events (`AgenticWaitingForUser`,
    /// `AgenticWaitingForConfirmation`, `approval.requested`) so frontend
    /// consumers can subscribe to one canonical shape instead of fanning
    /// across multiple distinct event types. The `correlation_id` matches
    /// the canonical identifier of the originating event (`pause_state_id`
    /// for agentic pauses, the `request_id` for user_request, the
    /// `approval_id` for approvals) so dual-emit deduplication is possible
    /// if both legacy + canonical fire.
    ///
    /// Phase H3 (`/api/.../hitl/{correlation_id}/respond`) will use this
    /// event as the authoritative request shape; Phase H4 reconciles
    /// planning vs execution pause/resume mechanics on top of it.
    HitlRequested {
        /// Canonical identifier — `pause_state_id` / `request_id` /
        /// `approval_id` depending on the source. Stable for the lifetime
        /// of the request; the matching `HitlResolved` carries the same
        /// id so consumers can deduplicate locally.
        correlation_id: String,
        /// Where the request originated. One of: `agentic`, `primitive`,
        /// `user_request`, `approval`, `clarification`, `escalation`, `bot_auth`,
        /// `mcp_oauth`.
        source: String,
        /// Input shape the user must provide. One of: `text`, `password`,
        /// `otp`, `choice`, `multi_choice`, `confirmation`, `external_action`,
        /// `file_path`, `guidance`, `tool_authorization`, `sandbox_override`,
        /// `diff_approval`, `form`.
        input_type: String,
        /// Prompt shown to the user (the question, summary, or escalation
        /// banner — adapter-specific).
        prompt: String,
        /// Optional supplementary hint.
        #[serde(skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
        /// Full typed input schema (placeholder, options, multiline,
        /// allow_other, min_selections, max_selections, etc.). Mirrors
        /// the `input_schema` JSON the legacy events carry. Frontend
        /// `HitlInputSchema` consumes this directly. When the ask collects
        /// a secret, `sensitive` carries the value-free `SensitiveInputSpec`
        /// (P3) — for `user_request` and `agentic` sources — so a client
        /// masks by it rather than by the request-type name.
        #[serde(skip_serializing_if = "Option::is_none")]
        input_schema: Option<Value>,
        /// Scope identifiers — at least one should be present.
        #[serde(skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// Canonical HITL resolution event. Mirrors `HitlRequested` and is
    /// emitted alongside legacy resolution events (`AgenticResumed`,
    /// approval-resolved). Frontend `pendingHitlStore` reads this to drop
    /// completed requests from the pending count.
    HitlResolved {
        /// Same `correlation_id` as the originating `HitlRequested`.
        correlation_id: String,
        /// Where the request originated (kept for reconstructing routing
        /// without subscribing to `HitlRequested`).
        source: String,
        /// `responded` (operator answered), `expired`, `cancelled`, or
        /// `dismissed`. Drives whether the resolution is shown as a
        /// success or as a closed-without-action state.
        outcome: String,
        /// Optional canonical decision (e.g., `approve` / `reject` for
        /// approvals, `confirm` / `deny` for confirmations). Free-form
        /// per source; consumers should treat it as opaque text.
        #[serde(skip_serializing_if = "Option::is_none")]
        decision: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    // `UserRequestResolved` removed in H7.1 — superseded by canonical
    // `HitlResolved { source: "user_request", ... }`.
    /// A critical request's alert offered to the bot of one channel type
    /// (secure HITL plan §6.1, P5). Value-free and address-free: the safe card
    /// only; the channel's bot claims the delivery over the authenticated API
    /// (`POST /api/magician/v2/hitl/deliveries/{id}/claim`) to learn the owner
    /// address and bind the record to itself, then reports the provider's
    /// answer. Scoped: only the owning scope's realtime clients see it.
    CriticalRequestAlert {
        delivery_id: String,
        correlation_id: String,
        channel_type: String,
        /// `request` or `test`.
        kind: String,
        /// The safe card: `service_alias`, `reason`, `text`, optional
        /// `deadline_ms`, optional `open_url`. Never a prompt, an option, an
        /// answer or a token.
        alert: Value,
        revision: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        deadline_ms: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// A delivered critical-request alert is no longer actionable — the
    /// request was answered, expired, cancelled or superseded. The bot that
    /// claimed the delivery edits or annotates what it sent.
    CriticalRequestRetired {
        delivery_id: String,
        correlation_id: String,
        channel_type: String,
        /// `responded`, `expired`, `cancelled`, `dismissed` or `superseded`.
        outcome: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// The safe state of automatic verification-code retrieval for one
    /// pending `otp` ask (secure HITL plan §6.2, P6): `waiting`, `code_used`,
    /// `ambiguous`, `unavailable`, `stopped`, with the source kinds watched
    /// and a value-free reason. Never the code, never the message.
    VerificationRetrievalStatus {
        correlation_id: String,
        status: String,
        sources: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        timestamp: i64,
    },

    /// An agent definition changed and active session-scoped chat lifecycle
    /// subscriptions should be re-materialized.
    AgentDefinitionChanged {
        principal: String,
        workspace: String,
        agent_id: String,
        timestamp: i64,
    },

    /// Scoped task was created.
    TaskCreated {
        principal: String,
        workspace: String,
        task_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ui_thread_id: Option<String>,
        title: String,
        created_at: i64,
        updated_at: i64,
        timestamp: i64,
    },

    /// Scoped task metadata changed (for example title, thread, or assignee).
    TaskUpdated {
        principal: String,
        workspace: String,
        task_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ui_thread_id: Option<String>,
        title: String,
        updated_at: i64,
        timestamp: i64,
    },

    /// Scoped task was deleted.
    TaskDeleted {
        principal: String,
        workspace: String,
        task_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ui_thread_id: Option<String>,
        title: String,
        deleted_at: i64,
        timestamp: i64,
    },

    /// Feed projection inserted a new scoped feed item.
    FeedItemCreated {
        item: FeedItem,
        /// Required for chronological sort across the unified event
        /// stream. The inner `item.created_at` lives under
        /// `data.item.created_at` on the wire — the frontend's
        /// `extractTimestamp` ladder only reads the top-level / `data.*`
        /// / `payload.*` keys, so without this field the row would
        /// arrive without a usable timestamp. Set at emission time
        /// (ms epoch).
        #[serde(default)]
        timestamp: i64,
    },

    /// Feed projection patched an existing scoped feed item.
    FeedItemUpdated {
        principal: String,
        workspace: String,
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ui_thread_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        patch: FeedItemPatch,
        /// Required for chronological sort across the unified event
        /// stream. Set at emission time (ms epoch).
        #[serde(default)]
        timestamp: i64,
    },

    /// Feed projection removed a scoped feed item.
    FeedItemRemoved {
        principal: String,
        workspace: String,
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ui_thread_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        /// Required for chronological sort across the unified event
        /// stream. Set at emission time (ms epoch).
        #[serde(default)]
        timestamp: i64,
    },

    /// Curated execution-panel state delta for a scoped task or execution.
    ExecutionPanelDelta {
        principal: String,
        workspace: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
        state: ExecutionPanelState,
        timestamp: i64,
    },

    // ============================================================
    // Activity — unified runtime activity view
    // ============================================================
    // Emitted by the tracing layer described in
    // `docs/plans/2026-08-14-unified-runtime-activity-view.md`, which turns
    // every instrumented span into a started/finished pair and every INFO+
    // log line into a progress row. Being observable is a property of
    // running inside a span, so background work (distillation,
    // classification, memory consolidation, taste profiling) shows up
    // without each author remembering to emit.
    //
    // PRIVACY — this whole family crosses a websocket to a browser and
    // there is no server-side redaction pass between the emit site and the
    // socket. `name`, `target`, and `kind` are code identifiers by
    // construction (span name, tracing target, span family). `message` is
    // the one field that is free-form; see the note on
    // [`RuntimeTransportEvent::ActivityProgress`].
    //
    // SCOPE — all three variants carry `principal` / `workspace` and all
    // three route per-workspace. They must stay that way as a set: an
    // `ActivityStarted` a user can see whose `ActivityFinished` routes
    // elsewhere leaves that user's activity tree permanently half-built,
    // showing work that begins and never ends. The scope decision is
    // spelled out in `api/websocket_handler.rs::event_visible_to_scope`
    // and mirrored in `transport_log.rs::optional_scope`; adding a fourth
    // variant here means adding it to both.
    //
    // A span that declares no scope, and inherits none, falls back to the
    // DEFAULT scope (`anonymous`/`default`) — not `system`/`system`.
    // `system` means genuinely runtime-wide work (cron tickers, capability
    // bootstrap); memory consolidation, taste distillation, LLM dispatch
    // and capability invocation are work done *for* a principal that
    // merely failed to declare one. Filing them under `system` both
    // mislabelled them and forced every viewer to subscribe to a bucket
    // shared across principals in order to see ordinary background work.
    // Genuinely cross-scope passes declare `system`/`system` positively
    // (see `attention_rank_recompute_pass`) rather than being inferred
    // from silence — inference from absence is what conflated the two.
    //
    // The fallback is a signal, not a home: every span landing in the
    // default scope is one that forgot to say who it was for. If that
    // bucket starts carrying volume, instrument those spans; do not widen
    // the bucket.
    /// A unit of runtime work opened a span.
    ///
    /// Pairs 1:1 with an [`RuntimeTransportEvent::ActivityFinished`]
    /// carrying the same `activity_id`.
    ActivityStarted {
        /// Identity of this span instance. Correlates the matching
        /// `ActivityFinished` and every `ActivityProgress` beneath it.
        activity_id: String,
        /// Enclosing span, when this span opened inside another. `None`
        /// at the root of an activity tree.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_activity_id: Option<String>,
        /// Span name — a code identifier, never user content.
        name: String,
        /// Tracing target (module path) the span was opened from.
        target: String,
        /// Coarse family the view groups by.
        ///
        /// A closed set, not free text: the layer normalises every
        /// declared `activity_kind` onto `ACTIVITY_KINDS` (`agent` /
        /// `background` / `capability` / `llm` / `process` / `runtime`)
        /// and falls back to a target heuristic when a span declares
        /// something unrecognised. That normalisation is what makes the
        /// "code identifier, never user content" claim above literally
        /// true for this field rather than true by convention.
        kind: String,
        /// Scope, when the span carries one. Optional because the layer
        /// sees spans opened from code with no principal in context —
        /// the same span-floor honesty `ActivityProgress` applies to its
        /// `activity_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        /// What this work *is for*, as opposed to what it is.
        ///
        /// `kind` above says a span is an LLM call; this says whether anyone
        /// is waiting on it. The two are independent — memory consolidation
        /// and an agent answering a user both produce `kind: "llm"` children,
        /// and only this distinguishes them.
        ///
        /// A closed set normalised onto `ACTIVITY_WORKLOAD_CLASSES`, which is
        /// deliberately the *same* set of strings
        /// `llm_dispatch_batch.workload_class` stores: a live row and an
        /// analytical row group under the same name and join without a
        /// translation table. A span declaring anything else arrives here as
        /// `None` rather than as a class the analytical store can never hold.
        ///
        /// **`None` means undeclared and must render as its own state.** It is
        /// not a default and must never be folded into a real class — the same
        /// discipline `ActivityFinished::outcome` applies with `closed`, and
        /// for the same reason: an unknown must not borrow a plausible answer.
        /// A growing undeclared bucket is the signal to instrument those roots.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workload_class: Option<String>,
        /// Which agent, thread and task this unit of work belongs to, when it
        /// belongs to one. Inherited from the nearest declaring ancestor, so a
        /// child span need not restate them.
        ///
        /// Identifiers only — never user content — and length-bounded by the
        /// layer on the way in. They are the columns
        /// `llm_dispatch_batch` already carries under the same names, so the
        /// view can group live work the way the analytical store groups
        /// history.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        /// The model this span dispatched to, on LLM spans.
        ///
        /// Known while the span is live — the router selects it at submit — so
        /// unlike cost it travels on the span rather than in a later event.
        /// Absent on every span that is not an LLM call, and absence renders as
        /// absent rather than as a placeholder.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// What the span asked the model (or the retrieval ladder) to do —
        /// the `LLMOperation` name, e.g. `agentic_decision`,
        /// `taste_profile_distill`, `channel_distill`.
        ///
        /// The one dimension here that **does not inherit**. Every other field
        /// on this event describes the subtree — whose work it is, what it is
        /// for — and a child that stays silent takes its ancestor's answer.
        /// An operation describes *this call and only this call*: handing it
        /// down would state that a span made a request it never made. So it is
        /// present exactly on the spans that declare it and absent everywhere
        /// else, and absence here is not a gap to go instrument.
        ///
        /// This is what makes a hundred `llm_dispatch` rows legible. The span
        /// name is the boundary's name and is identical on every one of them
        /// by construction; the operation is what differs. It is the same
        /// string `llm_dispatch_batch.operation` stores, so a live row and an
        /// analytical row name the operation identically.
        ///
        /// A code identifier, never user content: it comes from
        /// `LLMOperation::as_str`, whose open `Other` arm carries an operation
        /// name chosen at the call site. Length-bounded by the layer on the way
        /// in like every other identifier here.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation: Option<String>,
        /// Process-cumulative count of activity records the layer's
        /// bounded queue evicted before they could be emitted.
        ///
        /// Carried on every variant of this family rather than on a
        /// health event of its own, because it is a property of *this
        /// stream's completeness* — "some of the rows you are looking at
        /// are missing" belongs on the rows you are looking at. It is
        /// monotonic and stamped at drain time, so the newest event a
        /// viewer holds always carries the current total and a viewer
        /// that connects late learns the total from its first row.
        ///
        /// Always serialized (no `skip_serializing_if`) so a consumer can
        /// never confuse "absent" with "zero".
        #[serde(default)]
        dropped: u64,
        /// Identity of this emission. See
        /// [`RuntimeTransportEvent::ActivityProgress::seq`].
        #[serde(default)]
        seq: String,
        timestamp: i64,
    },

    /// The span opened by an [`RuntimeTransportEvent::ActivityStarted`]
    /// closed.
    ActivityFinished {
        /// Matches the `activity_id` of the opening `ActivityStarted`.
        activity_id: String,
        /// Wall-clock span lifetime, measured from span open time stored
        /// in the span's extensions.
        duration_ms: u64,
        /// Short status token, from the closed set `success` / `error` /
        /// `cancelled` / `closed`. A token, not an error body — error
        /// bodies can carry user content and this event goes to the
        /// browser, so anything the layer does not recognise collapses to
        /// `closed` rather than reaching the wire.
        ///
        /// `closed` is the honest default and the common case: it means
        /// the layer watched the span end without the span declaring an
        /// `activity_outcome`. It is deliberately not `success` — saying
        /// otherwise would make every abandoned unit of work look
        /// healthy. Consumers must handle it as its own state.
        outcome: String,
        /// Same scope the opening `ActivityStarted` carried. Present so
        /// the finish routes to the workspace that saw the start —
        /// without it the tree never closes for that viewer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        /// See [`RuntimeTransportEvent::ActivityStarted::dropped`].
        #[serde(default)]
        dropped: u64,
        /// Identity of this emission. See
        /// [`RuntimeTransportEvent::ActivityProgress::seq`].
        #[serde(default)]
        seq: String,
        timestamp: i64,
    },

    /// A log line emitted inside — or outside — an activity span.
    ///
    /// `activity_id` is deliberately optional: an INFO event emitted
    /// outside any instrumented span has no parent. That case is
    /// expected, not a bug. The design accepts a span floor and says so,
    /// and the view renders parentless rows loose at the root rather than
    /// dropping them. Do not make this field required.
    ActivityProgress {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        activity_id: Option<String>,
        /// Level of the originating tracing event ("info", "warn",
        /// "error"). The taxonomy severity of this variant is a static
        /// `Info` — a per-variant table cannot vary per instance — so
        /// consumers colour and threshold rows from this field, not from
        /// `taxonomy().severity`.
        level: String,
        /// The formatted log message.
        ///
        /// PRIVACY: this is the one field in the activity family that is
        /// not a code identifier, and it is broadcast to the browser
        /// verbatim. Emit sites are responsible for keeping credentials,
        /// prompt bodies, tool arguments, and user content out of the log
        /// lines they write — nothing between `tracing::info!` and the
        /// websocket redacts them.
        message: String,
        /// Tracing target (module path) that emitted the line.
        target: String,
        /// Scope of the enclosing span, when there is one. A parentless
        /// progress row (the span floor) usually has no scope either and
        /// routes to the system bucket.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        /// See [`RuntimeTransportEvent::ActivityStarted::dropped`].
        #[serde(default)]
        dropped: u64,
        /// Identity of this emission — unique across every event in the
        /// activity family, and stable for the life of the event.
        ///
        /// Exists because a consumer has to deduplicate. Backfill and the
        /// live tail overlap by design and deliver the same event twice,
        /// and until this field the only available key was the serialized
        /// line itself: `ActivityStarted` and `ActivityFinished` at least
        /// carry an `activity_id` to be idempotent on, but a progress row
        /// is a log line with nothing to distinguish it, so two identical
        /// messages logged in the same millisecond inside the same span
        /// were indistinguishable from one message delivered twice. One of
        /// them got silently dropped.
        ///
        /// Stamped once, when the record is drained onto the bus, so both
        /// delivery legs of the same event carry the same value. Drawn
        /// from the same process-unique counter as `activity_id`, so it
        /// survives a restart for the same reason: the view backfills a
        /// 24h window that spans earlier runs, and a counter restarting at
        /// a fixed base would collide with them.
        ///
        /// A decimal string, not a number, for the same reason
        /// `activity_id` is: the counter is seeded from a 63-bit random
        /// base, and a JSON number above 2^53 loses precision in a
        /// browser — two distinct values could round to the same one and
        /// silently deduplicate two different events into one. A string
        /// is exact, and a consumer only ever compares it.
        ///
        /// Always serialized, like `dropped`, so a consumer can never
        /// confuse "absent" with "empty".
        #[serde(default)]
        seq: String,
        timestamp: i64,
    },

    /// What a unit of work cost, delivered after the span that incurred it
    /// has already closed.
    ///
    /// **Why this is not a field on [`RuntimeTransportEvent::ActivityFinished`].**
    /// `llm_dispatch_rows` builds its rows in a sink task draining a broadcast
    /// channel; by the time a cost is known the submitting span is long gone,
    /// so there is no current span to read and nothing to attach it to. That
    /// is the same constraint that made `activity_id` ride on `JobOrigin`
    /// rather than being read at row-build time. Attaching cost to the finish
    /// event would mean either blocking the span until the provider answers,
    /// or reporting a cost of zero and correcting it later — a number that is
    /// wrong for a while is worse than one that arrives late.
    ///
    /// A consumer therefore sees this arrive *after* the `ActivityFinished`
    /// carrying the same `activity_id`, and must attach it to a span it
    /// already holds. A cost whose span is gone has nothing to attach to and
    /// is dropped: unlike a progress row it carries no readable content of its
    /// own.
    ActivityCost {
        /// The span this cost belongs to. Matches an earlier
        /// `ActivityStarted`/`ActivityFinished` pair.
        activity_id: String,
        /// Amount in millionths of one unit of `commodity`.
        ///
        /// Integer microunits rather than a float: costs are summed across
        /// many rows in the view, and binary floating point accumulates error
        /// over a long session in a figure an operator reads as money.
        cost_microunits: u64,
        /// What `cost_microunits` counts — `usd`, `tavily_credit`, and others.
        ///
        /// **Required, never optional.** Packages price in genuinely different
        /// commodities: `semantic-websearch-via-exa` reports `usd` while
        /// `news-search-via-tavily` reports `tavily_credit`, because Tavily
        /// returns no money figure at all. A spend figure that does not name
        /// what it counts cannot be compared or summed, and a consumer that
        /// added them would produce a confident wrong total. Show mixed
        /// commodities side by side; never add them.
        commodity: String,
        /// Token counts, when the provider reported them. Absent for priced
        /// work that is not token-metered.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u64>,
        /// Scope of the span this cost belongs to.
        ///
        /// Carried even though `activity_id` already identifies the span,
        /// because the websocket scope router filters each event on its own
        /// fields and cannot resolve an id back to an owner. Without these the
        /// only available decisions would be "deliver to everyone" — which
        /// publishes one principal's spend to every viewer — or "deliver to
        /// nobody". Both are worse than carrying two strings the sibling
        /// variants already carry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        principal: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        /// See [`RuntimeTransportEvent::ActivityStarted::dropped`].
        #[serde(default)]
        dropped: u64,
        /// See [`RuntimeTransportEvent::ActivityProgress::seq`].
        #[serde(default)]
        seq: String,
        timestamp: i64,
    },
}

/// Why a durable outbox record could not be replayed as the exact canonical
/// runtime fact its producer recorded.
///
/// Unlike [`RuntimeTransportBroadcaster::emit`], the recorded-route entry point
/// cannot fall back to the process-local scope registry: a cold projector is
/// specifically replaying a fact whose producing process may be gone. Returning
/// this refusal lets that projector leave its durable cursor below the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedRuntimeFactRefused {
    NotCanonicalInThisBuild,
    ExecutionMismatch { recorded: String, mapped: String },
    CanonicalSinkUnavailable,
    CanonicalSinkRefused { detail: String },
    HitlAdmissionRefused,
    HitlJournalUnavailable,
}

impl std::fmt::Display for RecordedRuntimeFactRefused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCanonicalInThisBuild => {
                formatter.write_str("this build cannot map the recorded event to a canonical fact")
            },
            Self::ExecutionMismatch { recorded, mapped } => write!(
                formatter,
                "the recorded canonical execution {recorded:?} conflicts with the event's mapped execution {mapped:?}"
            ),
            Self::CanonicalSinkUnavailable => {
                formatter.write_str("no canonical runtime-event sink is installed")
            },
            Self::CanonicalSinkRefused { detail } => {
                write!(formatter, "the canonical runtime-event sink refused durable admission: {detail}")
            },
            Self::HitlAdmissionRefused => {
                formatter.write_str("the recorded HITL event exceeds runtime admission")
            },
            Self::HitlJournalUnavailable => {
                formatter.write_str("the durable HITL lifecycle journal refused the event")
            },
        }
    }
}

impl std::error::Error for RecordedRuntimeFactRefused {}

pub use magician_event_taxonomy::{
    ChatRenderKind, CoalesceKey, EventCategory, EventSeverity, EventTaxonomy, RenderHint,
};

/// Single source of truth for event taxonomy.
///
/// Generates two outputs from one declarative table:
///   1. `RuntimeTransportEvent::taxonomy_lookup(&self)` — runtime match.
///   2. `EVENT_TAXONOMY_TABLE: &[(&str, EventTaxonomy)]` — iterable
///      catalog, used by `cargo run --bin event-taxonomy-dump` to emit
///      the TS mirror at `ui/unified-ui/src/lib/realtime/event-taxonomy.ts`.
///
/// Adding a new `RuntimeTransportEvent` variant requires a single line
/// added to the `taxonomies!` invocation below — the compiler enforces
/// exhaustiveness via the match arm; the catalog stays in sync because
/// both come from the same input.
macro_rules! taxonomies {
    (
        $(
            // Optional `#[doc = "..."]` / comment line above each entry
            // is allowed but not captured (use `//` for inline notes).
            $variant:ident => ($cat:ident, $sev:ident, $ur:expr)
        ),* $(,)?
    ) => {
        impl RuntimeTransportEvent {
            fn taxonomy_lookup(event: &Self) -> EventTaxonomy {
                match event {
                    $(
                        Self::$variant { .. } => EventTaxonomy::new(
                            EventCategory::$cat,
                            EventSeverity::$sev,
                            $ur,
                        ),
                    )*
                }
            }
        }

        /// The tagged variant names, as strings, from the same `$variant`
        /// tokens that generate the match above.
        ///
        /// This is the only compiler-backed list of real
        /// `RuntimeTransportEvent` names available to a string-keyed
        /// consumer. The match arms make rustc reject any name here that
        /// is not a variant; exhaustiveness makes it reject any variant
        /// missing from here. `EVENT_TAXONOMY_TABLE` in the taxonomy
        /// crate is built by `stringify!` with no enum in scope at all,
        /// so nothing ties its keys to reality — comparing it against
        /// this const is what closes that gap. See
        /// `every_taxonomy_table_row_has_a_transport_variant`.
        pub const TAGGED_TRANSPORT_EVENT_VARIANTS: &[&str] = &[
            $( stringify!($variant), )*
        ];

        /// The same rows again, carrying the taxonomy *values* — built in
        /// exactly the shape `EVENT_TAXONOMY_TABLE` uses so the two can be
        /// compared directly.
        ///
        /// `TAGGED_TRANSPORT_EVENT_VARIANTS` only closes the membership
        /// gap: it proves both tables list the same event names. It says
        /// nothing about the `(category, severity, user_relevant)` triple
        /// attached to each name, so a row promoted to `Warn` — or flipped
        /// to `user_relevant = true` — in one file and not the other would
        /// pass both membership gates. The `SOURCE_HASH` gate cannot see it
        /// either: that only detects "the Rust changed and codegen wasn't
        /// re-run", not "the two invocations disagree". This const is what
        /// makes the values comparable. See
        /// `taxonomy_table_values_match_transport_tags`.
        pub const TAGGED_TRANSPORT_EVENT_TAXONOMIES: &[(&str, EventTaxonomy)] = &[
            $(
                (
                    stringify!($variant),
                    EventTaxonomy::new(
                        EventCategory::$cat,
                        EventSeverity::$sev,
                        $ur,
                    ),
                ),
            )*
        ];
    };
}

// The iterable catalog (EVENT_TAXONOMY_TABLE) lives in the thin
// `magician-event-taxonomy` crate so `make event-taxonomy-codegen` only
// has to compile that crate (serde dep only) rather than all of magician.
pub use magician_event_taxonomy::EVENT_TAXONOMY_TABLE;

taxonomies! {
    // ─── Pipeline ────────────────────────────────────────────────────
    MessageProcessingStarted => (Pipeline, Info, true),
    QueryAnalysisCompleted => (Pipeline, Info, false),
    StrategySelected => (Pipeline, Decision, true),
    ExplorationProgress => (Pipeline, Info, false),
    MessageCompleted => (Pipeline, Info, true),
    ProcessingError => (Pipeline, Error, true),
    PipelineStarted => (Pipeline, Info, true),
    PipelineStepStarted => (Pipeline, Info, true),
    PipelineStepCompleted => (Pipeline, Info, true),
    PipelineCompleted => (Pipeline, Info, true),
    PipelineFailed => (Pipeline, Error, true),

    // ─── Plan (atomic plan generation + V3 task-scoped planning) ─────
    AtomicPlanOutlineStarted => (Plan, Info, true),
    AtomicPlanOutlineCompleted => (Plan, Info, true),
    AtomicPlanExpansionStarted => (Plan, Info, false),
    AtomicPlanGenerated => (Plan, Info, true),
    V3PlanningStarted => (Plan, Info, true),
    V3PlanningProgress => (Plan, Info, false),
    V3PlanningCompleted => (Plan, Info, true),
    V3PlanningFailed => (Plan, Error, true),

    // ─── Tool matching ───────────────────────────────────────────────
    ToolMatchingTierStarted => (Tool, Info, false),
    ToolMatchingTierCompleted => (Tool, Info, false),

    // ─── Slot extraction / parameter resolution ──────────────────────
    SlotExtractionStarted => (Slot, Info, false),
    SlotExtracted => (Slot, Info, false),
    SlotEnrichmentStarted => (Slot, Info, false),
    SlotEnrichmentCompleted => (Slot, Info, false),
    SlotConfidenceUpdated => (Slot, Info, false),
    SlotGraphDiff => (Slot, Info, false),
    ClarifiedTaskReady => (Slot, Info, true),
    ParameterInferenceAttempted => (Slot, Info, false),
    ParameterInferred => (Slot, Info, false),
    ParameterInferenceFailed => (Slot, Warn, false),
    ParameterDiscoveryAttempted => (Slot, Info, false),
    ParameterDiscovered => (Slot, Info, false),
    ParameterDiscoveryFailed => (Slot, Warn, false),
    ParameterResolutionProgress => (Slot, Info, false),

    // ─── Clarification (planning HITL) ───────────────────────────────
    ClarificationSessionSnapshot => (Clarification, Info, true),
    ClarificationConfidenceSnapshot => (Clarification, Info, false),
    ClarificationMetricsSnapshot => (Clarification, Info, false),

    // ─── Execution + Workflow lifecycle ──────────────────────────────
    ExecutionStarted => (Execution, Info, true),
    ExecutionStepStarted => (Execution, Info, true),
    ExecutionStepCompleted => (Execution, Info, true),
    ExecutionPaused => (Execution, Warn, true),
    ExecutionResumed => (Execution, Info, true),
    ExecutionFailed => (Execution, Error, true),
    ExecutionCancelled => (Execution, Warn, true),
    ExecutionCompleted => (Execution, Info, true),
    ExecutionInflightResent => (Execution, Info, false),
    ExecutionInflightDropped => (Execution, Warn, false),
    ExecutionRestoreFailed => (Execution, Error, true),
    ExecutionStatusChanged => (Execution, Info, false),
    ExecutionResponsibilityChanged => (Execution, Info, false),
    WorkflowResumed => (Execution, Info, true),
    WorkflowStageResumed => (Execution, Info, false),
    WorkflowResumeFailed => (Execution, Error, true),

    // ─── LLM I/O ─────────────────────────────────────────────────────
    LLMAnalysisStarted => (Llm, Info, false),
    LLMAnalysisCompleted => (Llm, Info, false),
    LLMAnalysisFailed => (Llm, Error, true),
    LLMRequestSent => (Llm, Info, false),
    LLMResponseReceived => (Llm, Info, false),
    InferenceAttempted => (Llm, Info, false),
    ThinkingModeActivated => (Llm, Info, true),
    ThinkingModeCompleted => (Llm, Info, false),

    // ─── Agentic loop ────────────────────────────────────────────────
    AgenticExecutionStarted => (Agentic, Info, true),
    AgenticExecutionCompleted => (Agentic, Info, true),
    AgenticIterationStarted => (Agentic, Info, false),
    AgenticIterationCompleted => (Agentic, Info, false),
    AgenticStepStarted => (Agentic, Info, true),
    AgenticStepCompleted => (Agentic, Info, true),
    AgenticStepFailed => (Agentic, Error, true),
    AgenticStepStuckWarning => (Agentic, Warn, true),
    AgenticPageUnderstanding => (Agentic, Info, false),
    AgenticDecisionMade => (Agentic, Decision, true),
    AgenticActionExecuted => (Agentic, Info, true),
    AgenticClickFallbackUsed => (Agentic, Warn, false),
    AgenticMaxIterationsReached => (Agentic, Warn, true),
    AgenticResumed => (Agentic, Info, true),
    DomChangeDetected => (Agentic, Info, false),
    SubGoalRequested => (Agentic, Decision, true),
    SubGoalOutcome => (Agentic, Info, true),

    // ─── HITL (execution-side AskUser / confirmation) ────────────────
    AgenticWaitingForUser => (Hitl, Attention, true),
    AgenticWaitingForConfirmation => (Hitl, Attention, true),
    // Canonical HITL events — Phase H2. Dual-emitted with the legacy
    // events above; consumers can subscribe to either shape.
    HitlRequested => (Hitl, Attention, true),
    HitlResolved => (Hitl, Info, true),
    // Critical-request delivery to a channel bot (P5): routing envelopes,
    // never shown as a user-facing card themselves.
    CriticalRequestAlert => (Hitl, Info, false),
    CriticalRequestRetired => (Hitl, Info, false),
    // Automatic verification-code retrieval status (P6): drives the safe
    // "waiting for the verification email" wording on the ask.
    VerificationRetrievalStatus => (Hitl, Info, true),

    // ─── Agent lifecycle ─────────────────────────────────────────────
    AgentCycleStarted => (Agent, Info, true),
    AgentCycleCompleted => (Agent, Info, true),
    AgentTriggered => (Agent, Info, true),
    AgentEvent => (Agent, Info, false),
    AgentDefinitionChanged => (Agent, Info, true),

    // ─── Task CRUD ───────────────────────────────────────────────────
    TaskCreated => (Task, Info, true),
    TaskUpdated => (Task, Info, false),
    TaskDeleted => (Task, Warn, true),

    // ─── Feed ────────────────────────────────────────────────────────
    FeedItemCreated => (Feed, Info, false),
    FeedItemUpdated => (Feed, Info, false),
    FeedItemRemoved => (Feed, Info, false),
    ExecutionPanelDelta => (Feed, Info, false),

    // ─── Observability (catch-all) ───────────────────────────────────
    Heartbeat => (Observability, Info, false),
    ObservabilityAlert => (Observability, Warn, true),
    ChatMessageReceived => (Observability, Info, true),
    ThinkingMapUpdated => (Observability, Info, false),
    ThinkingMapInterpretProgress => (Observability, Info, false),
    ProgressEvent => (Observability, Info, false),
    ShellOutputChunk => (Observability, Info, false),
    InteractivePtyChunk => (Observability, Info, false),

    // ─── Activity (unified runtime activity spans) ───────────────────
    // Not Observability: that bucket is the catch-all for one-off
    // telemetry (heartbeats, shell chunks, alerts). Activity is a
    // coherent, high-volume family the operator filters to as a whole.
    // `user_relevant=false` keeps it out of the notification overlay and
    // the feed — the activity view is an operator surface, and the
    // per-row level lives in the payload, not in the static severity.
    ActivityStarted => (Activity, Info, false),
    ActivityFinished => (Activity, Info, false),
    ActivityProgress => (Activity, Info, false),
    // Same family, same severity: a cost row is a fact about work that already
    // happened, not a condition. Spend that needs an operator's attention is a
    // budget concern and belongs to the budget events, which carry their own
    // severity — promoting this one to Warn would make every priced call look
    // like a problem.
    ActivityCost => (Activity, Info, false),
    DecisionShadowAgreement => (Activity, Info, false),
    DecisionAccountingGap => (Activity, Warn, false),
}

pub use magician_event_taxonomy::RuntimeAgentEventType;

pub use magician_event_taxonomy::GAUI_EVENT_TAXONOMY;

pub use magician_event_taxonomy::lookup_agent_event_taxonomy;

impl RuntimeTransportEvent {
    fn execution_id_for_scope_enrichment(&self) -> Option<&str> {
        match self {
            Self::MessageProcessingStarted { execution_id, .. }
            | Self::QueryAnalysisCompleted { execution_id, .. }
            | Self::StrategySelected { execution_id, .. }
            | Self::ExplorationProgress { execution_id, .. }
            | Self::ExecutionStarted { execution_id, .. }
            | Self::ExecutionStepStarted { execution_id, .. }
            | Self::ExecutionStepCompleted { execution_id, .. }
            | Self::ExecutionPaused { execution_id, .. }
            | Self::ExecutionResumed { execution_id, .. }
            | Self::ExecutionFailed { execution_id, .. }
            | Self::ExecutionCancelled { execution_id, .. }
            | Self::ExecutionInflightResent { execution_id, .. }
            | Self::ExecutionInflightDropped { execution_id, .. }
            | Self::ExecutionCompleted { execution_id, .. }
            | Self::ExecutionRestoreFailed { execution_id, .. }
            | Self::MessageCompleted { execution_id, .. }
            | Self::ExecutionStatusChanged { execution_id, .. }
            | Self::ExecutionResponsibilityChanged { execution_id, .. }
            | Self::ProcessingError { execution_id, .. }
            | Self::LLMAnalysisStarted { execution_id, .. }
            | Self::LLMAnalysisCompleted { execution_id, .. }
            | Self::LLMAnalysisFailed { execution_id, .. }
            | Self::ClarificationSessionSnapshot { execution_id, .. }
            | Self::ClarificationConfidenceSnapshot { execution_id, .. }
            | Self::SlotGraphDiff { execution_id, .. }
            | Self::WorkflowResumed { execution_id, .. }
            | Self::WorkflowStageResumed { execution_id, .. }
            | Self::WorkflowResumeFailed { execution_id, .. }
            | Self::AtomicPlanOutlineStarted { execution_id, .. }
            | Self::AtomicPlanOutlineCompleted { execution_id, .. }
            | Self::AtomicPlanExpansionStarted { execution_id, .. }
            | Self::AtomicPlanGenerated { execution_id, .. }
            | Self::ToolMatchingTierStarted { execution_id, .. }
            | Self::ToolMatchingTierCompleted { execution_id, .. }
            | Self::SlotExtractionStarted { execution_id, .. }
            | Self::SlotExtracted { execution_id, .. }
            | Self::SlotEnrichmentStarted { execution_id, .. }
            | Self::SlotEnrichmentCompleted { execution_id, .. }
            | Self::SlotConfidenceUpdated { execution_id, .. }
            | Self::ClarifiedTaskReady { execution_id, .. }
            | Self::ParameterInferenceAttempted { execution_id, .. }
            | Self::ParameterInferred { execution_id, .. }
            | Self::ParameterInferenceFailed { execution_id, .. }
            | Self::ParameterDiscoveryAttempted { execution_id, .. }
            | Self::ParameterDiscovered { execution_id, .. }
            | Self::ParameterDiscoveryFailed { execution_id, .. }
            | Self::ParameterResolutionProgress { execution_id, .. }
            | Self::ObservabilityAlert { execution_id, .. } => Some(execution_id.as_str()),
            Self::HitlRequested { execution_id, .. } | Self::HitlResolved { execution_id, .. } => {
                execution_id.as_deref()
            },
            _ => None,
        }
    }

    /// Operator-meaningful taxonomy for this event variant.
    ///
    /// Backed by the [`taxonomies!`] macro below — single source of truth
    /// across both the match (used at runtime) and `EVENT_TAXONOMY_TABLE`
    /// (used by `cargo run --bin event-taxonomy-dump` to emit the TS
    /// mirror at `ui/unified-ui/src/lib/realtime/event-taxonomy.ts`).
    /// Adding a new variant produces a compile error in the macro
    /// invocation until tagged.
    pub fn taxonomy(&self) -> EventTaxonomy {
        Self::taxonomy_lookup(self)
    }

    fn with_registered_scope_if_missing(mut self, scope: &CanonicalEventScope) -> Self {
        match &mut self {
            Self::MessageProcessingStarted {
                principal,
                workspace,
                ..
            }
            | Self::QueryAnalysisCompleted {
                principal,
                workspace,
                ..
            }
            | Self::StrategySelected {
                principal,
                workspace,
                ..
            }
            | Self::ExplorationProgress {
                principal,
                workspace,
                ..
            }
            | Self::ExecutionStarted {
                principal,
                workspace,
                ..
            }
            | Self::ExecutionStepStarted {
                principal,
                workspace,
                ..
            }
            | Self::ExecutionStepCompleted {
                principal,
                workspace,
                ..
            }
            | Self::ExecutionPaused {
                principal,
                workspace,
                ..
            }
            | Self::ExecutionResumed {
                principal,
                workspace,
                ..
            }
            | Self::ExecutionFailed {
                principal,
                workspace,
                ..
            }
            | Self::ExecutionCancelled {
                principal,
                workspace,
                ..
            }
            | Self::ExecutionInflightResent {
                principal,
                workspace,
                ..
            }
            | Self::ExecutionInflightDropped {
                principal,
                workspace,
                ..
            }
            | Self::ExecutionCompleted {
                principal,
                workspace,
                ..
            }
            | Self::ExecutionRestoreFailed {
                principal,
                workspace,
                ..
            }
            | Self::MessageCompleted {
                principal,
                workspace,
                ..
            }
            | Self::ExecutionStatusChanged {
                principal,
                workspace,
                ..
            }
            | Self::ExecutionResponsibilityChanged {
                principal,
                workspace,
                ..
            }
            | Self::ProcessingError {
                principal,
                workspace,
                ..
            }
            | Self::LLMAnalysisStarted {
                principal,
                workspace,
                ..
            }
            | Self::LLMAnalysisCompleted {
                principal,
                workspace,
                ..
            }
            | Self::LLMAnalysisFailed {
                principal,
                workspace,
                ..
            }
            | Self::ClarificationSessionSnapshot {
                principal,
                workspace,
                ..
            }
            | Self::ClarificationConfidenceSnapshot {
                principal,
                workspace,
                ..
            }
            | Self::SlotGraphDiff {
                principal,
                workspace,
                ..
            }
            | Self::WorkflowResumed {
                principal,
                workspace,
                ..
            }
            | Self::WorkflowStageResumed {
                principal,
                workspace,
                ..
            }
            | Self::WorkflowResumeFailed {
                principal,
                workspace,
                ..
            }
            | Self::AtomicPlanOutlineStarted {
                principal,
                workspace,
                ..
            }
            | Self::AtomicPlanOutlineCompleted {
                principal,
                workspace,
                ..
            }
            | Self::AtomicPlanExpansionStarted {
                principal,
                workspace,
                ..
            }
            | Self::AtomicPlanGenerated {
                principal,
                workspace,
                ..
            }
            | Self::ToolMatchingTierStarted {
                principal,
                workspace,
                ..
            }
            | Self::ToolMatchingTierCompleted {
                principal,
                workspace,
                ..
            }
            | Self::SlotExtractionStarted {
                principal,
                workspace,
                ..
            }
            | Self::SlotExtracted {
                principal,
                workspace,
                ..
            }
            | Self::SlotEnrichmentStarted {
                principal,
                workspace,
                ..
            }
            | Self::SlotEnrichmentCompleted {
                principal,
                workspace,
                ..
            }
            | Self::SlotConfidenceUpdated {
                principal,
                workspace,
                ..
            }
            | Self::ClarifiedTaskReady {
                principal,
                workspace,
                ..
            }
            | Self::ParameterInferenceAttempted {
                principal,
                workspace,
                ..
            }
            | Self::ParameterInferred {
                principal,
                workspace,
                ..
            }
            | Self::ParameterInferenceFailed {
                principal,
                workspace,
                ..
            }
            | Self::ParameterDiscoveryAttempted {
                principal,
                workspace,
                ..
            }
            | Self::ParameterDiscovered {
                principal,
                workspace,
                ..
            }
            | Self::ParameterDiscoveryFailed {
                principal,
                workspace,
                ..
            }
            | Self::ParameterResolutionProgress {
                principal,
                workspace,
                ..
            }
            | Self::ObservabilityAlert {
                principal,
                workspace,
                ..
            } => {
                if principal.is_none() {
                    *principal = Some(scope.principal.clone());
                }
                if workspace.is_none() {
                    *workspace = Some(scope.workspace.clone());
                }
            },
            // HitlRequested / HitlResolved enrich BOTH scope and task_id
            // from the registered execution scope. Sparse emitters like
            // `clarification_response_received` (which sets task_id /
            // agent_id / principal / workspace all to None) rely on
            // this so downstream consumers see the full scope.
            Self::HitlRequested {
                principal,
                workspace,
                task_id,
                ..
            }
            | Self::HitlResolved {
                principal,
                workspace,
                task_id,
                ..
            } => {
                if principal.is_none() {
                    *principal = Some(scope.principal.clone());
                }
                if workspace.is_none() {
                    *workspace = Some(scope.workspace.clone());
                }
                if task_id.is_none() {
                    *task_id = Some(scope.task_id.clone());
                }
            },
            _ => {},
        }
        self
    }

    /// Reconstruct a `CanonicalEventScope` from the event's own
    /// self-describing fields, WITHOUT consulting the live
    /// `runtime_canonical_event_scopes` registry.
    ///
    /// Used by [`RuntimeTransportBroadcaster::emit`] as a durability
    /// fallback: after a one-shot execution finishes (or across a
    /// process restart) the in-memory scope registration is gone, so a
    /// late `HitlResolved` would otherwise skip its durable
    /// `events.jsonl` write and the attention projection would rebuild
    /// the resolved card forever. Returns `Some` only when the event
    /// carries ALL four scope identifiers (principal + workspace +
    /// task_id + execution_id) so the reconstructed scope targets the
    /// correct per-execution log; events missing any of these still
    /// skip the write (no unambiguous target).
    ///
    /// `ui_thread_id` is not carried on these events and is not consumed
    /// by the attention projection, so it is defaulted to the empty
    /// string (the same value producers seed for non-chat scopes).
    fn self_describing_canonical_scope(&self) -> Option<CanonicalEventScope> {
        match self {
            Self::HitlRequested {
                task_id,
                execution_id,
                principal,
                workspace,
                ..
            }
            | Self::HitlResolved {
                task_id,
                execution_id,
                principal,
                workspace,
                ..
            } => Some(CanonicalEventScope {
                principal: principal.clone()?,
                workspace: workspace.clone()?,
                task_id: task_id.clone()?,
                execution_id: execution_id.clone()?,
                ui_thread_id: String::new(),
            }),
            _ => None,
        }
    }
}

/// Generic agent event envelope. New agent/workflow event types should use this
/// envelope instead of adding enum variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEventEnvelope {
    pub event_type: String, // e.g. "agent.cycle.started"
    pub agent_id: String,   // "__system__" for system-scoped events
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub payload: serde_json::Value, // Event-specific fields
    pub timestamp: i64,
}

impl AgentEventEnvelope {
    pub fn new(event_type: &str, agent_id: &str, payload: serde_json::Value) -> Self {
        Self {
            event_type: event_type.to_string(),
            agent_id: agent_id.to_string(),
            principal: None,
            workspace: None,
            payload,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }

    pub fn new_scoped(
        event_type: &str,
        agent_id: &str,
        principal: &str,
        workspace: &str,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            event_type: event_type.to_string(),
            agent_id: agent_id.to_string(),
            principal: Some(principal.to_string()),
            workspace: Some(workspace.to_string()),
            payload,
            timestamp: chrono::Utc::now().timestamp_millis(),
        }
    }
}

/// Candidate information for tier progress events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierCandidate {
    pub tool_name: String,
    pub score: f64,
    pub category: String,
}

// NOTE: ToolMatchStatus enum removed - ToolMatching event was removed
// NOTE: CancellationReason enum removed - ProcessingCancelled event was removed

/// Exploration summary for completed messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationSummary {
    pub strategy_used: StrategyType,
    pub nodes_explored: usize,
    pub max_depth_reached: usize,
    pub best_tool: String,
    pub best_confidence: f64,
    pub total_execution_time_ms: u64,
}

impl From<&ExplorationResult> for ExplorationSummary {
    fn from(result: &ExplorationResult) -> Self {
        // Get best node from best_path (last node ID in path)
        let best_node = result
            .best_path
            .last()
            .and_then(|node_id| result.all_nodes.get(node_id));

        // Extract best tool name and confidence
        let (best_tool, best_confidence) = if let Some(node) = best_node {
            let tool_name = node
                .tool_match
                .as_ref()
                .and_then(|tm| tm.primary_match.as_ref())
                .map(|pm| pm.tool_name.clone())
                .unwrap_or_else(|| "unknown".to_string());
            (tool_name, node.confidence)
        } else {
            ("unknown".to_string(), 0.0)
        };

        Self {
            strategy_used: result.strategy_metadata.strategy_type,
            nodes_explored: result.strategy_metadata.nodes_explored as usize,
            max_depth_reached: result.strategy_metadata.max_depth as usize,
            best_tool,
            best_confidence: best_confidence as f64,
            total_execution_time_ms: result.resources_consumed.time_ms,
        }
    }
}

/// Per-delegation fan-out target: stamp a copy of every transport
/// event whose payload carries this delegate's `execution_id` with
/// the chat session's agent identity so the chat's existing scoped
/// subscription receives reasoning / tool.call / plan events from
/// the delegated execution. Without this, those events live on the
/// delegate's `agent_id+scope` channel and never reach the chat
/// thread that spawned the delegation.
#[derive(Debug, Clone)]
pub struct ChatFanoutTarget {
    /// Chat session ID the events should land in (carried in
    /// `payload.chat_session_id` for UI filtering).
    pub chat_session_id: String,
    /// Personal-agent ID the chat thread is bound to. Becomes the
    /// envelope's `agent_id` so the chat's scoped subscription picks
    /// up the fanned-out copy.
    pub chat_agent_id: String,
    /// Scope identity for envelope routing (matches the chat thread's
    /// active workspace).
    pub principal: String,
    pub workspace: String,
    /// Chat turn correlation id. When set, the fan-out re-stamp pass
    /// injects this into every cloned payload's `chat_turn_id` field so
    /// downstream listeners (the UI's `RequestActivityCard` filtered by
    /// `/events?chat_turn_id=`) get sub-agent events under the same
    /// per-request umbrella as the chat-side LLM calls and tool calls
    /// that spawned them. None preserves the prior behaviour.
    pub chat_turn_id: Option<String>,
}

/// Lightweight, cycle-safe handle to the chat fan-out + canonical-scope
/// registries.
///
/// Holds only the registry maps (`Arc<DashMap<…>>`), NOT the
/// broadcaster's `broadcast::Sender`. A long-lived consumer such as
/// `ChatTurnEventSink` can therefore resolve chat-turn ids for events
/// that arrive without one, without keeping the broadcast channel open:
/// if it held the broadcaster itself, the sender would never drop and
/// the sink's `run` loop (which exits only on channel-closed) could
/// never shut down.
#[derive(Clone)]
pub struct ChatFanoutResolver {
    chat_fanout_by_task: Arc<DashMap<String, Vec<ChatFanoutTarget>>>,
    canonical_scopes: Arc<DashMap<String, CanonicalEventScope>>,
}

impl ChatFanoutResolver {
    /// True when at least one chat session has a registered fan-out.
    /// Hot-path consumers gate recovery work on this so the common
    /// "no chat is listening" case stays a single atomic check.
    pub fn has_fanout(&self) -> bool {
        !self.chat_fanout_by_task.is_empty()
    }

    /// Recover the chat-turn correlation id(s) + scope for an event that
    /// carries no `chat_turn_id` of its own.
    ///
    /// Inner-loop progress events (typed `Agentic*` / tool / llm
    /// transport variants emitted via `emit` / `emit_transport_only`)
    /// bypass the `emit_scoped_or_unscoped` chat-fan-out re-stamp, so
    /// they reach `ChatTurnEventSink` without a turn id and would be
    /// dropped — even when the chat session explicitly subscribed to the
    /// owning task. Resolve the owning `task_id` (explicit, else via the
    /// event's `execution_id` whose registered canonical scope names the
    /// task) and return `(chat_turn_id, principal, workspace)` for every
    /// registered fan-out target so the sink can persist + broadcast the
    /// event under each subscribed turn.
    pub fn resolve_turns(
        &self,
        task_id: Option<&str>,
        execution_id: Option<&str>,
    ) -> Vec<(String, String, String)> {
        let resolved_task_id = match task_id {
            Some(t) if !t.is_empty() => Some(t.to_owned()),
            _ => execution_id
                .and_then(|eid| self.canonical_scopes.get(eid).map(|s| s.task_id.clone())),
        };
        let Some(task_id) = resolved_task_id else {
            return Vec::new();
        };
        let Some(entry) = self.chat_fanout_by_task.get(&task_id) else {
            return Vec::new();
        };
        let mut out: Vec<(String, String, String)> = Vec::new();
        for target in entry.value().iter() {
            let Some(turn) = target.chat_turn_id.as_deref() else {
                continue;
            };
            if turn.is_empty() {
                continue;
            }
            let row = (
                turn.to_owned(),
                target.principal.clone(),
                target.workspace.clone(),
            );
            if !out.contains(&row) {
                out.push(row);
            }
        }
        out
    }
}

/// Default slot count for the production runtime-event `broadcast` channel.
///
/// This channel fans out every non-private transport event to `/events` SSE
/// forwarders and the on-disk transport-log writer via clones of one `Sender`.
/// Sealed app-owner lifecycle events are consumed before this ring. A `tokio`
/// broadcast advances its head to the fastest producer and drops for the
/// slowest receiver: whenever the slowest co-subscriber (a browser draining
/// its downstream mpsc, or the disk-append writer) falls further than the
/// capacity behind, `recv()` returns `RecvError::Lagged` and the SSE arm
/// paints the operator's "buffer error" banner. Sized to match the chat live
/// sink's `LIVE_BROADCAST_CAPACITY` (8192) so a burst has to be an order of
/// magnitude larger than the old 1000-slot channel before it can lag a live
/// subscriber. See `builder.rs` for the sole production construction site.
pub const DEFAULT_RUNTIME_TRANSPORT_CAPACITY: usize = 8192;
const HITL_LIFECYCLE_JOURNAL_FILENAME: &str = "pending_hitl_lifecycle.jsonl";
const HITL_LIFECYCLE_IMPORT_ARCHIVE_FILENAME: &str = "pending_hitl_lifecycle.v2-imported.jsonl";
#[cfg(test)]
const HITL_LIFECYCLE_AUTHORITY_FILENAME: &str = "pending_hitl_lifecycle.v3.sqlite3";
const HITL_LIFECYCLE_AUTHORITY_SCHEMA_VERSION: i64 = 3;
const HITL_LIFECYCLE_AUTHORITY_READY_KEY: &str = "authority_ready";
const MAX_HITL_LIFECYCLE_RECORD_BYTES: usize = 8 * 1024 * 1024;
const MAX_HITL_LIFECYCLE_RECORD_NODES: usize = 200_128;
const MAX_HITL_INPUT_SCHEMA_NODES: usize = 200_000;
const HITL_LIFECYCLE_READ_CHUNK_BYTES: usize = 64 * 1024;
const HITL_LIFECYCLE_PROOF_V1: &str = "hitl_lifecycle_proof.v1";
const HITL_LIFECYCLE_PROOF_V2: &str = "hitl_lifecycle_proof.v2";
const APP_OWNER_NOTIFICATION_EXPIRY_BATCH: usize = 512;
const HITL_LIFECYCLE_RECONCILE_BATCH: usize = 32;
const APP_OWNER_NOTIFICATION_COMPACTION_RETRY_MS: i64 = 60_000;
const HITL_LIFECYCLE_COMPACTION_CAS_FALLBACK_THRESHOLD: u8 = 3;

type HitlLifecycleKey = (String, String, String);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum HitlLifecycleProofStateV1 {
    PendingRedacted,
    Resolved,
}

/// Content-free durable lifecycle authority. Generic resolved HITL uses an
/// unbounded proof (`absolute_expires_at_ms = None`) exactly as before. App
/// owner notifications always carry their host-sealed absolute expiry; their
/// request body is never written to the lifecycle journal.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HitlLifecycleProofV1 {
    record_type: String,
    principal: String,
    workspace: String,
    correlation_id: String,
    state: HitlLifecycleProofStateV1,
    timestamp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    absolute_expires_at_ms: Option<i64>,
}

/// Privacy-safe resolved authority. V2 retains only the digest of the exact
/// canonical `HitlResolved` wire bytes, so a publication-debt owner can prove
/// idempotent replay without retaining the human response in compacted state.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HitlLifecycleProofV2 {
    record_type: String,
    principal: String,
    workspace: String,
    correlation_id: String,
    state: HitlLifecycleProofStateV1,
    timestamp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    absolute_expires_at_ms: Option<i64>,
    resolved_event_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HitlResolvedFingerprint([u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HitlResolvedAuthority {
    Verified(HitlResolvedFingerprint),
    /// Legacy V1 proofs establish only that this key is resolved. They cannot
    /// authorize clearing an exact publication debt.
    Unverified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GenericHitlResolvedState {
    timestamp: i64,
    authority: HitlResolvedAuthority,
}

struct PreparedGenericHitlRequest {
    index: usize,
    key: HitlLifecycleKey,
    request: RuntimeTransportEvent,
    timestamp: i64,
    body: Vec<u8>,
}

struct PreparedGenericHitlResolution {
    index: usize,
    key: HitlLifecycleKey,
    request: RuntimeTransportEvent,
    request_timestamp: i64,
    request_body: Vec<u8>,
    resolution: RuntimeTransportEvent,
    resolution_timestamp: i64,
    resolution_fingerprint: HitlResolvedFingerprint,
}

struct PreparedAppOwnerNotificationLifecycle {
    index: usize,
    key: HitlLifecycleKey,
    request: RuntimeTransportEvent,
    request_timestamp: i64,
    resolution: RuntimeTransportEvent,
    resolution_timestamp: i64,
    absolute_expires_at_ms: i64,
    resolution_fingerprint: HitlResolvedFingerprint,
    generation: AppOwnerNotificationPublicationGeneration,
}

struct PreparedAppOwnerNotificationRequest {
    index: usize,
    key: HitlLifecycleKey,
    request: RuntimeTransportEvent,
    authorization: AppOwnerNotificationRequestAuthorization,
    timestamp: i64,
    absolute_expires_at_ms: i64,
    generation: AppOwnerNotificationPublicationGeneration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AppOwnerNotificationRequestAuthorization {
    encoded_len: usize,
    sha256: [u8; 32],
}

/// Opaque first-owner generation persisted beside the private UserRequest
/// body and in the content-free lifecycle row. It is random authority, never
/// content-derived, so the lifecycle store gains substitution resistance
/// without learning a notification body or a dictionary-testable digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AppOwnerNotificationPublicationGeneration([u8; 16]);

impl AppOwnerNotificationPublicationGeneration {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let parsed = Uuid::parse_str(value).ok()?;
        (parsed.get_version_num() == 4 && parsed.to_string() == value)
            .then_some(Self(*parsed.as_bytes()))
    }

    fn canonical(self) -> String {
        Uuid::from_bytes(self.0).to_string()
    }
}

#[derive(Clone, Copy)]
struct AppOwnerNotificationAppendAuthority {
    generation: AppOwnerNotificationPublicationGeneration,
    absolute_expires_at_ms: i64,
}

fn app_owner_notification_request_authorization(
    encoded: &[u8],
) -> AppOwnerNotificationRequestAuthorization {
    let digest = Sha256::digest(encoded);
    let mut sha256 = [0_u8; 32];
    sha256.copy_from_slice(&digest);
    AppOwnerNotificationRequestAuthorization {
        encoded_len: encoded.len(),
        sha256,
    }
}

/// Short-lived authority handoff from fixed-page app-request reconciliation
/// to the exact live UserRequest revalidation and transport publication. All
/// fields are private so only this module can mint or inspect authority.
pub(crate) struct AppOwnerNotificationRequestPublicationTicket {
    accepted: Vec<bool>,
    authorized_requests: Vec<Option<AppOwnerNotificationRequestAuthorization>>,
    authorized_generations: Vec<Option<AppOwnerNotificationPublicationGeneration>>,
    owner_lifecycle_lock: Arc<Mutex<()>>,
    _publication_guard: Option<FileLockGuard>,
}

impl AppOwnerNotificationRequestPublicationTicket {
    fn empty(owner: &RuntimeTransportBroadcaster, len: usize) -> Self {
        Self {
            accepted: vec![false; len],
            authorized_requests: vec![None; len],
            authorized_generations: vec![None; len],
            owner_lifecycle_lock: Arc::clone(&owner.hitl_lifecycle_lock),
            _publication_guard: None,
        }
    }

    pub(crate) fn accepted(&self, index: usize) -> bool {
        self.accepted.get(index).copied().unwrap_or(false)
    }
}

#[derive(Clone, Copy, Debug)]
struct AppOwnerNotificationLifecycleState {
    state: HitlLifecycleProofStateV1,
    timestamp: i64,
    absolute_expires_at_ms: i64,
    resolved_authority: Option<HitlResolvedAuthority>,
    app_owner_generation: Option<AppOwnerNotificationPublicationGeneration>,
}

/// Owns the single app-notification expiry-sweeper slot. Constructed before
/// the future is handed to Tokio so abort-before-first-poll, runtime shutdown,
/// or panic always makes the worker restartable in this process.
struct AppOwnerNotificationExpirySweeperGuard {
    started: Arc<AtomicBool>,
    owns_slot: bool,
}

impl AppOwnerNotificationExpirySweeperGuard {
    fn new(started: Arc<AtomicBool>) -> Self {
        Self {
            started,
            owns_slot: true,
        }
    }

    fn release_for_handoff(&mut self) {
        if self.owns_slot {
            self.started.store(false, Ordering::SeqCst);
            self.owns_slot = false;
        }
    }
}

impl Drop for AppOwnerNotificationExpirySweeperGuard {
    fn drop(&mut self) {
        self.release_for_handoff();
    }
}

/// Owns the process-local orphan-sweeper slot. It is created before spawning
/// so cancellation before the future's first poll also restores restartability.
struct HitlLifecycleOrphanSweeperGuard {
    started: Arc<AtomicBool>,
}

impl Drop for HitlLifecycleOrphanSweeperGuard {
    fn drop(&mut self) {
        self.started.store(false, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug)]
struct HitlLifecycleReplaySummary {
    complete_len: u64,
    file_len: u64,
    file_sha256: String,
    requires_privacy_compaction: bool,
}

fn scoped_hitl_lifecycle_event(event: &RuntimeTransportEvent) -> bool {
    matches!(
        event,
        RuntimeTransportEvent::HitlRequested {
            principal: Some(_),
            workspace: Some(_),
            ..
        } | RuntimeTransportEvent::HitlResolved {
            principal: Some(_),
            workspace: Some(_),
            ..
        }
    )
}

fn scoped_hitl_lifecycle_key(event: &RuntimeTransportEvent) -> Option<HitlLifecycleKey> {
    match event {
        RuntimeTransportEvent::HitlRequested {
            correlation_id,
            principal: Some(principal),
            workspace: Some(workspace),
            ..
        }
        | RuntimeTransportEvent::HitlResolved {
            correlation_id,
            principal: Some(principal),
            workspace: Some(workspace),
            ..
        } => Some((principal.clone(), workspace.clone(), correlation_id.clone())),
        _ => None,
    }
}

/// Recover the app-notification expiry only from the host-created canonical
/// UserRequest schema. Seeing the marker without its sealed deadline is a
/// malformed app notification, not permission to fall back to the generic
/// non-expiring HITL lifecycle.
pub fn app_owner_notification_expiry_ms(
    event: &RuntimeTransportEvent,
) -> Result<Option<i64>, &'static str> {
    let RuntimeTransportEvent::HitlRequested {
        source,
        input_schema,
        timestamp,
        ..
    } = event
    else {
        return Ok(None);
    };
    let Some(context) = input_schema
        .as_ref()
        .and_then(|schema| schema.get("context"))
    else {
        return Ok(None);
    };
    if context
        .get("app_owner_notification")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Ok(None);
    }
    if source != "user_request" {
        return Err("app owner notification lifecycle has an invalid request source");
    }
    let expiry = context
        .get("absolute_expires_at_ms")
        .and_then(Value::as_i64)
        .ok_or("app owner notification lifecycle is missing its absolute expiry")?;
    if expiry <= *timestamp {
        return Err("app owner notification lifecycle expiry is not after creation");
    }
    Ok(Some(expiry))
}

/// True only for the host-sealed owner-notification HITL projection and its
/// dedicated resolution source. Generic HITL/UserRequest consumers use this
/// at fanout boundaries so app notification content remains exclusively on
/// the bounded UserRequest/Attention surface.
pub fn is_app_owner_notification_transport_event(event: &RuntimeTransportEvent) -> bool {
    match event {
        RuntimeTransportEvent::HitlRequested { input_schema, .. } => {
            input_schema
                .as_ref()
                .and_then(|schema| schema.get("context"))
                .and_then(|context| context.get("app_owner_notification"))
                .and_then(Value::as_bool)
                == Some(true)
        },
        RuntimeTransportEvent::HitlResolved { source, .. } => source == "app_owner_notification",
        _ => false,
    }
}

fn hitl_lifecycle_proof(
    key: &HitlLifecycleKey,
    state: HitlLifecycleProofStateV1,
    timestamp: i64,
    absolute_expires_at_ms: Option<i64>,
) -> HitlLifecycleProofV1 {
    HitlLifecycleProofV1 {
        record_type: HITL_LIFECYCLE_PROOF_V1.to_string(),
        principal: key.0.clone(),
        workspace: key.1.clone(),
        correlation_id: key.2.clone(),
        state,
        timestamp,
        absolute_expires_at_ms,
    }
}

fn hitl_resolved_fingerprint(event: &RuntimeTransportEvent) -> Option<HitlResolvedFingerprint> {
    if !matches!(event, RuntimeTransportEvent::HitlResolved { .. }) {
        return None;
    }
    let mut encoded = BoundedHitlLifecycleRecord::new();
    write_hitl_lifecycle_event(&mut encoded, event).ok()?;
    if encoded.exceeded {
        return None;
    }
    let digest = Sha256::digest(&encoded.bytes);
    let mut fingerprint = [0_u8; 32];
    fingerprint.copy_from_slice(&digest);
    Some(HitlResolvedFingerprint(fingerprint))
}

fn bounded_hitl_lifecycle_event_bytes(event: &RuntimeTransportEvent) -> Option<Vec<u8>> {
    let mut encoded = BoundedHitlLifecycleRecord::new();
    write_hitl_lifecycle_event(&mut encoded, event).ok()?;
    (!encoded.exceeded).then_some(encoded.bytes)
}

fn parse_hitl_resolved_fingerprint(value: &str) -> Option<HitlResolvedFingerprint> {
    let decoded = hex::decode(value).ok()?;
    let bytes: [u8; 32] = decoded.try_into().ok()?;
    Some(HitlResolvedFingerprint(bytes))
}

fn hitl_lifecycle_proof_v2(
    key: &HitlLifecycleKey,
    timestamp: i64,
    absolute_expires_at_ms: Option<i64>,
    fingerprint: HitlResolvedFingerprint,
) -> HitlLifecycleProofV2 {
    HitlLifecycleProofV2 {
        record_type: HITL_LIFECYCLE_PROOF_V2.to_string(),
        principal: key.0.clone(),
        workspace: key.1.clone(),
        correlation_id: key.2.clone(),
        state: HitlLifecycleProofStateV1::Resolved,
        timestamp,
        absolute_expires_at_ms,
        resolved_event_sha256: hex::encode(fingerprint.0),
    }
}

fn validate_hitl_lifecycle_proof(proof: &HitlLifecycleProofV1) -> Result<(), &'static str> {
    if proof.record_type != HITL_LIFECYCLE_PROOF_V1
        || proof.principal.is_empty()
        || proof.workspace.is_empty()
        || proof.correlation_id.is_empty()
    {
        return Err("invalid HITL lifecycle proof identity");
    }
    match (proof.state, proof.absolute_expires_at_ms) {
        (HitlLifecycleProofStateV1::PendingRedacted, None) => {
            Err("generic pending HITL cannot be represented without content")
        },
        (_, Some(expiry)) if expiry <= proof.timestamp => {
            Err("app owner notification lifecycle proof has an invalid expiry")
        },
        _ => Ok(()),
    }
}

fn validate_hitl_lifecycle_proof_v2(
    proof: &HitlLifecycleProofV2,
) -> Result<HitlResolvedFingerprint, &'static str> {
    if proof.record_type != HITL_LIFECYCLE_PROOF_V2
        || proof.principal.is_empty()
        || proof.workspace.is_empty()
        || proof.correlation_id.is_empty()
        || proof.state != HitlLifecycleProofStateV1::Resolved
    {
        return Err("invalid HITL lifecycle V2 proof identity");
    }
    if proof
        .absolute_expires_at_ms
        .is_some_and(|expiry| expiry <= proof.timestamp)
    {
        return Err("app owner notification lifecycle V2 proof has an invalid expiry");
    }
    parse_hitl_resolved_fingerprint(&proof.resolved_event_sha256)
        .ok_or("invalid HITL lifecycle V2 resolved-event fingerprint")
}

fn hitl_lifecycle_event(event: &RuntimeTransportEvent) -> bool {
    matches!(
        event,
        RuntimeTransportEvent::HitlRequested { .. } | RuntimeTransportEvent::HitlResolved { .. }
    )
}

fn hitl_lifecycle_event_is_admitted(event: &RuntimeTransportEvent) -> bool {
    match event {
        RuntimeTransportEvent::HitlRequested { input_schema, .. } => input_schema
            .as_ref()
            .map(|schema| {
                inspect_json_bounded(schema, MAX_HITL_INPUT_SCHEMA_NODES).is_some_and(|shape| {
                    // RuntimeTransportEvent's adjacent `data` envelope adds
                    // two containers above the schema on the JSON wire.
                    shape.max_depth.saturating_add(2) <= MAX_RETAINED_JSON_DEPTH
                })
            })
            .unwrap_or(true),
        RuntimeTransportEvent::HitlResolved { .. } => true,
        _ => false,
    }
}

fn discard_hitl_lifecycle_event_iteratively(mut event: RuntimeTransportEvent) {
    if let RuntimeTransportEvent::HitlRequested { input_schema, .. } = &mut event {
        if let Some(schema) = input_schema.take() {
            discard_json_iteratively(schema);
        }
    }
    drop(event);
}

fn clone_hitl_lifecycle_event_iteratively(
    event: &RuntimeTransportEvent,
) -> Option<RuntimeTransportEvent> {
    match event {
        RuntimeTransportEvent::HitlRequested {
            correlation_id,
            source,
            input_type,
            prompt,
            hint,
            input_schema,
            task_id,
            execution_id,
            agent_id,
            principal,
            workspace,
            timestamp,
        } => Some(RuntimeTransportEvent::HitlRequested {
            correlation_id: correlation_id.clone(),
            source: source.clone(),
            input_type: input_type.clone(),
            prompt: prompt.clone(),
            hint: hint.clone(),
            input_schema: input_schema.as_ref().map(clone_json_iteratively),
            task_id: task_id.clone(),
            execution_id: execution_id.clone(),
            agent_id: agent_id.clone(),
            principal: principal.clone(),
            workspace: workspace.clone(),
            timestamp: *timestamp,
        }),
        RuntimeTransportEvent::HitlResolved {
            correlation_id,
            source,
            outcome,
            decision,
            task_id,
            execution_id,
            agent_id,
            principal,
            workspace,
            timestamp,
        } => Some(RuntimeTransportEvent::HitlResolved {
            correlation_id: correlation_id.clone(),
            source: source.clone(),
            outcome: outcome.clone(),
            decision: decision.clone(),
            task_id: task_id.clone(),
            execution_id: execution_id.clone(),
            agent_id: agent_id.clone(),
            principal: principal.clone(),
            workspace: workspace.clone(),
            timestamp: *timestamp,
        }),
        _ => None,
    }
}

struct BoundedHitlLifecycleRecord {
    bytes: Vec<u8>,
    exceeded: bool,
}

#[derive(Default)]
struct BoundedHitlLifecycleSize {
    bytes: usize,
}

impl std::io::Write for BoundedHitlLifecycleSize {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|next| *next <= MAX_HITL_LIFECYCLE_RECORD_BYTES)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "HITL lifecycle record exceeds byte ceiling",
                )
            })?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn hitl_lifecycle_wire_size_is_admitted(event: &RuntimeTransportEvent) -> bool {
    write_hitl_lifecycle_event(&mut BoundedHitlLifecycleSize::default(), event).is_ok()
}

fn write_hitl_scalar<T: Serialize + ?Sized>(
    output: &mut dyn std::io::Write,
    value: &T,
) -> std::io::Result<()> {
    serde_json::to_writer(&mut *output, value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn write_hitl_field_prefix(
    output: &mut dyn std::io::Write,
    first: &mut bool,
    name: &str,
) -> std::io::Result<()> {
    if !*first {
        output.write_all(b",")?;
    }
    *first = false;
    write_hitl_scalar(output, name)?;
    output.write_all(b":")
}

fn write_hitl_field<T: Serialize + ?Sized>(
    output: &mut dyn std::io::Write,
    first: &mut bool,
    name: &str,
    value: &T,
) -> std::io::Result<()> {
    write_hitl_field_prefix(output, first, name)?;
    write_hitl_scalar(output, value)
}

fn write_optional_hitl_field<T: Serialize>(
    output: &mut dyn std::io::Write,
    first: &mut bool,
    name: &str,
    value: Option<&T>,
) -> std::io::Result<()> {
    if let Some(value) = value {
        write_hitl_field(output, first, name, value)?;
    }
    Ok(())
}

/// Serialize the two durable HITL lifecycle variants with the exact compact
/// derived-Serde wire shape while keeping the arbitrary `input_schema` Value
/// on heap-owned traversal frames. This function is also the authoritative
/// size-admission encoder, so persistence and no-persistence modes cannot
/// disagree about the accepted bytes.
fn write_hitl_lifecycle_event(
    output: &mut dyn std::io::Write,
    event: &RuntimeTransportEvent,
) -> std::io::Result<()> {
    output.write_all(b"{\"event_type\":")?;
    match event {
        RuntimeTransportEvent::HitlRequested {
            correlation_id,
            source,
            input_type,
            prompt,
            hint,
            input_schema,
            task_id,
            execution_id,
            agent_id,
            principal,
            workspace,
            timestamp,
        } => {
            write_hitl_scalar(output, "HitlRequested")?;
            output.write_all(b",\"data\":{")?;
            let mut first = true;
            write_hitl_field(output, &mut first, "correlation_id", correlation_id)?;
            write_hitl_field(output, &mut first, "source", source)?;
            write_hitl_field(output, &mut first, "input_type", input_type)?;
            write_hitl_field(output, &mut first, "prompt", prompt)?;
            write_optional_hitl_field(output, &mut first, "hint", hint.as_ref())?;
            if let Some(schema) = input_schema.as_ref() {
                write_hitl_field_prefix(output, &mut first, "input_schema")?;
                write_json(schema, output)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            }
            write_optional_hitl_field(output, &mut first, "task_id", task_id.as_ref())?;
            write_optional_hitl_field(output, &mut first, "execution_id", execution_id.as_ref())?;
            write_optional_hitl_field(output, &mut first, "agent_id", agent_id.as_ref())?;
            write_optional_hitl_field(output, &mut first, "principal", principal.as_ref())?;
            write_optional_hitl_field(output, &mut first, "workspace", workspace.as_ref())?;
            write_hitl_field(output, &mut first, "timestamp", timestamp)?;
        },
        RuntimeTransportEvent::HitlResolved {
            correlation_id,
            source,
            outcome,
            decision,
            task_id,
            execution_id,
            agent_id,
            principal,
            workspace,
            timestamp,
        } => {
            write_hitl_scalar(output, "HitlResolved")?;
            output.write_all(b",\"data\":{")?;
            let mut first = true;
            write_hitl_field(output, &mut first, "correlation_id", correlation_id)?;
            write_hitl_field(output, &mut first, "source", source)?;
            write_hitl_field(output, &mut first, "outcome", outcome)?;
            write_optional_hitl_field(output, &mut first, "decision", decision.as_ref())?;
            write_optional_hitl_field(output, &mut first, "task_id", task_id.as_ref())?;
            write_optional_hitl_field(output, &mut first, "execution_id", execution_id.as_ref())?;
            write_optional_hitl_field(output, &mut first, "agent_id", agent_id.as_ref())?;
            write_optional_hitl_field(output, &mut first, "principal", principal.as_ref())?;
            write_optional_hitl_field(output, &mut first, "workspace", workspace.as_ref())?;
            write_hitl_field(output, &mut first, "timestamp", timestamp)?;
        },
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "event is not a HITL lifecycle transition",
            ));
        },
    }
    output.write_all(b"}}")
}

fn write_hitl_lifecycle_proof(
    output: &mut dyn std::io::Write,
    proof: &HitlLifecycleProofV1,
) -> std::io::Result<()> {
    validate_hitl_lifecycle_proof(proof)
        .map_err(|detail| std::io::Error::new(std::io::ErrorKind::InvalidData, detail))?;
    serde_json::to_writer(output, proof)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn write_hitl_lifecycle_proof_v2(
    output: &mut dyn std::io::Write,
    proof: &HitlLifecycleProofV2,
) -> std::io::Result<()> {
    validate_hitl_lifecycle_proof_v2(proof)
        .map_err(|detail| std::io::Error::new(std::io::ErrorKind::InvalidData, detail))?;
    serde_json::to_writer(output, proof)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

impl BoundedHitlLifecycleRecord {
    fn new() -> Self {
        Self {
            bytes: Vec::with_capacity(MAX_HITL_LIFECYCLE_RECORD_BYTES.min(64 * 1024)),
            exceeded: false,
        }
    }
}

impl std::io::Write for BoundedHitlLifecycleRecord {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|next| next > MAX_HITL_LIFECYCLE_RECORD_BYTES)
        {
            self.exceeded = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "HITL lifecycle record exceeds byte ceiling",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Cancellation/panic-safe cleanup for lifecycle compaction staging. The file
/// is unique and remains inside the workspace provider root; provider-owned
/// publication either consumes a second verified staging copy or leaves this
/// guard to remove the caller-owned source.
struct HitlLifecycleCompactionStaging {
    path: std::path::PathBuf,
    work_item: HitlLifecycleWorkItem,
}

const HITL_REDUCER_GENERIC_PENDING: i64 = 1;
const HITL_REDUCER_GENERIC_RESOLVED: i64 = 2;
const HITL_REDUCER_APP_PENDING: i64 = 3;
const HITL_REDUCER_APP_RESOLVED: i64 = 4;
const HITL_LIFECYCLE_WORK_ROOT_FILENAME: &str = ".pending-hitl-lifecycle-work";
const HITL_LIFECYCLE_ORPHAN_SWEEP_SLICE: usize = 128;
const HITL_LIFECYCLE_ORPHAN_SWEEP_INTERVAL_SECS: u64 = 60 * 60;
const HITL_COMPACTION_ORPHAN_MIN_AGE_SECS: u64 = 24 * 60 * 60;

#[derive(Debug)]
struct HitlLifecycleReducedRecord {
    state: i64,
    timestamp: i64,
    absolute_expires_at_ms: Option<i64>,
    body: Option<Vec<u8>>,
    app_owner_generation: Option<String>,
}

fn hitl_compaction_orphan_metadata_is_safe(
    metadata: &std::fs::Metadata,
    expect_directory: bool,
) -> bool {
    if metadata.is_dir() != expect_directory || metadata.is_file() == expect_directory {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return false;
        }
        if !expect_directory && metadata.nlink() != 1 {
            return false;
        }
    }
    true
}

fn hitl_lifecycle_work_root(journal_path: &std::path::Path) -> std::path::PathBuf {
    journal_path.with_file_name(HITL_LIFECYCLE_WORK_ROOT_FILENAME)
}

fn hitl_lifecycle_private_directory_metadata_is_safe(metadata: &std::fs::Metadata) -> bool {
    if !metadata.is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return false;
        }
    }
    true
}

fn ensure_hitl_lifecycle_work_root(
    journal_path: &std::path::Path,
) -> Result<std::path::PathBuf, ArtifactV2Error> {
    let work_root = hitl_lifecycle_work_root(journal_path);
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(&work_root) {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(&work_root)?;
    if !hitl_lifecycle_private_directory_metadata_is_safe(&metadata) {
        return Err(ArtifactV2Error::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "HITL lifecycle work root failed owner/mode/type validation",
        )));
    }
    Ok(work_root)
}

#[derive(Clone, Copy)]
enum HitlLifecycleWorkKind {
    Reducer,
    Compaction,
}

impl HitlLifecycleWorkKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Reducer => "reducer-",
            Self::Compaction => "compaction-",
        }
    }
}

fn hitl_lifecycle_work_kind(name: &str) -> Option<HitlLifecycleWorkKind> {
    let (kind, identifier) = if let Some(identifier) = name.strip_prefix("reducer-") {
        (HitlLifecycleWorkKind::Reducer, identifier)
    } else if let Some(identifier) = name.strip_prefix("compaction-") {
        (HitlLifecycleWorkKind::Compaction, identifier)
    } else {
        return None;
    };
    (identifier.len() == 32 && identifier.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(kind)
}

fn remove_hitl_lifecycle_work_lease(operation_path: &std::path::Path) {
    let lease_path = AgentStorage::file_lock_path(operation_path);
    match std::fs::symlink_metadata(&lease_path) {
        Ok(metadata) if hitl_compaction_orphan_metadata_is_safe(&metadata, false) => {
            if let Err(error) = std::fs::remove_file(&lease_path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    warn!(
                        path = %lease_path.display(),
                        error = %error,
                        "[MAGICIAN-RUNTIME-EVENTS] Failed to remove lifecycle work lease"
                    );
                }
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Ok(_) | Err(_) => warn!(
            path = %lease_path.display(),
            "[MAGICIAN-RUNTIME-EVENTS] Refusing to remove unsafe lifecycle work lease"
        ),
    }
}

struct HitlLifecycleWorkItem {
    path: std::path::PathBuf,
    lease_guard: Option<FileLockGuard>,
}

impl HitlLifecycleWorkItem {
    fn new(
        journal_path: &std::path::Path,
        kind: HitlLifecycleWorkKind,
    ) -> Result<Self, ArtifactV2Error> {
        let root = ensure_hitl_lifecycle_work_root(journal_path)?;
        let path = root.join(format!("{}{}", kind.prefix(), Uuid::new_v4().simple()));
        // The stable sibling sentinel is published and locked before the work
        // directory appears. A concurrent sweeper therefore sees either a
        // live lease or a dead, exclusively reclaimable item, never an
        // unprotected active directory.
        let lease_guard =
            AgentStorage::acquire_file_lock_exclusive_sync(&path).map_err(|error| {
                ArtifactV2Error::Runtime(format!(
                    "failed to acquire HITL lifecycle work lease: {error}"
                ))
            })?;
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        if let Err(error) = builder.create(&path) {
            remove_hitl_lifecycle_work_lease(&path);
            drop(lease_guard);
            return Err(error.into());
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                let _ = std::fs::remove_dir(&path);
                remove_hitl_lifecycle_work_lease(&path);
                drop(lease_guard);
                return Err(error.into());
            },
        };
        if !hitl_lifecycle_private_directory_metadata_is_safe(&metadata) {
            let _ = std::fs::remove_dir(&path);
            remove_hitl_lifecycle_work_lease(&path);
            drop(lease_guard);
            return Err(ArtifactV2Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "HITL lifecycle work item failed owner/mode/type validation",
            )));
        }
        Ok(Self {
            path,
            lease_guard: Some(lease_guard),
        })
    }

    fn release_lease(&mut self) {
        if self.lease_guard.is_some() {
            let work_item_gone = matches!(
                std::fs::symlink_metadata(&self.path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            );
            if work_item_gone {
                // Unlink the unique sentinel while its descriptor is still
                // locked; the UUID operation name is never reused.
                remove_hitl_lifecycle_work_lease(&self.path);
            } else {
                // A transient cleanup failure left work behind. Preserve the
                // stable sentinel and release only its advisory owner so a
                // later sliced sweeper can acquire and retry. Removing the
                // path here would make the non-creating cleanup probe skip
                // this UUID directory forever.
                warn!(
                    path = %self.path.display(),
                    "[MAGICIAN-RUNTIME-EVENTS] Preserving lifecycle work lease for background cleanup retry"
                );
            }
            drop(self.lease_guard.take());
        }
    }
}

impl Drop for HitlLifecycleWorkItem {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound
                && error.kind() != std::io::ErrorKind::DirectoryNotEmpty
            {
                warn!(
                    path = %self.path.display(),
                    error = %error,
                    "[MAGICIAN-RUNTIME-EVENTS] Failed to remove lifecycle work directory"
                );
            }
        }
        self.release_lease();
    }
}

fn cleanup_known_hitl_lifecycle_work_item(
    path: &std::path::Path,
    kind: HitlLifecycleWorkKind,
) -> bool {
    let filenames: &[&str] = match kind {
        HitlLifecycleWorkKind::Reducer => &[
            "reducer.sqlite3",
            "reducer.sqlite3-journal",
            "reducer.sqlite3-wal",
            "reducer.sqlite3-shm",
        ],
        HitlLifecycleWorkKind::Compaction => &["staging.tmp"],
    };
    for filename in filenames {
        let child = path.join(filename);
        match std::fs::symlink_metadata(&child) {
            Ok(metadata) if hitl_compaction_orphan_metadata_is_safe(&metadata, false) => {
                if let Err(error) = std::fs::remove_file(&child) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        warn!(
                            path = %child.display(),
                            error = %error,
                            "[MAGICIAN-RUNTIME-EVENTS] Failed to remove lifecycle orphan file"
                        );
                        return false;
                    }
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Ok(_) | Err(_) => return false,
        }
    }
    match std::fs::remove_dir(path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::DirectoryNotEmpty {
                warn!(
                    path = %path.display(),
                    error = %error,
                    "[MAGICIAN-RUNTIME-EVENTS] Failed to remove lifecycle orphan directory"
                );
            }
            false
        },
    }
}

fn cleanup_hitl_lifecycle_work_item(path: &std::path::Path, kind: HitlLifecycleWorkKind) {
    let lease_guard = match AgentStorage::try_acquire_file_lock_exclusive_sync(path) {
        Ok(Some(guard)) => guard,
        Ok(None) => return,
        Err(error) => {
            warn!(
                path = %path.display(),
                error = %error,
                "[MAGICIAN-RUNTIME-EVENTS] Failed to inspect lifecycle orphan lease"
            );
            return;
        },
    };
    let safe_directory = match std::fs::symlink_metadata(path) {
        Ok(metadata) => hitl_lifecycle_private_directory_metadata_is_safe(&metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => false,
    };
    if safe_directory && cleanup_known_hitl_lifecycle_work_item(path, kind) {
        remove_hitl_lifecycle_work_lease(path);
    }
    drop(lease_guard);
}

fn cleanup_hitl_lifecycle_work_entry(path: &std::path::Path) {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    if let Some(kind) = hitl_lifecycle_work_kind(name) {
        cleanup_hitl_lifecycle_work_item(path, kind);
        return;
    }
    let Some(operation_name) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_suffix(".flock"))
    else {
        return;
    };
    let Some(kind) = hitl_lifecycle_work_kind(operation_name) else {
        return;
    };
    let operation_path = path.with_file_name(operation_name);
    if std::fs::symlink_metadata(&operation_path).is_ok() {
        cleanup_hitl_lifecycle_work_item(&operation_path, kind);
        return;
    }
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    let old_enough = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age.as_secs() >= HITL_COMPACTION_ORPHAN_MIN_AGE_SECS);
    if !old_enough || !hitl_compaction_orphan_metadata_is_safe(&metadata, false) {
        return;
    }
    let guard = match AgentStorage::try_acquire_file_lock_exclusive_sync(&operation_path) {
        Ok(Some(guard)) => guard,
        Ok(None) | Err(_) => return,
    };
    if matches!(
        std::fs::symlink_metadata(&operation_path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    ) {
        remove_hitl_lifecycle_work_lease(&operation_path);
    }
    drop(guard);
}

fn cleanup_legacy_hitl_compaction_orphan(path: &std::path::Path) {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let staging = name.starts_with(".pending-hitl-lifecycle-compaction-") && name.ends_with(".tmp");
    let reducer = name.starts_with(".pending-hitl-lifecycle-reducer-");
    if !staging && !reducer {
        return;
    }
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    let old_enough = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age.as_secs() >= HITL_COMPACTION_ORPHAN_MIN_AGE_SECS);
    if !old_enough || !hitl_compaction_orphan_metadata_is_safe(&metadata, reducer) {
        return;
    }
    if staging {
        if let Err(error) = std::fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    path = %path.display(),
                    error = %error,
                    "[MAGICIAN-RUNTIME-EVENTS] Failed to remove legacy lifecycle staging orphan"
                );
            }
        }
    } else {
        let _ = cleanup_known_hitl_lifecycle_work_item(path, HitlLifecycleWorkKind::Reducer);
    }
}

async fn sweep_hitl_lifecycle_orphan_directory(
    directory: std::path::PathBuf,
    cleanup: fn(&std::path::Path),
) {
    let mut entries = match tokio::fs::read_dir(&directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            warn!(
                path = %directory.display(),
                error = %error,
                "[MAGICIAN-RUNTIME-EVENTS] Failed to open lifecycle orphan sweep directory"
            );
            return;
        },
    };
    loop {
        let mut paths = Vec::with_capacity(HITL_LIFECYCLE_ORPHAN_SWEEP_SLICE);
        let mut reached_end = false;
        while paths.len() < HITL_LIFECYCLE_ORPHAN_SWEEP_SLICE {
            match entries.next_entry().await {
                Ok(Some(entry)) => paths.push(entry.path()),
                Ok(None) => {
                    reached_end = true;
                    break;
                },
                Err(error) => {
                    warn!(
                        path = %directory.display(),
                        error = %error,
                        "[MAGICIAN-RUNTIME-EVENTS] Failed while reading lifecycle orphan sweep directory"
                    );
                    return;
                },
            }
        }
        if paths.is_empty() {
            return;
        }
        if let Err(error) = magician_core::blocking_admission::spawn_blocking_admitted(move || {
            for path in paths {
                cleanup(&path);
            }
        })
        .await
        {
            warn!(
                path = %directory.display(),
                error = %error,
                "[MAGICIAN-RUNTIME-EVENTS] Lifecycle orphan cleanup task failed"
            );
            return;
        }
        if reached_end {
            return;
        }
        tokio::task::yield_now().await;
    }
}

/// One cycle walks each directory to EOF but admits only a fixed page of
/// filesystem work at a time. The cursor remains in the background task across
/// yields, so more than one page cannot be pinned forever behind an arbitrary
/// `read_dir` prefix as it could under the former startup-only scan.
async fn sweep_hitl_lifecycle_orphans_once(journal_path: &std::path::Path) {
    let work_root = hitl_lifecycle_work_root(journal_path);
    match tokio::fs::symlink_metadata(&work_root).await {
        Ok(metadata) if hitl_lifecycle_private_directory_metadata_is_safe(&metadata) => {
            sweep_hitl_lifecycle_orphan_directory(work_root, cleanup_hitl_lifecycle_work_entry)
                .await;
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Ok(_) | Err(_) => warn!(
            path = %work_root.display(),
            "[MAGICIAN-RUNTIME-EVENTS] Refusing to sweep unsafe lifecycle work root"
        ),
    }
    if let Some(parent) = journal_path.parent() {
        sweep_hitl_lifecycle_orphan_directory(
            parent.to_path_buf(),
            cleanup_legacy_hitl_compaction_orphan,
        )
        .await;
    }
}

struct HitlLifecycleReducerDirectory {
    work_item: HitlLifecycleWorkItem,
}

impl Drop for HitlLifecycleReducerDirectory {
    fn drop(&mut self) {
        for filename in [
            "reducer.sqlite3",
            "reducer.sqlite3-journal",
            "reducer.sqlite3-wal",
            "reducer.sqlite3-shm",
        ] {
            let path = self.work_item.path.join(filename);
            if let Err(cleanup_error) = std::fs::remove_file(&path) {
                if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                    warn!(
                        path = %path.display(),
                        error = %cleanup_error,
                        "[MAGICIAN-RUNTIME-EVENTS] Failed to remove lifecycle reducer file"
                    );
                }
            }
        }
        if let Err(cleanup_error) = std::fs::remove_dir(&self.work_item.path) {
            if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    path = %self.work_item.path.display(),
                    error = %cleanup_error,
                    "[MAGICIAN-RUNTIME-EVENTS] Failed to remove lifecycle reducer directory"
                );
            }
        }
        self.work_item.release_lease();
    }
}

struct HitlLifecycleDiskReducer {
    connection: Option<Connection>,
    _cleanup: Option<HitlLifecycleReducerDirectory>,
    durable_authority: bool,
    authority_path: Option<std::path::PathBuf>,
}

impl Drop for HitlLifecycleDiskReducer {
    fn drop(&mut self) {
        // SQLite must release every sidecar before the directory cleanup guard
        // runs, including on serialization/CAS failure or panic unwinding.
        drop(self.connection.take());
    }
}

fn hitl_reducer_error(context: &str, error: rusqlite::Error) -> ArtifactV2Error {
    ArtifactV2Error::Runtime(format!("{context}: {error}"))
}

fn hitl_lifecycle_append_error_is_admission_rejection(error: &ArtifactV2Error) -> bool {
    let ArtifactV2Error::InvalidRequest(detail) = error else {
        return false;
    };
    matches!(
        detail.as_str(),
        "lifecycle authority rejected a non-exact state transition"
            | "app lifecycle authority rejected a non-exact generation transition"
            | "generic resolution cannot consume app lifecycle authority"
            | "app resolution cannot consume generic lifecycle authority"
            | "app resolution requires existing app lifecycle authority"
            | "tokenless app lifecycle mutation is forbidden after V3 cutover"
            | "expired app lifecycle mutation is forbidden"
            | "app lifecycle mutation is missing its scope"
            | "app lifecycle append authority has a mismatched expiry"
            | "app resolution requires live pending authority"
            | "app resolution has no canonical fingerprint"
            | "generic pending HITL exceeds lifecycle reducer admission"
            | "resolved HITL lifecycle has no canonical fingerprint"
    )
}

fn validate_hitl_lifecycle_authority_files(path: &std::path::Path) -> Result<(), ArtifactV2Error> {
    let sidecar = |suffix: &str| {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        std::path::PathBuf::from(value)
    };
    for candidate in [
        path.to_path_buf(),
        sidecar("-journal"),
        sidecar("-wal"),
        sidecar("-shm"),
    ] {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = match options.open(&candidate) {
            Ok(file) => file,
            Err(error) if candidate != path && error.kind() == std::io::ErrorKind::NotFound => {
                continue;
            },
            Err(error) => return Err(error.into()),
        };
        ensure_private_workspace_file_path_identity(
            &file,
            &candidate,
            "lifecycle authority SQLite file",
        )?;
    }
    Ok(())
}

impl HitlLifecycleDiskReducer {
    fn new(journal_path: &std::path::Path) -> Result<Self, ArtifactV2Error> {
        let work_item = HitlLifecycleWorkItem::new(journal_path, HitlLifecycleWorkKind::Reducer)?;
        let directory = work_item.path.clone();
        let cleanup = HitlLifecycleReducerDirectory { work_item };
        let db_path = directory.join("reducer.sqlite3");
        let connection = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|error| hitl_reducer_error("failed to open lifecycle disk reducer", error))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = OFF;
                 PRAGMA synchronous = OFF;
                 PRAGMA temp_store = FILE;
                 PRAGMA cache_size = -2048;
                 CREATE TABLE lifecycle (
                     principal TEXT NOT NULL,
                     workspace TEXT NOT NULL,
                     correlation_id TEXT NOT NULL,
                     state INTEGER NOT NULL,
                     timestamp INTEGER NOT NULL,
                     absolute_expires_at_ms INTEGER,
                     body BLOB,
                     app_owner_generation TEXT,
                     PRIMARY KEY (principal, workspace, correlation_id)
                 ) WITHOUT ROWID;",
            )
            .map_err(|error| {
                hitl_reducer_error("failed to initialize lifecycle disk reducer", error)
            })?;
        Ok(Self {
            connection: Some(connection),
            _cleanup: Some(cleanup),
            durable_authority: false,
            authority_path: None,
        })
    }

    fn open_authority(path: &std::path::Path) -> Result<Self, ArtifactV2Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let existed = match std::fs::symlink_metadata(path) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if !existed {
            let mut options = std::fs::OpenOptions::new();
            options.create_new(true).read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            let file = options.open(path)?;
            ensure_private_workspace_file_path_identity(&file, path, "new lifecycle authority")?;
            file.sync_all()?;
            if let Some(parent) = path.parent() {
                std::fs::File::open(parent)?.sync_all()?;
            }
        }
        // A newly-created main database may coexist with attacker-planted
        // SQLite sidecar names. Validate every path before SQLite has a chance
        // to open or replace a journal/WAL/SHM entry.
        validate_hitl_lifecycle_authority_files(path)?;
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|error| hitl_reducer_error("failed to open lifecycle authority", error))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = DELETE;
                 PRAGMA synchronous = FULL;
                 PRAGMA temp_store = FILE;
                 PRAGMA cache_size = -2048;
                 PRAGMA secure_delete = ON;
                 PRAGMA auto_vacuum = INCREMENTAL;
                 CREATE TABLE IF NOT EXISTS lifecycle_authority_meta (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 ) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS lifecycle (
                     principal TEXT NOT NULL,
                     workspace TEXT NOT NULL,
                     correlation_id TEXT NOT NULL,
                     state INTEGER NOT NULL,
                     timestamp INTEGER NOT NULL,
                     absolute_expires_at_ms INTEGER,
                     body BLOB,
                     app_owner_generation TEXT,
                     PRIMARY KEY (principal, workspace, correlation_id)
                 ) WITHOUT ROWID;
                 CREATE INDEX IF NOT EXISTS lifecycle_app_expiry
                     ON lifecycle(absolute_expires_at_ms)
                     WHERE absolute_expires_at_ms IS NOT NULL;",
            )
            .map_err(|error| {
                hitl_reducer_error("failed to initialize lifecycle authority", error)
            })?;
        let version = connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .map_err(|error| {
                hitl_reducer_error("failed to read lifecycle authority schema", error)
            })?;
        if version == 0 {
            connection
                .pragma_update(
                    None,
                    "user_version",
                    HITL_LIFECYCLE_AUTHORITY_SCHEMA_VERSION,
                )
                .map_err(|error| {
                    hitl_reducer_error("failed to stamp lifecycle authority schema", error)
                })?;
        } else if version != HITL_LIFECYCLE_AUTHORITY_SCHEMA_VERSION {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "unsupported lifecycle authority schema {version}"
            )));
        }
        validate_hitl_lifecycle_authority_files(path)?;
        Ok(Self {
            connection: Some(connection),
            _cleanup: None,
            durable_authority: true,
            authority_path: Some(path.to_path_buf()),
        })
    }

    fn validate_durable_files(&self) -> Result<(), ArtifactV2Error> {
        if self.connection()?.is_autocommit() {
            if let Some(path) = self.authority_path.as_deref() {
                validate_hitl_lifecycle_authority_files(path)?;
            }
        }
        Ok(())
    }

    fn authority_is_ready(&self) -> Result<bool, ArtifactV2Error> {
        if !self.durable_authority {
            return Ok(false);
        }
        self.connection()?
            .query_row(
                "SELECT value FROM lifecycle_authority_meta WHERE key = ?1",
                [HITL_LIFECYCLE_AUTHORITY_READY_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map(|value| value.as_deref() == Some("v3"))
            .map_err(|error| {
                hitl_reducer_error("failed to read lifecycle authority readiness", error)
            })
    }

    fn reset_authority_for_import(&self) -> Result<(), ArtifactV2Error> {
        if !self.durable_authority {
            return Err(ArtifactV2Error::InvalidRequest(
                "disposable lifecycle reducer cannot become authority".to_string(),
            ));
        }
        self.connection()?
            .execute_batch(
                "BEGIN IMMEDIATE;
                 DELETE FROM lifecycle;
                 DELETE FROM lifecycle_authority_meta;
                 COMMIT;",
            )
            .map_err(|error| {
                hitl_reducer_error("failed to reset lifecycle authority import", error)
            })?;
        self.validate_durable_files()
    }

    fn mark_authority_ready(&self) -> Result<(), ArtifactV2Error> {
        if !self.durable_authority {
            return Err(ArtifactV2Error::InvalidRequest(
                "disposable lifecycle reducer cannot become authority".to_string(),
            ));
        }
        self.connection()?
            .execute(
                "INSERT INTO lifecycle_authority_meta(key, value) VALUES (?1, 'v3')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [HITL_LIFECYCLE_AUTHORITY_READY_KEY],
            )
            .map_err(|error| hitl_reducer_error("failed to publish lifecycle authority", error))?;
        self.validate_durable_files()
    }

    fn connection(&self) -> Result<&Connection, ArtifactV2Error> {
        self.connection
            .as_ref()
            .ok_or_else(|| ArtifactV2Error::Runtime("lifecycle disk reducer is closed".to_string()))
    }

    fn begin(&self) -> Result<(), ArtifactV2Error> {
        self.connection()?
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|error| hitl_reducer_error("failed to begin lifecycle reduction", error))
    }

    fn finish(&self) -> Result<(), ArtifactV2Error> {
        self.connection()?
            .execute_batch("COMMIT")
            .map_err(|error| hitl_reducer_error("failed to commit lifecycle reduction", error))?;
        self.validate_durable_files()
    }

    fn upsert(
        &self,
        key: &HitlLifecycleKey,
        state: i64,
        timestamp: i64,
        absolute_expires_at_ms: Option<i64>,
        body: Option<&[u8]>,
    ) -> Result<(), ArtifactV2Error> {
        let changed = self.connection()?
            .execute(
                "INSERT INTO lifecycle (
                     principal, workspace, correlation_id, state, timestamp,
                     absolute_expires_at_ms, body
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(principal, workspace, correlation_id) DO UPDATE SET
                     state = excluded.state,
                     timestamp = excluded.timestamp,
                     absolute_expires_at_ms = excluded.absolute_expires_at_ms,
                     body = excluded.body
                 WHERE
                     (excluded.state = 1
                      AND lifecycle.state = 1
                      AND lifecycle.timestamp = excluded.timestamp
                      AND lifecycle.absolute_expires_at_ms IS NULL
                      AND lifecycle.body IS excluded.body)
                  OR (excluded.state = 1
                      AND lifecycle.state = 2
                      AND lifecycle.absolute_expires_at_ms IS NULL
                      AND lifecycle.timestamp < excluded.timestamp)
                  OR (excluded.state = 2
                      AND ((lifecycle.state = 1
                            AND lifecycle.absolute_expires_at_ms IS NULL
                            AND lifecycle.timestamp <= excluded.timestamp)
                           OR (lifecycle.state = 2
                               AND lifecycle.timestamp = excluded.timestamp
                               AND lifecycle.absolute_expires_at_ms IS NULL
                               AND lifecycle.body IS excluded.body)))
                  OR (excluded.state = 3
                      AND lifecycle.state = 3
                      AND lifecycle.timestamp = excluded.timestamp
                      AND lifecycle.absolute_expires_at_ms IS excluded.absolute_expires_at_ms
                      AND lifecycle.body IS NULL
                      AND excluded.body IS NULL)
                  OR (excluded.state = 4
                      AND ((lifecycle.state = 3
                            AND lifecycle.absolute_expires_at_ms IS excluded.absolute_expires_at_ms
                            AND lifecycle.timestamp <= excluded.timestamp
                            AND excluded.timestamp < lifecycle.absolute_expires_at_ms)
                           OR (lifecycle.state = 4
                               AND lifecycle.timestamp = excluded.timestamp
                               AND lifecycle.absolute_expires_at_ms IS excluded.absolute_expires_at_ms
                               AND lifecycle.body IS excluded.body)))",
                params![
                    &key.0,
                    &key.1,
                    &key.2,
                    state,
                    timestamp,
                    absolute_expires_at_ms,
                    body,
                ],
            )
            .map_err(|error| hitl_reducer_error("failed to update lifecycle disk reducer", error))?;
        if changed != 1 {
            return Err(ArtifactV2Error::InvalidRequest(
                "lifecycle authority rejected a non-exact state transition".to_string(),
            ));
        }
        self.validate_durable_files()
    }

    fn upsert_app_with_generation(
        &self,
        key: &HitlLifecycleKey,
        state: i64,
        timestamp: i64,
        absolute_expires_at_ms: i64,
        body: Option<&[u8]>,
        generation: &str,
    ) -> Result<(), ArtifactV2Error> {
        let changed = self.connection()?
            .execute(
                "INSERT INTO lifecycle (
                     principal, workspace, correlation_id, state, timestamp,
                     absolute_expires_at_ms, body, app_owner_generation
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(principal, workspace, correlation_id) DO UPDATE SET
                     state = excluded.state,
                     timestamp = excluded.timestamp,
                     absolute_expires_at_ms = excluded.absolute_expires_at_ms,
                     body = excluded.body,
                     app_owner_generation = excluded.app_owner_generation
                 WHERE
                     (excluded.state = 3
                      AND lifecycle.state = 3
                      AND lifecycle.timestamp = excluded.timestamp
                      AND lifecycle.absolute_expires_at_ms IS excluded.absolute_expires_at_ms
                      AND lifecycle.body IS NULL
                      AND excluded.body IS NULL
                      AND (lifecycle.app_owner_generation IS NULL
                           OR lifecycle.app_owner_generation = excluded.app_owner_generation))
                  OR (excluded.state = 4
                      AND ((lifecycle.state = 3
                            AND lifecycle.absolute_expires_at_ms IS excluded.absolute_expires_at_ms
                            AND lifecycle.timestamp <= excluded.timestamp
                            AND excluded.timestamp < lifecycle.absolute_expires_at_ms
                            AND (lifecycle.app_owner_generation IS NULL
                                 OR lifecycle.app_owner_generation = excluded.app_owner_generation))
                           OR (lifecycle.state = 4
                               AND lifecycle.timestamp = excluded.timestamp
                               AND lifecycle.absolute_expires_at_ms IS excluded.absolute_expires_at_ms
                               AND lifecycle.body IS excluded.body
                               AND lifecycle.app_owner_generation = excluded.app_owner_generation)))",
                params![
                    &key.0,
                    &key.1,
                    &key.2,
                    state,
                    timestamp,
                    absolute_expires_at_ms,
                    body,
                    generation,
                ],
            )
            .map_err(|error| hitl_reducer_error("failed to update app lifecycle authority", error))?;
        if changed != 1 {
            return Err(ArtifactV2Error::InvalidRequest(
                "app lifecycle authority rejected a non-exact generation transition".to_string(),
            ));
        }
        self.validate_durable_files()
    }

    fn delete_expired_app_page(&self, now_ms: i64, limit: usize) -> Result<usize, ArtifactV2Error> {
        let deleted = self
            .connection()?
            .execute(
                "DELETE FROM lifecycle
                 WHERE (principal, workspace, correlation_id) IN (
                     SELECT principal, workspace, correlation_id
                     FROM lifecycle
                     WHERE absolute_expires_at_ms IS NOT NULL
                       AND absolute_expires_at_ms <= ?1
                     ORDER BY absolute_expires_at_ms, principal, workspace, correlation_id
                     LIMIT ?2
                 )",
                params![now_ms, i64::try_from(limit).unwrap_or(i64::MAX)],
            )
            .map_err(|error| {
                hitl_reducer_error("failed to expire app lifecycle authority", error)
            })?;
        Ok(deleted)
    }

    fn minimum_app_expiry(&self) -> Result<Option<i64>, ArtifactV2Error> {
        self.connection()?
            .query_row(
                "SELECT MIN(absolute_expires_at_ms)
                 FROM lifecycle
                 WHERE absolute_expires_at_ms IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .map_err(|error| hitl_reducer_error("failed to read next app lifecycle expiry", error))
    }

    fn incremental_vacuum(&self, pages: usize) -> Result<(), ArtifactV2Error> {
        self.connection()?
            .execute_batch(&format!(
                "PRAGMA incremental_vacuum({});",
                pages.min(APP_OWNER_NOTIFICATION_EXPIRY_BATCH)
            ))
            .map_err(|error| {
                hitl_reducer_error("failed bounded lifecycle authority vacuum", error)
            })?;
        self.validate_durable_files()
    }

    fn existing_state(
        &self,
        key: &HitlLifecycleKey,
    ) -> Result<Option<(i64, Option<i64>)>, ArtifactV2Error> {
        self.connection()?
            .query_row(
                "SELECT state, absolute_expires_at_ms
                 FROM lifecycle
                 WHERE principal = ?1 AND workspace = ?2 AND correlation_id = ?3",
                params![&key.0, &key.1, &key.2],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| hitl_reducer_error("failed to read lifecycle reducer row", error))
    }

    fn existing_record(
        &self,
        key: &HitlLifecycleKey,
    ) -> Result<Option<HitlLifecycleReducedRecord>, ArtifactV2Error> {
        self.connection()?
            .query_row(
                "SELECT state, timestamp, absolute_expires_at_ms, body,
                        app_owner_generation
                 FROM lifecycle
                 WHERE principal = ?1 AND workspace = ?2 AND correlation_id = ?3",
                params![&key.0, &key.1, &key.2],
                |row| {
                    Ok(HitlLifecycleReducedRecord {
                        state: row.get(0)?,
                        timestamp: row.get(1)?,
                        absolute_expires_at_ms: row.get(2)?,
                        body: row.get(3)?,
                        app_owner_generation: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|error| {
                hitl_reducer_error("failed to read exact lifecycle reducer row", error)
            })
    }

    fn ingest_event(&self, event: &RuntimeTransportEvent) -> Result<bool, ArtifactV2Error> {
        let Some(key) = scoped_hitl_lifecycle_key(event) else {
            return Ok(is_app_owner_notification_transport_event(event));
        };
        match event {
            RuntimeTransportEvent::HitlRequested { timestamp, .. } => {
                match app_owner_notification_expiry_ms(event) {
                    Ok(Some(absolute_expires_at_ms)) => {
                        self.upsert(
                            &key,
                            HITL_REDUCER_APP_PENDING,
                            *timestamp,
                            Some(absolute_expires_at_ms),
                            None,
                        )?;
                        Ok(true)
                    },
                    Ok(None) => {
                        let mut body = BoundedHitlLifecycleRecord::new();
                        write_hitl_lifecycle_event(&mut body, event)?;
                        if body.exceeded {
                            return Err(ArtifactV2Error::InvalidRequest(
                                "generic pending HITL exceeds lifecycle reducer admission"
                                    .to_string(),
                            ));
                        }
                        self.upsert(
                            &key,
                            HITL_REDUCER_GENERIC_PENDING,
                            *timestamp,
                            None,
                            Some(&body.bytes),
                        )?;
                        Ok(false)
                    },
                    Err(_) => Ok(true),
                }
            },
            RuntimeTransportEvent::HitlResolved {
                source, timestamp, ..
            } => {
                let fingerprint = hitl_resolved_fingerprint(event).ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest(
                        "resolved HITL lifecycle has no canonical fingerprint".to_string(),
                    )
                })?;
                match self.existing_state(&key)? {
                    Some((state, Some(expiry)))
                        if matches!(
                            state,
                            HITL_REDUCER_APP_PENDING | HITL_REDUCER_APP_RESOLVED
                        ) && source == "app_owner_notification" =>
                    {
                        self.upsert(
                            &key,
                            HITL_REDUCER_APP_RESOLVED,
                            *timestamp,
                            Some(expiry),
                            Some(&fingerprint.0),
                        )?;
                        Ok(true)
                    },
                    Some((state, Some(_)))
                        if matches!(
                            state,
                            HITL_REDUCER_APP_PENDING | HITL_REDUCER_APP_RESOLVED
                        ) =>
                    {
                        Err(ArtifactV2Error::InvalidRequest(
                            "generic resolution cannot consume app lifecycle authority".to_string(),
                        ))
                    },
                    Some((HITL_REDUCER_GENERIC_PENDING, _))
                        if source == "app_owner_notification" =>
                    {
                        Err(ArtifactV2Error::InvalidRequest(
                            "app resolution cannot consume generic lifecycle authority".to_string(),
                        ))
                    },
                    _ if source == "app_owner_notification" => {
                        Err(ArtifactV2Error::InvalidRequest(
                            "app resolution requires existing app lifecycle authority".to_string(),
                        ))
                    },
                    _ => {
                        self.upsert(
                            &key,
                            HITL_REDUCER_GENERIC_RESOLVED,
                            *timestamp,
                            None,
                            Some(&fingerprint.0),
                        )?;
                        Ok(false)
                    },
                }
            },
            _ => Ok(false),
        }
    }

    fn ingest_proof(&self, proof: &HitlLifecycleProofV1) -> Result<bool, ArtifactV2Error> {
        let key = (
            proof.principal.clone(),
            proof.workspace.clone(),
            proof.correlation_id.clone(),
        );
        match proof.absolute_expires_at_ms {
            Some(expiry) => {
                let state = match proof.state {
                    HitlLifecycleProofStateV1::PendingRedacted => HITL_REDUCER_APP_PENDING,
                    HitlLifecycleProofStateV1::Resolved => HITL_REDUCER_APP_RESOLVED,
                };
                self.upsert(&key, state, proof.timestamp, Some(expiry), None)?;
                Ok(true)
            },
            None => {
                self.upsert(
                    &key,
                    HITL_REDUCER_GENERIC_RESOLVED,
                    proof.timestamp,
                    None,
                    None,
                )?;
                Ok(false)
            },
        }
    }

    fn ingest_proof_v2(&self, proof: &HitlLifecycleProofV2) -> Result<bool, ArtifactV2Error> {
        let fingerprint = validate_hitl_lifecycle_proof_v2(proof)
            .map_err(|detail| ArtifactV2Error::InvalidRequest(detail.to_string()))?;
        let key = (
            proof.principal.clone(),
            proof.workspace.clone(),
            proof.correlation_id.clone(),
        );
        let state = if proof.absolute_expires_at_ms.is_some() {
            HITL_REDUCER_APP_RESOLVED
        } else {
            HITL_REDUCER_GENERIC_RESOLVED
        };
        self.upsert(
            &key,
            state,
            proof.timestamp,
            proof.absolute_expires_at_ms,
            Some(&fingerprint.0),
        )?;
        Ok(proof.absolute_expires_at_ms.is_some())
    }

    fn ingest_record(
        &self,
        record: &[u8],
        oversized: bool,
    ) -> Result<(bool, bool), ArtifactV2Error> {
        if oversized
            || !json_bytes_nesting_is_bounded(record, MAX_RETAINED_JSON_DEPTH)
            || !json_bytes_nodes_are_bounded(record, MAX_HITL_LIFECYCLE_RECORD_NODES)
        {
            return Ok((true, true));
        }
        if record.iter().all(u8::is_ascii_whitespace) {
            return Ok((false, false));
        }
        match serde_json::from_slice::<RuntimeTransportEvent>(record) {
            Ok(event) if scoped_hitl_lifecycle_event(&event) => {
                if matches!(&event, RuntimeTransportEvent::HitlRequested { .. })
                    && app_owner_notification_expiry_ms(&event).is_err()
                {
                    return Ok((true, true));
                }
                if matches!(&event, RuntimeTransportEvent::HitlRequested { .. }) {
                    let Some(key) = scoped_hitl_lifecycle_key(&event) else {
                        return Ok((true, true));
                    };
                    let RuntimeTransportEvent::HitlRequested { timestamp, .. } = &event else {
                        unreachable!("guarded HitlRequested lifecycle event")
                    };
                    let request_expiry = app_owner_notification_expiry_ms(&event).ok().flatten();
                    let request_is_app = request_expiry.is_some();
                    let request_body = if request_is_app {
                        None
                    } else {
                        let Some(body) = bounded_hitl_lifecycle_event_bytes(&event) else {
                            return Ok((true, true));
                        };
                        Some(body)
                    };
                    if let Some(existing) = self.existing_record(&key)? {
                        let existing_is_app = matches!(
                            existing.state,
                            HITL_REDUCER_APP_PENDING | HITL_REDUCER_APP_RESOLVED
                        );
                        let existing_is_resolved = matches!(
                            existing.state,
                            HITL_REDUCER_GENERIC_RESOLVED | HITL_REDUCER_APP_RESOLVED
                        );
                        let exact_pending = if request_is_app {
                            existing.timestamp == *timestamp
                                && existing.absolute_expires_at_ms == request_expiry
                                && existing.body.is_none()
                        } else {
                            existing.timestamp == *timestamp
                                && existing.absolute_expires_at_ms.is_none()
                                && existing.body.as_deref() == request_body.as_deref()
                        };
                        if existing_is_app != request_is_app
                            || existing_is_resolved
                            || !exact_pending
                        {
                            return Ok((true, true));
                        }
                    }
                }
                if let RuntimeTransportEvent::HitlResolved { source, .. } = &event {
                    let Some(key) = scoped_hitl_lifecycle_key(&event) else {
                        return Ok((true, true));
                    };
                    let existing = self.existing_record(&key)?;
                    let app_authority = existing.as_ref().is_some_and(|existing| {
                        existing.absolute_expires_at_ms.is_some()
                            && matches!(
                                existing.state,
                                HITL_REDUCER_APP_PENDING | HITL_REDUCER_APP_RESOLVED
                            )
                    });
                    if (source == "app_owner_notification") != app_authority {
                        // App and generic lifecycles cannot consume each
                        // other's same-key authority.
                        return Ok((true, true));
                    }
                    if let Some(existing) = existing {
                        let existing_is_resolved = matches!(
                            existing.state,
                            HITL_REDUCER_GENERIC_RESOLVED | HITL_REDUCER_APP_RESOLVED
                        );
                        if existing_is_resolved {
                            let RuntimeTransportEvent::HitlResolved { timestamp, .. } = &event
                            else {
                                unreachable!("guarded HitlResolved lifecycle event")
                            };
                            let Some(fingerprint) = hitl_resolved_fingerprint(&event) else {
                                return Ok((true, true));
                            };
                            if existing.timestamp != *timestamp
                                || existing.body.as_deref() != Some(&fingerprint.0[..])
                            {
                                return Ok((true, true));
                            }
                        }
                    }
                }
                self.ingest_event(&event).map(|privacy| (privacy, false))
            },
            Ok(event) => Ok((is_app_owner_notification_transport_event(&event), true)),
            Err(_) => match serde_json::from_slice::<HitlLifecycleProofV2>(record) {
                Ok(proof) if validate_hitl_lifecycle_proof_v2(&proof).is_ok() => {
                    let Some(proof_fingerprint) =
                        parse_hitl_resolved_fingerprint(&proof.resolved_event_sha256)
                    else {
                        return Ok((true, true));
                    };
                    let key = (
                        proof.principal.clone(),
                        proof.workspace.clone(),
                        proof.correlation_id.clone(),
                    );
                    let existing = self.existing_record(&key)?;
                    let proof_is_app = proof.absolute_expires_at_ms.is_some();
                    let admitted = match existing {
                        None => true,
                        Some(existing) => {
                            let existing_is_app = matches!(
                                existing.state,
                                HITL_REDUCER_APP_PENDING | HITL_REDUCER_APP_RESOLVED
                            );
                            if existing_is_app != proof_is_app {
                                false
                            } else if matches!(
                                existing.state,
                                HITL_REDUCER_GENERIC_PENDING | HITL_REDUCER_APP_PENDING
                            ) {
                                existing.absolute_expires_at_ms == proof.absolute_expires_at_ms
                            } else {
                                existing.timestamp == proof.timestamp
                                    && existing.absolute_expires_at_ms
                                        == proof.absolute_expires_at_ms
                                    && existing.body.as_deref() == Some(&proof_fingerprint.0[..])
                            }
                        },
                    };
                    if !admitted {
                        return Ok((true, true));
                    }
                    self.ingest_proof_v2(&proof).map(|privacy| (privacy, false))
                },
                Ok(_) | Err(_) => match serde_json::from_slice::<HitlLifecycleProofV1>(record) {
                    Ok(proof) if validate_hitl_lifecycle_proof(&proof).is_ok() => {
                        let key = (
                            proof.principal.clone(),
                            proof.workspace.clone(),
                            proof.correlation_id.clone(),
                        );
                        let existing = self.existing_record(&key)?;
                        let proof_is_app = proof.absolute_expires_at_ms.is_some();
                        let admitted = match existing {
                            None => true,
                            Some(existing) => {
                                let existing_is_app = matches!(
                                    existing.state,
                                    HITL_REDUCER_APP_PENDING | HITL_REDUCER_APP_RESOLVED
                                );
                                if existing_is_app != proof_is_app {
                                    false
                                } else {
                                    match proof.state {
                                        HitlLifecycleProofStateV1::PendingRedacted => {
                                            existing.state == HITL_REDUCER_APP_PENDING
                                                && existing.timestamp == proof.timestamp
                                                && existing.absolute_expires_at_ms
                                                    == proof.absolute_expires_at_ms
                                        },
                                        HitlLifecycleProofStateV1::Resolved => {
                                            matches!(
                                                existing.state,
                                                HITL_REDUCER_GENERIC_PENDING
                                                    | HITL_REDUCER_APP_PENDING
                                            ) && existing.absolute_expires_at_ms
                                                == proof.absolute_expires_at_ms
                                                || matches!(
                                                    existing.state,
                                                    HITL_REDUCER_GENERIC_RESOLVED
                                                        | HITL_REDUCER_APP_RESOLVED
                                                ) && existing.timestamp == proof.timestamp
                                                    && existing.absolute_expires_at_ms
                                                        == proof.absolute_expires_at_ms
                                                    && existing.body.is_none()
                                        },
                                    }
                                }
                            },
                        };
                        if !admitted {
                            return Ok((true, true));
                        }
                        self.ingest_proof(&proof).map(|privacy| (privacy, false))
                    },
                    Ok(_) | Err(_) => Ok((true, true)),
                },
            },
        }
    }
}

impl HitlLifecycleCompactionStaging {
    fn new(journal_path: &std::path::Path) -> Result<(Self, std::fs::File), ArtifactV2Error> {
        let work_item =
            HitlLifecycleWorkItem::new(journal_path, HitlLifecycleWorkKind::Compaction)?;
        let path = work_item.path.join("staging.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(&path)?;
        Ok((Self { path, work_item }, file))
    }
}

impl Drop for HitlLifecycleCompactionStaging {
    fn drop(&mut self) {
        if let Err(cleanup_error) = std::fs::remove_file(&self.path) {
            if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    path = %self.path.display(),
                    error = %cleanup_error,
                    "[MAGICIAN-RUNTIME-EVENTS] Failed to remove lifecycle compaction staging file"
                );
            }
        }
        if let Err(cleanup_error) = std::fs::remove_dir(&self.work_item.path) {
            if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    path = %self.work_item.path.display(),
                    error = %cleanup_error,
                    "[MAGICIAN-RUNTIME-EVENTS] Failed to remove lifecycle compaction work directory"
                );
            }
        }
        self.work_item.release_lease();
    }
}

#[derive(Debug)]
struct HitlLifecycleReducedSummary {
    complete_len: u64,
    file_len: u64,
    file_sha256: String,
    encountered_invalid_record: bool,
}

fn reduce_hitl_lifecycle_journal_to_disk(
    path: &std::path::Path,
    reducer: &HitlLifecycleDiskReducer,
) -> Result<HitlLifecycleReducedSummary, ArtifactV2Error> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    validate_private_workspace_file_path_identity(&file, path, "HITL lifecycle compaction input")?;
    let file_len = file.metadata()?.len();
    let mut file_digest = Sha256::new();
    let mut chunk = [0_u8; HITL_LIFECYCLE_READ_CHUNK_BYTES];
    let mut record = Vec::with_capacity(HITL_LIFECYCLE_READ_CHUNK_BYTES);
    let mut record_oversized = false;
    let mut bytes_read = 0_u64;
    let mut complete_len = 0_u64;
    let mut encountered_invalid_record = false;
    reducer.begin()?;

    while bytes_read < file_len {
        let requested = (file_len - bytes_read).min(chunk.len() as u64) as usize;
        let count = file.read(&mut chunk[..requested])?;
        if count == 0 {
            return Err(ArtifactV2Error::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "HITL lifecycle journal shortened during disk reduction",
            )));
        }
        file_digest.update(&chunk[..count]);
        let mut start = 0usize;
        while start < count {
            let newline = chunk[start..count]
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|relative| start + relative);
            let end = newline.unwrap_or(count);
            let segment = &chunk[start..end];
            if !record_oversized {
                if record
                    .len()
                    .checked_add(segment.len())
                    .is_some_and(|next| next <= MAX_HITL_LIFECYCLE_RECORD_BYTES)
                {
                    record.extend_from_slice(segment);
                } else {
                    record.clear();
                    record_oversized = true;
                }
            }
            let consumed = end - start + usize::from(newline.is_some());
            bytes_read = bytes_read.saturating_add(consumed as u64);
            start = end + usize::from(newline.is_some());
            if newline.is_some() {
                let (_, invalid) = reducer.ingest_record(&record, record_oversized)?;
                encountered_invalid_record |= invalid;
                complete_len = bytes_read;
                record.clear();
                record_oversized = false;
            }
        }
    }

    validate_private_workspace_file_path_identity(&file, path, "HITL lifecycle compaction input")?;
    let current_len = file.metadata()?.len();
    if current_len < file_len {
        return Err(ArtifactV2Error::InvalidRequest(
            "HITL lifecycle journal shortened during disk reduction".to_string(),
        ));
    }
    // Growth is safe to defer to the final destination CAS: the reducer and
    // digest describe the exact original prefix, so an append becomes the
    // typed CAS conflict that drives the bounded liveness fallback. Refusing
    // here would let steady appends starve before the conflict counter moves.
    reducer.finish()?;
    Ok(HitlLifecycleReducedSummary {
        complete_len,
        file_len,
        file_sha256: format!("{:x}", file_digest.finalize()),
        encountered_invalid_record,
    })
}

fn hitl_lifecycle_import_archive_path(
    journal_path: &std::path::Path,
) -> Result<std::path::PathBuf, ArtifactV2Error> {
    let parent = journal_path.parent().ok_or_else(|| {
        ArtifactV2Error::InvalidRequest(
            "lifecycle journal has no private workspace parent".to_string(),
        )
    })?;
    Ok(parent.join(HITL_LIFECYCLE_IMPORT_ARCHIVE_FILENAME))
}

fn validate_hitl_lifecycle_legacy_fence(
    journal_path: &std::path::Path,
) -> Result<(), ArtifactV2Error> {
    let metadata = std::fs::symlink_metadata(journal_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
        let directory = options.open(journal_path)?;
        let opened = directory.metadata()?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || !opened.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || opened.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o700
            || opened.mode() & 0o777 != 0o700
            || metadata.dev() != opened.dev()
            || metadata.ino() != opened.ino()
        {
            return Err(ArtifactV2Error::InvalidRequest(
                "legacy lifecycle pathname is not the private downgrade fence".to_string(),
            ));
        }
    }
    #[cfg(not(unix))]
    if !metadata.is_dir() {
        return Err(ArtifactV2Error::InvalidRequest(
            "legacy lifecycle pathname is not the downgrade fence".to_string(),
        ));
    }
    Ok(())
}

fn create_hitl_lifecycle_legacy_fence(
    journal_path: &std::path::Path,
) -> Result<(), ArtifactV2Error> {
    match std::fs::symlink_metadata(journal_path) {
        Ok(_) => return validate_hitl_lifecycle_legacy_fence(journal_path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error.into()),
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(journal_path)?;
    validate_hitl_lifecycle_legacy_fence(journal_path)?;
    if let Some(parent) = journal_path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn create_empty_hitl_lifecycle_import_archive(
    archive_path: &std::path::Path,
) -> Result<(), ArtifactV2Error> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(archive_path)?;
    validate_private_workspace_file_path_identity(
        &file,
        archive_path,
        "empty lifecycle import archive",
    )?;
    file.sync_all()?;
    if let Some(parent) = archive_path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn remove_ready_hitl_lifecycle_import_archive(
    journal_path: &std::path::Path,
) -> Result<(), ArtifactV2Error> {
    let archive_path = hitl_lifecycle_import_archive_path(journal_path)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let archive = match options.open(&archive_path) {
        Ok(archive) => archive,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    validate_private_workspace_file_path_identity(
        &archive,
        &archive_path,
        "ready lifecycle import archive cleanup",
    )?;
    drop(archive);
    std::fs::remove_file(&archive_path)?;
    if let Some(parent) = archive_path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn initialize_hitl_lifecycle_authority(
    workspace: &ArtifactV2Workspace,
    journal_path: &std::path::Path,
    authority_path: &std::path::Path,
) -> Result<HitlLifecycleDiskReducer, ArtifactV2Error> {
    let authority = HitlLifecycleDiskReducer::open_authority(authority_path)?;
    if authority.authority_is_ready()? {
        validate_hitl_lifecycle_legacy_fence(journal_path)?;
        remove_ready_hitl_lifecycle_import_archive(journal_path)?;
        return Ok(authority);
    }
    let archive_path = hitl_lifecycle_import_archive_path(journal_path)?;
    let import_path = match std::fs::symlink_metadata(journal_path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            if std::fs::symlink_metadata(&archive_path).is_ok() {
                return Err(ArtifactV2Error::InvalidRequest(
                    "lifecycle import found both live journal and crash archive".to_string(),
                ));
            }
            journal_path.to_path_buf()
        },
        Ok(_) => {
            validate_hitl_lifecycle_legacy_fence(journal_path)?;
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW);
            }
            let archive = options.open(&archive_path)?;
            validate_private_workspace_file_path_identity(
                &archive,
                &archive_path,
                "restartable lifecycle import archive",
            )?;
            archive_path.clone()
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::symlink_metadata(&archive_path) {
                Ok(_) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    create_empty_hitl_lifecycle_import_archive(&archive_path)?;
                },
                Err(error) => return Err(error.into()),
            }
            archive_path.clone()
        },
        Err(error) => return Err(error.into()),
    };
    authority.reset_authority_for_import()?;
    match reduce_hitl_lifecycle_journal_to_disk(&import_path, &authority) {
        Ok(summary)
            if summary.complete_len == summary.file_len && !summary.encountered_invalid_record => {
        },
        Ok(summary) => {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "legacy lifecycle import is incomplete or invalid (complete={}, file={})",
                summary.complete_len, summary.file_len
            )));
        },
        Err(error) => return Err(error),
    }
    if import_path == journal_path {
        workspace.rename_path_sync(journal_path, &archive_path)?;
        if let Some(parent) = journal_path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
    }
    create_hitl_lifecycle_legacy_fence(journal_path)?;
    // The ready record is deliberately published only after both the complete
    // import commit and durable directory fence. A crash before this write
    // resets and reimports the retained archive; a crash afterward cannot let
    // an older JSONL writer reopen split authority at the legacy pathname.
    authority.mark_authority_ready()?;
    remove_ready_hitl_lifecycle_import_archive(journal_path)?;
    Ok(authority)
}

fn prepare_exact_hitl_lifecycle_reducer(
    persistence: &HitlLifecyclePersistence,
) -> Result<HitlLifecycleDiskReducer, ArtifactV2Error> {
    if let Some(authority_path) = persistence.authority_path.as_ref() {
        validate_hitl_lifecycle_legacy_fence(&persistence.path)?;
        let authority = HitlLifecycleDiskReducer::open_authority(authority_path)?;
        if !authority.authority_is_ready()? {
            return Err(ArtifactV2Error::InvalidRequest(
                "lifecycle authority is not durably ready".to_string(),
            ));
        }
        return Ok(authority);
    }
    Err(ArtifactV2Error::InvalidRequest(
        "keyed lifecycle authority is unavailable after migration refusal".to_string(),
    ))
}

fn stage_reduced_hitl_lifecycle(
    reducer: &HitlLifecycleDiskReducer,
    staging: &mut File,
    digest: &mut Sha256,
) -> Result<Option<i64>, ArtifactV2Error> {
    let connection = reducer.connection()?;
    let mut statement = connection
        .prepare(
            "SELECT principal, workspace, correlation_id, state, timestamp,
                    absolute_expires_at_ms, body
             FROM lifecycle
             ORDER BY principal, workspace, correlation_id",
        )
        .map_err(|error| hitl_reducer_error("failed to prepare lifecycle reducer output", error))?;
    let mut rows = statement
        .query([])
        .map_err(|error| hitl_reducer_error("failed to query lifecycle reducer output", error))?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut earliest_retained_app_expiry = None::<i64>;
    while let Some(row) = rows
        .next()
        .map_err(|error| hitl_reducer_error("failed to read lifecycle reducer output", error))?
    {
        let principal: String = row
            .get(0)
            .map_err(|error| hitl_reducer_error("invalid lifecycle reducer principal", error))?;
        let workspace: String = row
            .get(1)
            .map_err(|error| hitl_reducer_error("invalid lifecycle reducer workspace", error))?;
        let correlation_id: String = row
            .get(2)
            .map_err(|error| hitl_reducer_error("invalid lifecycle reducer correlation", error))?;
        let state: i64 = row
            .get(3)
            .map_err(|error| hitl_reducer_error("invalid lifecycle reducer state", error))?;
        let timestamp: i64 = row
            .get(4)
            .map_err(|error| hitl_reducer_error("invalid lifecycle reducer timestamp", error))?;
        let absolute_expires_at_ms: Option<i64> = row
            .get(5)
            .map_err(|error| hitl_reducer_error("invalid lifecycle reducer expiry", error))?;
        let body: Option<Vec<u8>> = row
            .get(6)
            .map_err(|error| hitl_reducer_error("invalid lifecycle reducer body", error))?;
        let key = (principal, workspace, correlation_id);
        match state {
            HITL_REDUCER_GENERIC_PENDING => {
                let body = body.ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest(
                        "generic pending lifecycle reducer row has no body".to_string(),
                    )
                })?;
                if body.len() > MAX_HITL_LIFECYCLE_RECORD_BYTES {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "generic pending lifecycle reducer body exceeds admission".to_string(),
                    ));
                }
                staging.write_all(&body)?;
                staging.write_all(b"\n")?;
                digest.update(&body);
                digest.update(b"\n");
            },
            HITL_REDUCER_GENERIC_RESOLVED => {
                let mut record = BoundedHitlLifecycleRecord::new();
                match body {
                    Some(body) => {
                        let bytes: [u8; 32] = body.try_into().map_err(|_| {
                            ArtifactV2Error::InvalidRequest(
                                "generic resolved lifecycle reducer row has an invalid fingerprint"
                                    .to_string(),
                            )
                        })?;
                        write_hitl_lifecycle_proof_v2(
                            &mut record,
                            &hitl_lifecycle_proof_v2(
                                &key,
                                timestamp,
                                None,
                                HitlResolvedFingerprint(bytes),
                            ),
                        )?;
                    },
                    None => write_hitl_lifecycle_proof(
                        &mut record,
                        &hitl_lifecycle_proof(
                            &key,
                            HitlLifecycleProofStateV1::Resolved,
                            timestamp,
                            None,
                        ),
                    )?,
                }
                write_compacted_hitl_lifecycle_record(staging, digest, record)?;
            },
            HITL_REDUCER_APP_PENDING | HITL_REDUCER_APP_RESOLVED => {
                let expiry = absolute_expires_at_ms.ok_or_else(|| {
                    ArtifactV2Error::InvalidRequest(
                        "app lifecycle reducer row has no absolute expiry".to_string(),
                    )
                })?;
                if expiry <= now_ms {
                    continue;
                }
                earliest_retained_app_expiry = Some(
                    earliest_retained_app_expiry.map_or(expiry, |earliest| earliest.min(expiry)),
                );
                let proof_state = if state == HITL_REDUCER_APP_PENDING {
                    HitlLifecycleProofStateV1::PendingRedacted
                } else {
                    HitlLifecycleProofStateV1::Resolved
                };
                let mut record = BoundedHitlLifecycleRecord::new();
                match (proof_state, body) {
                    (HitlLifecycleProofStateV1::Resolved, Some(body)) => {
                        let bytes: [u8; 32] = body.try_into().map_err(|_| {
                            ArtifactV2Error::InvalidRequest(
                                "app resolved lifecycle reducer row has an invalid fingerprint"
                                    .to_string(),
                            )
                        })?;
                        write_hitl_lifecycle_proof_v2(
                            &mut record,
                            &hitl_lifecycle_proof_v2(
                                &key,
                                timestamp,
                                Some(expiry),
                                HitlResolvedFingerprint(bytes),
                            ),
                        )?;
                    },
                    _ => write_hitl_lifecycle_proof(
                        &mut record,
                        &hitl_lifecycle_proof(&key, proof_state, timestamp, Some(expiry)),
                    )?,
                }
                write_compacted_hitl_lifecycle_record(staging, digest, record)?;
            },
            _ => {
                return Err(ArtifactV2Error::InvalidRequest(
                    "lifecycle reducer contains an unknown state".to_string(),
                ));
            },
        }
    }
    Ok(earliest_retained_app_expiry)
}

fn write_compacted_hitl_lifecycle_record(
    staging: &mut File,
    digest: &mut Sha256,
    mut record: BoundedHitlLifecycleRecord,
) -> Result<(), ArtifactV2Error> {
    if record.exceeded {
        return Err(ArtifactV2Error::InvalidRequest(
            "compacted HITL lifecycle record exceeds byte ceiling".to_string(),
        ));
    }
    record.bytes.push(b'\n');
    staging.write_all(&record.bytes)?;
    digest.update(&record.bytes);
    Ok(())
}

fn repair_truncated_hitl_lifecycle_tail(
    path: &std::path::Path,
    observed_len: u64,
    complete_len: u64,
    expected_sha256: &str,
) -> Result<(), ArtifactV2Error> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path)?;
    ensure_private_workspace_file_path_identity(&file, path, "HITL lifecycle tail repair")?;
    let current_len = file.metadata()?.len();
    if current_len != observed_len || complete_len > current_len {
        return Err(ArtifactV2Error::InvalidRequest(format!(
            "HITL lifecycle journal changed before tail repair (observed {observed_len}, current {current_len}, complete {complete_len})"
        )));
    }
    let mut remaining = observed_len;
    let mut digest = Sha256::new();
    let mut chunk = [0_u8; HITL_LIFECYCLE_READ_CHUNK_BYTES];
    while remaining > 0 {
        let requested = remaining.min(chunk.len() as u64) as usize;
        let count = file.read(&mut chunk[..requested])?;
        if count == 0 {
            return Err(ArtifactV2Error::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "HITL lifecycle journal shortened before tail repair",
            )));
        }
        digest.update(&chunk[..count]);
        remaining -= count as u64;
    }
    ensure_private_workspace_file_path_identity(&file, path, "HITL lifecycle tail repair")?;
    let actual_sha256 = format!("{:x}", digest.finalize());
    if file.metadata()?.len() != observed_len
        || !actual_sha256.eq_ignore_ascii_case(expected_sha256)
    {
        return Err(ArtifactV2Error::InvalidRequest(
            "HITL lifecycle journal changed before tail repair CAS".to_string(),
        ));
    }
    file.set_len(complete_len)?;
    file.sync_all()?;
    ensure_private_workspace_file_path_identity(
        &file,
        path,
        "HITL lifecycle repaired startup tail publication",
    )?;
    Ok(())
}

/// Remove only a provably uncommitted final JSONL fragment before a new
/// cross-process append. A peer may die after writing part of a record; if the
/// next process appended directly, its valid record and newline would turn
/// that fragment into one corrupt *complete* row. The reverse scan uses fixed
/// memory and refuses fragments beyond the admitted record ceiling.
fn repair_unterminated_hitl_lifecycle_tail_before_append(
    path: &std::path::Path,
) -> Result<bool, ArtifactV2Error> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    ensure_private_workspace_file_path_identity(
        &file,
        path,
        "HITL lifecycle pre-append tail repair",
    )?;
    let observed_len = file.metadata()?.len();
    if observed_len == 0 {
        return Ok(false);
    }

    file.seek(std::io::SeekFrom::End(-1))?;
    let mut final_byte = [0_u8; 1];
    file.read_exact(&mut final_byte)?;
    if final_byte[0] == b'\n' {
        ensure_private_workspace_file_path_identity(
            &file,
            path,
            "HITL lifecycle pre-append tail validation",
        )?;
        if file.metadata()?.len() != observed_len {
            return Err(ArtifactV2Error::InvalidRequest(
                "HITL lifecycle journal changed during pre-append tail validation".to_string(),
            ));
        }
        return Ok(false);
    }

    let scan_ceiling = (MAX_HITL_LIFECYCLE_RECORD_BYTES as u64).saturating_add(1);
    let mut cursor = observed_len;
    let mut remaining = observed_len.min(scan_ceiling);
    let mut chunk = [0_u8; HITL_LIFECYCLE_READ_CHUNK_BYTES];
    let mut truncate_to = None;
    while remaining > 0 {
        let requested = remaining.min(chunk.len() as u64) as usize;
        cursor = cursor.saturating_sub(requested as u64);
        file.seek(std::io::SeekFrom::Start(cursor))?;
        file.read_exact(&mut chunk[..requested])?;
        if let Some(newline) = chunk[..requested].iter().rposition(|byte| *byte == b'\n') {
            truncate_to = Some(cursor.saturating_add(newline as u64).saturating_add(1));
            break;
        }
        remaining = remaining.saturating_sub(requested as u64);
    }
    let truncate_to = match truncate_to {
        Some(committed_len)
            if observed_len.saturating_sub(committed_len)
                <= MAX_HITL_LIFECYCLE_RECORD_BYTES as u64 =>
        {
            committed_len
        },
        None if observed_len <= MAX_HITL_LIFECYCLE_RECORD_BYTES as u64 => 0,
        _ => {
            ensure_private_workspace_file_path_identity(
                &file,
                path,
                "HITL lifecycle over-bound tail refusal",
            )?;
            if file.metadata()?.len() != observed_len {
                return Err(ArtifactV2Error::InvalidRequest(
                    "HITL lifecycle journal changed during over-bound tail refusal".to_string(),
                ));
            }
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "unterminated HITL lifecycle suffix exceeds the bounded {}-byte repair ceiling",
                MAX_HITL_LIFECYCLE_RECORD_BYTES
            )));
        },
    };

    ensure_private_workspace_file_path_identity(
        &file,
        path,
        "HITL lifecycle pre-append tail repair CAS",
    )?;
    if file.metadata()?.len() != observed_len {
        return Err(ArtifactV2Error::InvalidRequest(
            "HITL lifecycle journal changed before pre-append tail repair CAS".to_string(),
        ));
    }
    file.set_len(truncate_to)?;
    file.sync_all()?;
    ensure_private_workspace_file_path_identity(
        &file,
        path,
        "HITL lifecycle repaired tail publication",
    )?;
    Ok(true)
}

#[derive(Clone, Debug)]
struct HitlLifecyclePersistence {
    workspace: ArtifactV2Workspace,
    /// Legacy append journal retained only as the one-time V3 import source
    /// and as the stable cross-version publication-lock namespace.
    path: std::path::PathBuf,
    /// Present only after a complete, clean JSONL import has durably published
    /// the keyed SQLite authority. `None` makes post-startup exact operations
    /// fail closed when migration could not prove every record.
    authority_path: Option<std::path::PathBuf>,
}

#[derive(Clone, Debug)]
pub enum HitlLifecycleState {
    Pending(RuntimeTransportEvent),
    Resolved,
    Unknown,
    Unavailable,
}

/// V2 Event Broadcaster for real-time updates
#[derive(Clone)]
pub struct RuntimeTransportBroadcaster {
    service_health_notices: Arc<Mutex<service_health::HealthNotices>>,
    sender: broadcast::Sender<RuntimeTransportEvent>,
    /// Optional conversation store for persisting stage/provider metadata
    conversation_store: Option<Arc<dyn V2ConversationStore>>,
    runtime_canonical_event_sink: Arc<RwLock<Option<Arc<dyn RuntimeCanonicalEventSink>>>>,
    runtime_canonical_event_scopes: Arc<DashMap<String, CanonicalEventScope>>,
    /// Canonical HITL requests that are still pending, keyed by the same
    /// `(principal, workspace, correlation_id)` contract used by notification
    /// deep links. Updating this map synchronously before broadcasting closes
    /// the race where a taskless request (for example `bot_auth`) is clicked
    /// before any asynchronous projection has observed it.
    pending_hitl_requests: Arc<DashMap<(String, String, String), RuntimeTransportEvent>>,
    /// Durable resolution tombstones prevent an asynchronously stale V3
    /// projection from reopening a request after a crash between lifecycle
    /// journal commit and canonical projection persistence.
    resolved_hitl_requests: Arc<DashMap<HitlLifecycleKey, GenericHitlResolvedState>>,
    /// Expiry-aware, content-free authority for app owner notifications. A
    /// redacted pending proof is deliberately not enough to surface a prompt;
    /// the durable UserRequest owner must republish the still-live body after
    /// restart. Resolved proofs suppress stale projections only until TTL.
    app_owner_notification_lifecycle:
        Arc<DashMap<HitlLifecycleKey, AppOwnerNotificationLifecycleState>>,
    app_owner_notification_expiry_index: Arc<Mutex<BTreeMap<i64, HashSet<HitlLifecycleKey>>>>,
    app_owner_notification_expiry_sweeper_started: Arc<AtomicBool>,
    /// Process-local ownership of the restartable, sliced filesystem orphan
    /// sweeper. Cross-process safety is provided separately by each work
    /// item's advisory lease.
    hitl_lifecycle_orphan_sweeper_started: Arc<AtomicBool>,
    /// Dedicated authoritative lifecycle journal. Unlike the observability
    /// transport log it is synchronous, non-retention-bounded, and committed
    /// before registry/canonical/broadcast visibility.
    hitl_lifecycle_persistence: Option<Arc<HitlLifecyclePersistence>>,
    hitl_lifecycle_recovery_healthy: Arc<AtomicBool>,
    /// Consecutive optimistic destination-CAS conflicts. Reaching the small
    /// fixed threshold forces one publication-owned reduction so a busy peer
    /// cannot starve physical lifecycle cleanup indefinitely.
    hitl_lifecycle_compaction_cas_conflicts: Arc<AtomicU8>,
    /// Serializes canonical persistence, durable lifecycle/registry
    /// mutation, and broadcast. The broadcast channel defines the order seen
    /// by consumers, so every authoritative surface must commit in that exact
    /// same order when request and resolution race on producer threads.
    hitl_lifecycle_lock: Arc<Mutex<()>>,
    /// Per-delegate-execution-id chat fan-out registry. When a chat
    /// session delegates to another agent, it registers the
    /// delegate's `execution_id` here. `emit_scoped_or_unscoped`
    /// inspects each emitted event's `payload.execution_id` and, on
    /// hit, emits a duplicate envelope re-stamped with the chat's
    /// agent_id+scope so the chat session's existing transport
    /// subscription receives reasoning / tool.call / plan events from
    /// the delegated execution alongside its progress stream. Cleared
    /// at turn end; see `chat/service.rs::dispatch_delegate_to_agent`
    /// and `unsubscribe_turn_subscriptions`.
    /// Per-task-id chat fan-out registry. The single fanout map —
    /// Used by chat → task immersion: at task-create time we know the
    /// `task_id` but not the `execution_id` (the scheduler picks it up
    /// later), so the chat dispatcher registers here. Inner-loop
    /// events emitted via `emit_named` always carry both `task_id` and
    /// `execution_id` in the payload (see
    /// `execution/primitive/runner.rs:2006-2022`), so the fanout pass
    /// in `emit_scoped_or_unscoped` looks up targets under both keys
    /// and unions the lists before re-stamping. Cleared on terminal
    /// status, single-slot subscription replacement, or chat-session
    /// cleanup — see `chat/service.rs::dispatch_create_task` and the
    /// `subscribe_to_task` / unsubscribe paths.
    chat_fanout_by_task: Arc<DashMap<String, Vec<ChatFanoutTarget>>>,
    v3_planning_transport_scopes: Arc<DashMap<String, V3PlanningTransportScope>>,
}

#[derive(Clone, Debug)]
struct V3PlanningTransportScope {
    principal: String,
    workspace: String,
    task_id: String,
    task_title: String,
    agent_id: String,
    plan_id: String,
    ui_thread_id: String,
}

impl std::fmt::Debug for RuntimeTransportBroadcaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeTransportBroadcaster")
            .finish_non_exhaustive()
    }
}

impl RuntimeTransportBroadcaster {
    /// Create a new event broadcaster
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            sender,
            service_health_notices: Arc::new(Mutex::new(service_health::HealthNotices::default())),
            conversation_store: None,
            runtime_canonical_event_sink: Arc::new(RwLock::new(None)),
            runtime_canonical_event_scopes: Arc::new(DashMap::new()),
            pending_hitl_requests: Arc::new(DashMap::new()),
            resolved_hitl_requests: Arc::new(DashMap::new()),
            app_owner_notification_lifecycle: Arc::new(DashMap::new()),
            app_owner_notification_expiry_index: Arc::new(Mutex::new(BTreeMap::new())),
            app_owner_notification_expiry_sweeper_started: Arc::new(AtomicBool::new(false)),
            hitl_lifecycle_orphan_sweeper_started: Arc::new(AtomicBool::new(false)),
            hitl_lifecycle_persistence: None,
            hitl_lifecycle_recovery_healthy: Arc::new(AtomicBool::new(true)),
            hitl_lifecycle_compaction_cas_conflicts: Arc::new(AtomicU8::new(0)),
            hitl_lifecycle_lock: Arc::new(Mutex::new(())),
            chat_fanout_by_task: Arc::new(DashMap::new()),
            v3_planning_transport_scopes: Arc::new(DashMap::new()),
        }
    }

    /// Attach a conversation store for persisting stage/provider metadata
    pub fn with_store(mut self, store: Arc<dyn V2ConversationStore>) -> Self {
        self.conversation_store = Some(store);
        self
    }

    /// Attach the keyed durable lifecycle authority before the broadcaster is
    /// shared with any producer. Legacy JSONL receives one streaming import;
    /// ready V3 startup is constant work and exact reads hydrate only the key
    /// requested instead of rebuilding aggregate process memory.
    pub fn with_hitl_lifecycle_persistence(mut self, workspace: ArtifactV2Workspace) -> Self {
        let path = workspace.base_root().join(HITL_LIFECYCLE_JOURNAL_FILENAME);
        let authority_path =
            host_database_path(workspace.base_root(), DatabaseOwner::HitlLifecycle);
        let mut requires_privacy_compaction = false;
        let mut authority_ready = false;
        match AgentStorage::acquire_file_lock_exclusive_sync(&path) {
            Ok(startup_publication_guard) => {
                let existing_authority = std::fs::symlink_metadata(&authority_path)
                    .ok()
                    .and_then(|_| HitlLifecycleDiskReducer::open_authority(&authority_path).ok());
                if let Some(_authority) = existing_authority
                    .filter(|authority| matches!(authority.authority_is_ready(), Ok(true)))
                {
                    match validate_hitl_lifecycle_legacy_fence(&path)
                        .and_then(|()| remove_ready_hitl_lifecycle_import_archive(&path))
                    {
                        Ok(()) => authority_ready = true,
                        Err(read_error) => {
                            self.hitl_lifecycle_recovery_healthy
                                .store(false, Ordering::SeqCst);
                            error!(
                                error = %read_error,
                                path = %authority_path.display(),
                                "[MAGICIAN-RUNTIME-EVENTS] Ready HITL authority has no durable downgrade fence; new scoped HITL is withheld"
                            );
                        },
                    }
                } else if validate_hitl_lifecycle_legacy_fence(&path).is_ok() {
                    // A prior migration crashed after installing the durable
                    // downgrade fence but before publishing DB readiness.
                    // The retained private archive is reimported below; do
                    // not try to read the directory sentinel as JSONL.
                } else {
                    match self.replay_hitl_lifecycle_journal(&path) {
                        Ok(Some(summary)) if summary.complete_len != summary.file_len => {
                            requires_privacy_compaction = summary.requires_privacy_compaction;
                            let incomplete_bytes = summary.file_len - summary.complete_len;
                            let repair_result = if incomplete_bytes
                                > MAX_HITL_LIFECYCLE_RECORD_BYTES as u64
                            {
                                Err(ArtifactV2Error::InvalidRequest(format!(
                                "unterminated HITL lifecycle suffix exceeds the bounded {}-byte repair ceiling",
                                MAX_HITL_LIFECYCLE_RECORD_BYTES
                            )))
                            } else {
                                repair_truncated_hitl_lifecycle_tail(
                                    &path,
                                    summary.file_len,
                                    summary.complete_len,
                                    &summary.file_sha256,
                                )
                            };
                            match repair_result {
                                Ok(()) => warn!(
                                    path = %path.display(),
                                    truncated_bytes = incomplete_bytes,
                                    "[MAGICIAN-RUNTIME-EVENTS] Repaired truncated HITL lifecycle journal tail"
                                ),
                                Err(repair_error) => {
                                    self.hitl_lifecycle_recovery_healthy
                                        .store(false, Ordering::SeqCst);
                                    error!(
                                        error = %repair_error,
                                        path = %path.display(),
                                        "[MAGICIAN-RUNTIME-EVENTS] Failed to repair truncated HITL lifecycle journal; unknown exact lookups will fail closed"
                                    );
                                },
                            }
                        },
                        Ok(Some(summary)) => {
                            requires_privacy_compaction = summary.requires_privacy_compaction;
                        },
                        Ok(None) => {},
                        Err(ArtifactV2Error::Io(read_error))
                            if read_error.kind() == std::io::ErrorKind::NotFound => {},
                        Err(read_error) => {
                            self.hitl_lifecycle_recovery_healthy
                                .store(false, Ordering::SeqCst);
                            error!(
                                error = %read_error,
                                path = %path.display(),
                                "[MAGICIAN-RUNTIME-EVENTS] Failed to restore HITL lifecycle journal; unknown exact lookups will fail closed"
                            );
                        },
                    }
                }
                drop(startup_publication_guard);
            },
            Err(lock_error) => {
                self.hitl_lifecycle_recovery_healthy
                    .store(false, Ordering::SeqCst);
                error!(
                    error = %lock_error,
                    path = %path.display(),
                    "[MAGICIAN-RUNTIME-EVENTS] Failed to acquire HITL lifecycle startup publication authority; recovery and new scoped HITL are withheld"
                );
            },
        }
        self.hitl_lifecycle_persistence = Some(Arc::new(HitlLifecyclePersistence {
            workspace,
            path: path.clone(),
            authority_path: authority_ready.then(|| authority_path.clone()),
        }));
        if requires_privacy_compaction {
            // Privacy compaction is independent of admission health. A
            // different corrupt row must not pin a successfully identified
            // legacy app-notification body on disk beyond its absolute TTL.
            // The unhealthy bit remains false after a successful rewrite, so
            // unknown lifecycle lookups and new scoped HITL still fail closed.
            if let Err(compaction_error) = self.compact_hitl_lifecycle_journal() {
                self.index_app_owner_notification_compaction_retry();
                self.hitl_lifecycle_recovery_healthy
                    .store(false, Ordering::SeqCst);
                error!(
                    error = %compaction_error,
                    "[MAGICIAN-RUNTIME-EVENTS] Failed to redact recovered app owner notification lifecycle; new scoped HITL is withheld"
                );
            }
        }
        if !authority_ready && self.hitl_lifecycle_recovery_healthy.load(Ordering::SeqCst) {
            let initialization_result = {
                let persistence = self
                    .hitl_lifecycle_persistence
                    .as_ref()
                    .expect("HITL lifecycle persistence is attached before authority import");
                AgentStorage::acquire_file_lock_exclusive_sync(&path)
                    .map_err(|error| ArtifactV2Error::Runtime(error.to_string()))
                    .and_then(|_guard| {
                        initialize_hitl_lifecycle_authority(
                            &persistence.workspace,
                            &path,
                            &authority_path,
                        )
                    })
            };
            match initialization_result {
                Ok(_authority) => {
                    if let Some(persistence) = self.hitl_lifecycle_persistence.as_ref() {
                        self.hitl_lifecycle_persistence =
                            Some(Arc::new(HitlLifecyclePersistence {
                                workspace: persistence.workspace.clone(),
                                path: persistence.path.clone(),
                                authority_path: Some(authority_path.clone()),
                            }));
                    }
                },
                Err(import_error) => {
                    self.hitl_lifecycle_recovery_healthy
                        .store(false, Ordering::SeqCst);
                    error!(
                        error = %import_error,
                        legacy_path = %path.display(),
                        authority_path = %authority_path.display(),
                        "[MAGICIAN-RUNTIME-EVENTS] Failed forward-only HITL lifecycle authority import; new scoped HITL is withheld"
                    );
                },
            }
        }
        self.arm_restored_app_owner_notification_expiries();
        self.ensure_hitl_lifecycle_orphan_sweeper();
        self
    }

    /// Replay newline-committed records with one fixed read chunk and one
    /// capped record buffer. Corrupt/oversized complete records poison only
    /// unknown lookups and are skipped so later lifecycle transitions still
    /// reconstruct the exact known pending/resolved state.
    fn replay_hitl_lifecycle_journal(
        &self,
        path: &std::path::Path,
    ) -> Result<Option<HitlLifecycleReplaySummary>, ArtifactV2Error> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(path)?;
        ensure_private_workspace_file_path_identity(&file, path, "HITL lifecycle replay")?;
        let file_len = file.metadata()?.len();
        let mut file_digest = Sha256::new();
        let mut chunk = [0_u8; HITL_LIFECYCLE_READ_CHUNK_BYTES];
        let mut record = Vec::with_capacity(HITL_LIFECYCLE_READ_CHUNK_BYTES);
        let mut record_oversized = false;
        let mut bytes_read = 0_u64;
        let mut complete_len = 0_u64;
        let mut line_number = 0_u64;
        let mut requires_privacy_compaction = false;

        while bytes_read < file_len {
            let requested = (file_len - bytes_read).min(chunk.len() as u64) as usize;
            let count = file.read(&mut chunk[..requested])?;
            if count == 0 {
                return Err(ArtifactV2Error::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "HITL lifecycle journal shortened during replay",
                )));
            }
            file_digest.update(&chunk[..count]);
            let mut start = 0usize;
            while start < count {
                let newline = chunk[start..count]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map(|relative| start + relative);
                let end = newline.unwrap_or(count);
                let segment = &chunk[start..end];
                if !record_oversized {
                    if record
                        .len()
                        .checked_add(segment.len())
                        .is_some_and(|next| next <= MAX_HITL_LIFECYCLE_RECORD_BYTES)
                    {
                        record.extend_from_slice(segment);
                    } else {
                        record.clear();
                        record_oversized = true;
                    }
                }
                let consumed = end - start + usize::from(newline.is_some());
                bytes_read = bytes_read.saturating_add(consumed as u64);
                start = end + usize::from(newline.is_some());

                if newline.is_some() {
                    line_number = line_number.saturating_add(1);
                    complete_len = bytes_read;
                    requires_privacy_compaction |= self.replay_hitl_lifecycle_record(
                        path,
                        line_number,
                        &record,
                        record_oversized,
                    );
                    record.clear();
                    record_oversized = false;
                }
            }
        }

        ensure_private_workspace_file_path_identity(&file, path, "HITL lifecycle replay")?;
        if file.metadata()?.len() != file_len {
            return Err(ArtifactV2Error::InvalidRequest(
                "HITL lifecycle journal changed while it was being replayed".to_string(),
            ));
        }
        Ok(Some(HitlLifecycleReplaySummary {
            complete_len,
            file_len,
            file_sha256: format!("{:x}", file_digest.finalize()),
            requires_privacy_compaction,
        }))
    }

    fn replay_hitl_lifecycle_record(
        &self,
        path: &std::path::Path,
        line_number: u64,
        record: &[u8],
        oversized: bool,
    ) -> bool {
        if oversized
            || !json_bytes_nesting_is_bounded(record, MAX_RETAINED_JSON_DEPTH)
            || !json_bytes_nodes_are_bounded(record, MAX_HITL_LIFECYCLE_RECORD_NODES)
        {
            self.hitl_lifecycle_recovery_healthy
                .store(false, Ordering::SeqCst);
            error!(
                line = line_number,
                path = %path.display(),
                max_record_bytes = MAX_HITL_LIFECYCLE_RECORD_BYTES,
                max_record_depth = MAX_RETAINED_JSON_DEPTH,
                max_record_nodes = MAX_HITL_LIFECYCLE_RECORD_NODES,
                "[MAGICIAN-RUNTIME-EVENTS] HITL lifecycle journal record exceeds admission; unknown exact lookups will fail closed"
            );
            // Rebuild from admitted state even when this row cannot be
            // classified. Otherwise a damaged/oversized legacy notification
            // body could remain physically pinned merely because its marker
            // is no longer parseable.
            return true;
        }
        if record.iter().all(u8::is_ascii_whitespace) {
            return false;
        }
        match serde_json::from_slice::<RuntimeTransportEvent>(record) {
            Ok(event) if scoped_hitl_lifecycle_event(&event) => {
                self.record_replayed_hitl_lifecycle(event)
            },
            Ok(event) => is_app_owner_notification_transport_event(&event),
            Err(event_parse_error) => {
                match serde_json::from_slice::<HitlLifecycleProofV2>(record) {
                    Ok(proof) if validate_hitl_lifecycle_proof_v2(&proof).is_ok() => {
                        self.record_replayed_hitl_lifecycle_proof_v2(proof)
                    },
                    Ok(_) | Err(_) => {
                        match serde_json::from_slice::<HitlLifecycleProofV1>(record) {
                            Ok(proof) if validate_hitl_lifecycle_proof(&proof).is_ok() => {
                                self.record_replayed_hitl_lifecycle_proof(proof)
                            },
                            Ok(_) | Err(_) => {
                                self.hitl_lifecycle_recovery_healthy
                                    .store(false, Ordering::SeqCst);
                                error!(
                                    error = %event_parse_error,
                                    line = line_number,
                                    path = %path.display(),
                                    "[MAGICIAN-RUNTIME-EVENTS] HITL lifecycle journal is corrupt; unknown exact lookups will fail closed"
                                );
                                // The row is not recoverable authority. Trigger the same
                                // provider-owned rebuild used for app proofs so corrupt
                                // bytes cannot retain an unclassifiable notification body.
                                true
                            },
                        }
                    },
                }
            },
        }
    }

    /// Set the conversation store (for existing instances)
    pub fn set_store(&mut self, store: Arc<dyn V2ConversationStore>) {
        self.conversation_store = Some(store);
    }

    pub fn set_runtime_canonical_event_sink(&self, sink: Arc<dyn RuntimeCanonicalEventSink>) {
        *self
            .runtime_canonical_event_sink
            .write()
            .expect("runtime_canonical_event_sink lock poisoned") = Some(sink);
    }

    pub fn register_runtime_canonical_event_scope(&self, scope: CanonicalEventScope) {
        self.runtime_canonical_event_scopes
            .insert(scope.execution_id.clone(), scope);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn register_v3_planning_transport_scope(
        &self,
        execution_id: String,
        principal: String,
        workspace: String,
        task_id: String,
        task_title: String,
        agent_id: String,
        plan_id: String,
        ui_thread_id: String,
    ) {
        self.v3_planning_transport_scopes.insert(
            execution_id,
            V3PlanningTransportScope {
                principal,
                workspace,
                task_id,
                task_title,
                agent_id,
                plan_id,
                ui_thread_id,
            },
        );
    }

    /// Look up a registered canonical event scope by `execution_id`.
    /// Synchronous DashMap lookup — safe to call from a broadcast
    /// subscriber's hot path. Returns `None` if no scope was registered
    /// for that id (e.g., system-wide events or unregistered children).
    pub fn lookup_canonical_event_scope(&self, execution_id: &str) -> Option<CanonicalEventScope> {
        self.runtime_canonical_event_scopes
            .get(execution_id)
            .map(|entry| entry.clone())
    }

    /// Emit a `ProgressEvent` (wrapped `ProgressMessage`) onto the bus.
    /// Typed shortcut for the most common producer call shape — wraps
    /// `emit_transport_only(RuntimeTransportEvent::ProgressEvent {…})`
    /// with the timestamp stamped. Producers call this directly; the
    /// progress router subscribes like any other consumer and fans out
    /// to its registered channels.
    pub fn emit_progress(&self, message: super::progress_channel_seam::types::ProgressMessage) {
        self.emit_transport_only(RuntimeTransportEvent::ProgressEvent {
            message,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Emit a transport-only event to all subscribers.
    ///
    /// This does not write to canonical `events.jsonl`; callers should use
    /// [`emit`] for durable execution-scoped runtime facts.
    pub fn emit_transport_only(&self, event: RuntimeTransportEvent) {
        let _ = self.emit_transport_only_if_accepted(event);
    }

    /// Admit and publish one scoped HITL lifecycle event, reporting whether
    /// its lifecycle owner accepted it. This narrow receipt is for durable
    /// publication-debt owners: they may delete a content-free retry marker
    /// only after this returns `true`. Generic callers retain the fire-and-
    /// forget semantics of [`Self::emit_transport_only`] and [`Self::emit`].
    ///
    /// A configured lifecycle authority must accept the transition before the live
    /// projection is mutated or broadcast. When persistence is not configured,
    /// the existing in-memory accepted semantics apply. Unscoped/non-HITL,
    /// malformed, oversized, recovery-blocked, serialization-failed, and
    /// append-failed events return `false`. A ready V3 authority also rejects
    /// app-owner notification events here because those require the dedicated
    /// generation-bound reconciliation APIs.
    pub fn emit_hitl_lifecycle_if_accepted(&self, event: RuntimeTransportEvent) -> bool {
        if !hitl_lifecycle_event(&event)
            || !scoped_hitl_lifecycle_event(&event)
            || (is_app_owner_notification_transport_event(&event)
                && self
                    .hitl_lifecycle_persistence
                    .as_ref()
                    .is_some_and(|persistence| persistence.authority_path.is_some()))
        {
            discard_hitl_lifecycle_event_iteratively(event);
            return false;
        }
        self.emit_transport_only_if_accepted(event)
    }

    /// Atomically establish the exact sealed app-notification request proof
    /// and publish its exact resolution under one cross-process publication lock.
    /// This closes the gap where a peer could claim the same key between a
    /// pending-proof recovery and a separate resolution append.
    #[cfg(test)]
    pub(crate) fn ensure_and_resolve_app_owner_notification_lifecycle(
        &self,
        expected_request: &RuntimeTransportEvent,
        resolution: RuntimeTransportEvent,
        generation: AppOwnerNotificationPublicationGeneration,
    ) -> bool {
        let RuntimeTransportEvent::HitlRequested {
            timestamp: request_timestamp,
            ..
        } = expected_request
        else {
            return false;
        };
        let RuntimeTransportEvent::HitlResolved {
            source,
            timestamp: resolution_timestamp,
            ..
        } = &resolution
        else {
            return false;
        };
        if source != "app_owner_notification" || !hitl_lifecycle_event_is_admitted(expected_request)
        {
            return false;
        }
        let request_timestamp = *request_timestamp;
        let resolution_timestamp = *resolution_timestamp;
        let Some(key) = scoped_hitl_lifecycle_key(expected_request) else {
            return false;
        };
        if scoped_hitl_lifecycle_key(&resolution).as_ref() != Some(&key) {
            return false;
        }
        let Ok(Some(absolute_expires_at_ms)) = app_owner_notification_expiry_ms(expected_request)
        else {
            return false;
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        if absolute_expires_at_ms <= now_ms
            || absolute_expires_at_ms <= resolution_timestamp
            || resolution_timestamp < request_timestamp
        {
            return false;
        }
        let Some(resolution_fingerprint) = hitl_resolved_fingerprint(&resolution) else {
            return false;
        };
        let resolution_authority = HitlResolvedAuthority::Verified(resolution_fingerprint);

        let publication_guard = match self.acquire_hitl_lifecycle_publication_lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };

        let install_pending = || {
            self.pending_hitl_requests.remove(&key);
            self.resolved_hitl_requests.remove(&key);
            self.app_owner_notification_lifecycle.insert(
                key.clone(),
                AppOwnerNotificationLifecycleState {
                    state: HitlLifecycleProofStateV1::PendingRedacted,
                    timestamp: request_timestamp,
                    absolute_expires_at_ms,
                    resolved_authority: None,
                    app_owner_generation: Some(generation),
                },
            );
            self.arm_app_owner_notification_expiry(key.clone(), absolute_expires_at_ms);
        };
        let install_resolved = || {
            self.pending_hitl_requests.remove(&key);
            self.resolved_hitl_requests.remove(&key);
            self.app_owner_notification_lifecycle.insert(
                key.clone(),
                AppOwnerNotificationLifecycleState {
                    state: HitlLifecycleProofStateV1::Resolved,
                    timestamp: resolution_timestamp,
                    absolute_expires_at_ms,
                    resolved_authority: Some(resolution_authority),
                    app_owner_generation: Some(generation),
                },
            );
            self.arm_app_owner_notification_expiry(key.clone(), absolute_expires_at_ms);
        };

        let Some(persistence) = self.hitl_lifecycle_persistence.as_ref() else {
            let lifecycle_guard = self
                .hitl_lifecycle_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(existing) = self.app_owner_notification_lifecycle.get(&key) {
                match existing.state {
                    HitlLifecycleProofStateV1::Resolved => {
                        return existing.timestamp == resolution_timestamp
                            && existing.absolute_expires_at_ms == absolute_expires_at_ms
                            && existing.resolved_authority == Some(resolution_authority)
                            && existing.app_owner_generation == Some(generation);
                    },
                    HitlLifecycleProofStateV1::PendingRedacted
                        if existing.timestamp == request_timestamp
                            && existing.absolute_expires_at_ms == absolute_expires_at_ms
                            && existing
                                .app_owner_generation
                                .is_none_or(|stored| stored == generation) => {},
                    _ => return false,
                }
            } else {
                if self.pending_hitl_requests.contains_key(&key)
                    || self.resolved_hitl_requests.contains_key(&key)
                {
                    return false;
                }
                install_pending();
            }
            if absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis()
                || !self.append_hitl_lifecycle_journal(&resolution)
            {
                return false;
            }
            self.record_pending_hitl_lifecycle(&resolution);
            drop(publication_guard);
            self.broadcast_transport_event(resolution);
            drop(lifecycle_guard);
            return true;
        };

        let Ok(reducer) = prepare_exact_hitl_lifecycle_reducer(persistence) else {
            return false;
        };
        let existing = match reducer.existing_record(&key) {
            Ok(existing) => existing,
            Err(_) => return false,
        };

        let lifecycle_guard = self
            .hitl_lifecycle_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis() {
            return false;
        }
        match existing {
            Some(existing) if existing.state == HITL_REDUCER_APP_RESOLVED => {
                if existing.timestamp != resolution_timestamp
                    || existing.absolute_expires_at_ms != Some(absolute_expires_at_ms)
                    || existing.body.as_deref() != Some(&resolution_fingerprint.0[..])
                    || existing.app_owner_generation.as_deref()
                        != Some(generation.canonical().as_str())
                {
                    return false;
                }
                install_resolved();
                true
            },
            Some(existing) if existing.state == HITL_REDUCER_APP_PENDING => {
                if existing.timestamp != request_timestamp
                    || existing.absolute_expires_at_ms != Some(absolute_expires_at_ms)
                {
                    return false;
                }
                let canonical_generation = generation.canonical();
                if existing
                    .app_owner_generation
                    .as_deref()
                    .is_some_and(|stored| stored != canonical_generation.as_str())
                    || reducer
                        .upsert_app_with_generation(
                            &key,
                            HITL_REDUCER_APP_PENDING,
                            request_timestamp,
                            absolute_expires_at_ms,
                            None,
                            &canonical_generation,
                        )
                        .is_err()
                {
                    return false;
                }
                install_pending();
                if absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis()
                    || !self.append_app_owner_notification_lifecycle_after_exact_reduction(
                        &resolution,
                        generation,
                        absolute_expires_at_ms,
                    )
                {
                    return false;
                }
                self.record_pending_hitl_lifecycle(&resolution);
                drop(publication_guard);
                self.broadcast_transport_event(resolution);
                drop(lifecycle_guard);
                true
            },
            Some(_) => false,
            None => {
                if absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis()
                    || !self.append_app_owner_notification_lifecycle_after_exact_reduction(
                        expected_request,
                        generation,
                        absolute_expires_at_ms,
                    )
                {
                    return false;
                }
                if reducer
                    .upsert_app_with_generation(
                        &key,
                        HITL_REDUCER_APP_PENDING,
                        request_timestamp,
                        absolute_expires_at_ms,
                        None,
                        &generation.canonical(),
                    )
                    .is_err()
                {
                    return false;
                }
                install_pending();
                if absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis()
                    || !self.append_app_owner_notification_lifecycle_after_exact_reduction(
                        &resolution,
                        generation,
                        absolute_expires_at_ms,
                    )
                {
                    return false;
                }
                self.record_pending_hitl_lifecycle(&resolution);
                drop(publication_guard);
                self.broadcast_transport_event(resolution);
                drop(lifecycle_guard);
                true
            },
        }
    }

    /// Reconcile one fixed-size page of durable app-notification request debts
    /// with one publication lock and bounded exact keyed lookups. Only the
    /// exact expiry-bound pending app proof may own a key. A terminal proof is
    /// intentionally not accepted because it no longer retains the original
    /// request timestamp needed to prove exact request identity. The returned
    /// opaque ticket retains publication authority for the caller's immediate,
    /// non-awaiting exact-live revalidation and guarded broadcasts; dropping it
    /// releases that authority.
    pub(crate) fn reconcile_app_owner_notification_request_batch(
        &self,
        requests: Vec<(
            RuntimeTransportEvent,
            AppOwnerNotificationPublicationGeneration,
        )>,
    ) -> AppOwnerNotificationRequestPublicationTicket {
        let request_count = requests.len();
        let mut ticket = AppOwnerNotificationRequestPublicationTicket::empty(self, request_count);
        if requests.is_empty() || request_count > HITL_LIFECYCLE_RECONCILE_BATCH {
            return ticket;
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut prepared = Vec::with_capacity(request_count);
        let mut prepared_keys = HashSet::with_capacity(request_count);
        for (index, (request, generation)) in requests.into_iter().enumerate() {
            let RuntimeTransportEvent::HitlRequested {
                source, timestamp, ..
            } = &request
            else {
                continue;
            };
            let timestamp = *timestamp;
            if source != "user_request" || !hitl_lifecycle_event_is_admitted(&request) {
                continue;
            }
            let Some(key) = scoped_hitl_lifecycle_key(&request) else {
                continue;
            };
            let Ok(Some(absolute_expires_at_ms)) = app_owner_notification_expiry_ms(&request)
            else {
                continue;
            };
            if absolute_expires_at_ms <= now_ms || timestamp >= absolute_expires_at_ms {
                continue;
            }
            let Some(body) = bounded_hitl_lifecycle_event_bytes(&request) else {
                continue;
            };
            if !prepared_keys.insert(key.clone()) {
                continue;
            }
            let authorization = app_owner_notification_request_authorization(&body);
            prepared.push(PreparedAppOwnerNotificationRequest {
                index,
                key,
                request,
                authorization,
                timestamp,
                absolute_expires_at_ms,
                generation,
            });
        }
        if prepared.is_empty() {
            return ticket;
        }

        let publication_guard = match self.acquire_hitl_lifecycle_publication_lock() {
            Ok(guard) => guard,
            Err(_) => return ticket,
        };
        let install_pending_proof =
            |key: &HitlLifecycleKey,
             timestamp: i64,
             absolute_expires_at_ms: i64,
             generation: AppOwnerNotificationPublicationGeneration| {
                self.pending_hitl_requests.remove(key);
                self.resolved_hitl_requests.remove(key);
                self.app_owner_notification_lifecycle.insert(
                    key.clone(),
                    AppOwnerNotificationLifecycleState {
                        state: HitlLifecycleProofStateV1::PendingRedacted,
                        timestamp,
                        absolute_expires_at_ms,
                        resolved_authority: None,
                        app_owner_generation: Some(generation),
                    },
                );
                self.arm_app_owner_notification_expiry(key.clone(), absolute_expires_at_ms);
            };
        let Some(persistence) = self.hitl_lifecycle_persistence.as_ref() else {
            let lifecycle_guard = self
                .hitl_lifecycle_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for entry in prepared {
                if entry.absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis() {
                    continue;
                }
                if let Some(existing) = self.app_owner_notification_lifecycle.get(&entry.key) {
                    ticket.accepted[entry.index] = existing.state
                        == HitlLifecycleProofStateV1::PendingRedacted
                        && existing.absolute_expires_at_ms == entry.absolute_expires_at_ms
                        && existing.timestamp == entry.timestamp
                        && existing
                            .app_owner_generation
                            .is_none_or(|stored| stored == entry.generation);
                    drop(existing);
                    if ticket.accepted[entry.index] {
                        install_pending_proof(
                            &entry.key,
                            entry.timestamp,
                            entry.absolute_expires_at_ms,
                            entry.generation,
                        );
                        ticket.authorized_requests[entry.index] = Some(entry.authorization);
                        ticket.authorized_generations[entry.index] = Some(entry.generation);
                    }
                    continue;
                }
                if self.pending_hitl_requests.contains_key(&entry.key)
                    || self.resolved_hitl_requests.contains_key(&entry.key)
                {
                    continue;
                }
                install_pending_proof(
                    &entry.key,
                    entry.timestamp,
                    entry.absolute_expires_at_ms,
                    entry.generation,
                );
                ticket.accepted[entry.index] = true;
                ticket.authorized_requests[entry.index] = Some(entry.authorization);
                ticket.authorized_generations[entry.index] = Some(entry.generation);
            }
            drop(lifecycle_guard);
            ticket._publication_guard = publication_guard;
            return ticket;
        };

        let reducer = match prepare_exact_hitl_lifecycle_reducer(persistence) {
            Ok(reducer) => reducer,
            Err(_) => {
                self.hitl_lifecycle_recovery_healthy
                    .store(false, Ordering::SeqCst);
                drop(publication_guard);
                return ticket;
            },
        };
        let lifecycle_guard = self
            .hitl_lifecycle_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for entry in prepared {
            if entry.absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis() {
                continue;
            }
            let existing = match reducer.existing_record(&entry.key) {
                Ok(existing) => existing,
                Err(_) => {
                    self.hitl_lifecycle_recovery_healthy
                        .store(false, Ordering::SeqCst);
                    break;
                },
            };
            if entry.absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis() {
                continue;
            }
            match existing {
                Some(existing) if existing.state == HITL_REDUCER_APP_PENDING => {
                    if existing.timestamp != entry.timestamp
                        || existing.absolute_expires_at_ms != Some(entry.absolute_expires_at_ms)
                    {
                        continue;
                    }
                    let canonical_generation = entry.generation.canonical();
                    if existing
                        .app_owner_generation
                        .as_deref()
                        .is_some_and(|stored| stored != canonical_generation.as_str())
                    {
                        continue;
                    }
                    if reducer
                        .upsert_app_with_generation(
                            &entry.key,
                            HITL_REDUCER_APP_PENDING,
                            entry.timestamp,
                            entry.absolute_expires_at_ms,
                            None,
                            &canonical_generation,
                        )
                        .is_err()
                    {
                        self.hitl_lifecycle_recovery_healthy
                            .store(false, Ordering::SeqCst);
                        break;
                    }
                    install_pending_proof(
                        &entry.key,
                        entry.timestamp,
                        entry.absolute_expires_at_ms,
                        entry.generation,
                    );
                    ticket.accepted[entry.index] = true;
                    ticket.authorized_requests[entry.index] = Some(entry.authorization);
                    ticket.authorized_generations[entry.index] = Some(entry.generation);
                },
                Some(existing) if existing.state == HITL_REDUCER_APP_RESOLVED => {},
                Some(_) => {},
                None => {
                    self.pending_hitl_requests.remove(&entry.key);
                    self.resolved_hitl_requests.remove(&entry.key);
                    self.app_owner_notification_lifecycle.remove(&entry.key);
                    if !self.append_app_owner_notification_lifecycle_in_repaired_batch(
                        &entry.request,
                        entry.generation,
                        entry.absolute_expires_at_ms,
                    ) {
                        break;
                    }
                    let canonical_generation = entry.generation.canonical();
                    if reducer
                        .upsert_app_with_generation(
                            &entry.key,
                            HITL_REDUCER_APP_PENDING,
                            entry.timestamp,
                            entry.absolute_expires_at_ms,
                            None,
                            &canonical_generation,
                        )
                        .is_err()
                    {
                        self.hitl_lifecycle_recovery_healthy
                            .store(false, Ordering::SeqCst);
                        break;
                    }
                    install_pending_proof(
                        &entry.key,
                        entry.timestamp,
                        entry.absolute_expires_at_ms,
                        entry.generation,
                    );
                    ticket.accepted[entry.index] = true;
                    ticket.authorized_requests[entry.index] = Some(entry.authorization);
                    ticket.authorized_generations[entry.index] = Some(entry.generation);
                },
            }
        }
        drop(lifecycle_guard);
        ticket._publication_guard = publication_guard;
        ticket
    }

    /// Broadcast a request whose durable app-family proof was reconciled by
    /// [`Self::reconcile_app_owner_notification_request_batch`]. This method
    /// performs no disk IO. The crate-private caller must hold the exact live
    /// `UserRequestService` pending marker guard and the matching unconsumed
    /// ticket slot. The ticket retains cross-process publication authority;
    /// the lifecycle mutex makes the final proof/body/expiry check atomic with
    /// lifecycle publication, so a racing resolution either follows this
    /// request or wins and suppresses it. The private event is consumed at the
    /// broadcaster boundary and is never inserted into the shared ring.
    pub(crate) fn broadcast_reconciled_app_owner_notification_request(
        &self,
        ticket: &mut AppOwnerNotificationRequestPublicationTicket,
        index: usize,
        event: RuntimeTransportEvent,
    ) -> bool {
        if !Arc::ptr_eq(&ticket.owner_lifecycle_lock, &self.hitl_lifecycle_lock)
            || (self.hitl_lifecycle_persistence.is_some() && ticket._publication_guard.is_none())
            || !ticket.accepted(index)
        {
            discard_hitl_lifecycle_event_iteratively(event);
            return false;
        }
        if !hitl_lifecycle_event_is_admitted(&event) {
            discard_hitl_lifecycle_event_iteratively(event);
            return false;
        }
        let Some(key) = scoped_hitl_lifecycle_key(&event) else {
            discard_hitl_lifecycle_event_iteratively(event);
            return false;
        };
        let RuntimeTransportEvent::HitlRequested { timestamp, .. } = &event else {
            discard_hitl_lifecycle_event_iteratively(event);
            return false;
        };
        let timestamp = *timestamp;
        let Some(expected_body) = bounded_hitl_lifecycle_event_bytes(&event) else {
            discard_hitl_lifecycle_event_iteratively(event);
            return false;
        };
        if ticket.authorized_requests.get(index).copied().flatten()
            != Some(app_owner_notification_request_authorization(&expected_body))
        {
            discard_hitl_lifecycle_event_iteratively(event);
            return false;
        }
        let Ok(Some(absolute_expires_at_ms)) = app_owner_notification_expiry_ms(&event) else {
            discard_hitl_lifecycle_event_iteratively(event);
            return false;
        };
        let _lifecycle_guard = self
            .hitl_lifecycle_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let authorized_generation = ticket.authorized_generations.get(index).copied().flatten();
        if absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis()
            || self
                .app_owner_notification_lifecycle
                .get(&key)
                .is_none_or(|state| {
                    state.state != HitlLifecycleProofStateV1::PendingRedacted
                        || state.timestamp != timestamp
                        || state.absolute_expires_at_ms != absolute_expires_at_ms
                        || state.app_owner_generation != authorized_generation
                })
        {
            discard_hitl_lifecycle_event_iteratively(event);
            return false;
        }
        if let Some(actual) = self.pending_hitl_requests.get(&key) {
            if bounded_hitl_lifecycle_event_bytes(actual.value()).as_deref()
                != Some(expected_body.as_slice())
            {
                discard_hitl_lifecycle_event_iteratively(event);
                return false;
            }
        }
        // Reconciliation already installed the content-free proof with the
        // exact private generation. The generic recorder would overwrite that
        // generation with `None` and clone the private body into
        // `pending_hitl_requests`, breaking later generation-bound resolution
        // in the in-memory path and creating an unintended second body owner.
        self.broadcast_transport_event(event);
        ticket.accepted[index] = false;
        ticket.authorized_requests[index] = None;
        ticket.authorized_generations[index] = None;
        true
    }

    /// Reconcile one fixed-size page of already-durable app-owner notification
    /// resolutions with one publication lock, tail repair, and shared-journal
    /// reduction. Each entry preserves the single-item contract: only the
    /// exact expiry-bound app family may own the key, an exact V2 resolution
    /// proof is idempotent success, and a missing key appends the content-free
    /// request proof before its resolution under the same authority.
    pub(crate) fn reconcile_app_owner_notification_lifecycle_batch(
        &self,
        lifecycle: Vec<(
            RuntimeTransportEvent,
            RuntimeTransportEvent,
            AppOwnerNotificationPublicationGeneration,
        )>,
    ) -> Vec<bool> {
        let mut accepted = vec![false; lifecycle.len()];
        if lifecycle.is_empty() || lifecycle.len() > HITL_LIFECYCLE_RECONCILE_BATCH {
            return accepted;
        }

        let mut prepared = Vec::with_capacity(lifecycle.len());
        let mut prepared_keys = HashSet::with_capacity(lifecycle.len());
        for (index, (request, resolution, generation)) in lifecycle.into_iter().enumerate() {
            let RuntimeTransportEvent::HitlRequested {
                source: request_source,
                timestamp: request_timestamp,
                ..
            } = &request
            else {
                continue;
            };
            let RuntimeTransportEvent::HitlResolved {
                source: resolution_source,
                timestamp: resolution_timestamp,
                ..
            } = &resolution
            else {
                continue;
            };
            let request_timestamp = *request_timestamp;
            let resolution_timestamp = *resolution_timestamp;
            if request_source != "user_request"
                || resolution_source != "app_owner_notification"
                || !hitl_lifecycle_event_is_admitted(&request)
                || !hitl_lifecycle_event_is_admitted(&resolution)
            {
                continue;
            }
            let Some(key) = scoped_hitl_lifecycle_key(&request) else {
                continue;
            };
            if scoped_hitl_lifecycle_key(&resolution).as_ref() != Some(&key) {
                continue;
            }
            let Ok(Some(absolute_expires_at_ms)) = app_owner_notification_expiry_ms(&request)
            else {
                continue;
            };
            if absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis()
                || absolute_expires_at_ms <= resolution_timestamp
                || resolution_timestamp < request_timestamp
            {
                continue;
            }
            let Some(resolution_fingerprint) = hitl_resolved_fingerprint(&resolution) else {
                continue;
            };
            if !prepared_keys.insert(key.clone()) {
                continue;
            }
            prepared.push(PreparedAppOwnerNotificationLifecycle {
                index,
                key,
                request,
                request_timestamp,
                resolution,
                resolution_timestamp,
                absolute_expires_at_ms,
                resolution_fingerprint,
                generation,
            });
        }
        if prepared.is_empty() {
            return accepted;
        }

        let publication_guard = match self.acquire_hitl_lifecycle_publication_lock() {
            Ok(guard) => guard,
            Err(_) => return accepted,
        };
        let mut broadcasts = Vec::with_capacity(prepared.len());
        let Some(persistence) = self.hitl_lifecycle_persistence.as_ref() else {
            let lifecycle_guard = self
                .hitl_lifecycle_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for entry in prepared {
                if entry.absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis() {
                    continue;
                }
                let resolution_authority =
                    HitlResolvedAuthority::Verified(entry.resolution_fingerprint);
                if let Some(existing) = self.app_owner_notification_lifecycle.get(&entry.key) {
                    match existing.state {
                        HitlLifecycleProofStateV1::Resolved => {
                            accepted[entry.index] = existing.timestamp
                                == entry.resolution_timestamp
                                && existing.absolute_expires_at_ms == entry.absolute_expires_at_ms
                                && existing.resolved_authority == Some(resolution_authority)
                                && existing.app_owner_generation == Some(entry.generation);
                            continue;
                        },
                        HitlLifecycleProofStateV1::PendingRedacted
                            if existing.timestamp == entry.request_timestamp
                                && existing.absolute_expires_at_ms
                                    == entry.absolute_expires_at_ms
                                && existing
                                    .app_owner_generation
                                    .is_none_or(|stored| stored == entry.generation) => {},
                        _ => continue,
                    }
                } else {
                    if self.pending_hitl_requests.contains_key(&entry.key)
                        || self.resolved_hitl_requests.contains_key(&entry.key)
                    {
                        continue;
                    }
                    self.pending_hitl_requests.remove(&entry.key);
                    self.resolved_hitl_requests.remove(&entry.key);
                    self.app_owner_notification_lifecycle.insert(
                        entry.key.clone(),
                        AppOwnerNotificationLifecycleState {
                            state: HitlLifecycleProofStateV1::PendingRedacted,
                            timestamp: entry.request_timestamp,
                            absolute_expires_at_ms: entry.absolute_expires_at_ms,
                            resolved_authority: None,
                            app_owner_generation: Some(entry.generation),
                        },
                    );
                    self.arm_app_owner_notification_expiry(
                        entry.key.clone(),
                        entry.absolute_expires_at_ms,
                    );
                }
                if let Some(mut state) = self.app_owner_notification_lifecycle.get_mut(&entry.key) {
                    state.app_owner_generation = Some(entry.generation);
                }
                if entry.absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis()
                    || !self.append_app_owner_notification_lifecycle_in_repaired_batch(
                        &entry.resolution,
                        entry.generation,
                        entry.absolute_expires_at_ms,
                    )
                {
                    continue;
                }
                self.record_pending_hitl_lifecycle(&entry.resolution);
                accepted[entry.index] = true;
                broadcasts.push(entry.resolution);
            }
            drop(publication_guard);
            for event in broadcasts {
                self.broadcast_transport_event(event);
            }
            drop(lifecycle_guard);
            return accepted;
        };

        let reducer = match prepare_exact_hitl_lifecycle_reducer(persistence) {
            Ok(reducer) => reducer,
            Err(_) => {
                self.hitl_lifecycle_recovery_healthy
                    .store(false, Ordering::SeqCst);
                return accepted;
            },
        };
        let lifecycle_guard = self
            .hitl_lifecycle_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for entry in prepared {
            if entry.absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis() {
                continue;
            }
            let existing = match reducer.existing_record(&entry.key) {
                Ok(existing) => existing,
                Err(_) => {
                    self.hitl_lifecycle_recovery_healthy
                        .store(false, Ordering::SeqCst);
                    break;
                },
            };
            if entry.absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis() {
                continue;
            }
            let resolution_authority =
                HitlResolvedAuthority::Verified(entry.resolution_fingerprint);
            match existing {
                Some(existing) if existing.state == HITL_REDUCER_APP_RESOLVED => {
                    if existing.timestamp != entry.resolution_timestamp
                        || existing.absolute_expires_at_ms != Some(entry.absolute_expires_at_ms)
                        || existing.body.as_deref() != Some(&entry.resolution_fingerprint.0[..])
                        || existing.app_owner_generation.as_deref()
                            != Some(entry.generation.canonical().as_str())
                    {
                        continue;
                    }
                    self.pending_hitl_requests.remove(&entry.key);
                    self.resolved_hitl_requests.remove(&entry.key);
                    self.app_owner_notification_lifecycle.insert(
                        entry.key.clone(),
                        AppOwnerNotificationLifecycleState {
                            state: HitlLifecycleProofStateV1::Resolved,
                            timestamp: entry.resolution_timestamp,
                            absolute_expires_at_ms: entry.absolute_expires_at_ms,
                            resolved_authority: Some(resolution_authority),
                            app_owner_generation: Some(entry.generation),
                        },
                    );
                    self.arm_app_owner_notification_expiry(entry.key, entry.absolute_expires_at_ms);
                    accepted[entry.index] = true;
                },
                Some(existing) if existing.state == HITL_REDUCER_APP_PENDING => {
                    if existing.timestamp != entry.request_timestamp
                        || existing.absolute_expires_at_ms != Some(entry.absolute_expires_at_ms)
                    {
                        continue;
                    }
                    let canonical_generation = entry.generation.canonical();
                    if existing
                        .app_owner_generation
                        .as_deref()
                        .is_some_and(|stored| stored != canonical_generation.as_str())
                    {
                        continue;
                    }
                    if reducer
                        .upsert_app_with_generation(
                            &entry.key,
                            HITL_REDUCER_APP_PENDING,
                            entry.request_timestamp,
                            entry.absolute_expires_at_ms,
                            None,
                            &canonical_generation,
                        )
                        .is_err()
                    {
                        self.hitl_lifecycle_recovery_healthy
                            .store(false, Ordering::SeqCst);
                        break;
                    }
                    self.pending_hitl_requests.remove(&entry.key);
                    self.resolved_hitl_requests.remove(&entry.key);
                    self.app_owner_notification_lifecycle.insert(
                        entry.key.clone(),
                        AppOwnerNotificationLifecycleState {
                            state: HitlLifecycleProofStateV1::PendingRedacted,
                            timestamp: entry.request_timestamp,
                            absolute_expires_at_ms: entry.absolute_expires_at_ms,
                            resolved_authority: None,
                            app_owner_generation: Some(entry.generation),
                        },
                    );
                    self.arm_app_owner_notification_expiry(
                        entry.key.clone(),
                        entry.absolute_expires_at_ms,
                    );
                    if entry.absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis()
                        || !self.append_app_owner_notification_lifecycle_in_repaired_batch(
                            &entry.resolution,
                            entry.generation,
                            entry.absolute_expires_at_ms,
                        )
                    {
                        continue;
                    }
                    self.record_pending_hitl_lifecycle(&entry.resolution);
                    accepted[entry.index] = true;
                    broadcasts.push(entry.resolution);
                    if reducer
                        .upsert(
                            &entry.key,
                            HITL_REDUCER_APP_RESOLVED,
                            entry.resolution_timestamp,
                            Some(entry.absolute_expires_at_ms),
                            Some(&entry.resolution_fingerprint.0),
                        )
                        .is_err()
                    {
                        self.hitl_lifecycle_recovery_healthy
                            .store(false, Ordering::SeqCst);
                        break;
                    }
                },
                Some(_) => {},
                None => {
                    self.pending_hitl_requests.remove(&entry.key);
                    self.resolved_hitl_requests.remove(&entry.key);
                    self.app_owner_notification_lifecycle.remove(&entry.key);
                    if !self.append_app_owner_notification_lifecycle_in_repaired_batch(
                        &entry.request,
                        entry.generation,
                        entry.absolute_expires_at_ms,
                    ) {
                        break;
                    }
                    if reducer
                        .upsert_app_with_generation(
                            &entry.key,
                            HITL_REDUCER_APP_PENDING,
                            entry.request_timestamp,
                            entry.absolute_expires_at_ms,
                            None,
                            &entry.generation.canonical(),
                        )
                        .is_err()
                    {
                        self.hitl_lifecycle_recovery_healthy
                            .store(false, Ordering::SeqCst);
                        break;
                    }
                    self.app_owner_notification_lifecycle.insert(
                        entry.key.clone(),
                        AppOwnerNotificationLifecycleState {
                            state: HitlLifecycleProofStateV1::PendingRedacted,
                            timestamp: entry.request_timestamp,
                            absolute_expires_at_ms: entry.absolute_expires_at_ms,
                            resolved_authority: None,
                            app_owner_generation: Some(entry.generation),
                        },
                    );
                    self.arm_app_owner_notification_expiry(
                        entry.key.clone(),
                        entry.absolute_expires_at_ms,
                    );
                    if entry.absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis()
                        || !self.append_app_owner_notification_lifecycle_in_repaired_batch(
                            &entry.resolution,
                            entry.generation,
                            entry.absolute_expires_at_ms,
                        )
                    {
                        continue;
                    }
                    self.record_pending_hitl_lifecycle(&entry.resolution);
                    accepted[entry.index] = true;
                    broadcasts.push(entry.resolution);
                    if reducer
                        .upsert(
                            &entry.key,
                            HITL_REDUCER_APP_RESOLVED,
                            entry.resolution_timestamp,
                            Some(entry.absolute_expires_at_ms),
                            Some(&entry.resolution_fingerprint.0),
                        )
                        .is_err()
                    {
                        self.hitl_lifecycle_recovery_healthy
                            .store(false, Ordering::SeqCst);
                        break;
                    }
                },
            }
        }
        drop(publication_guard);
        for event in broadcasts {
            self.broadcast_transport_event(event);
        }
        drop(lifecycle_guard);
        accepted
    }

    /// Reconcile one fixed-size page of already-durable generic UserRequest
    /// requested-event debts against exact shared authority. A same-key pending
    /// event succeeds only when its complete canonical bytes match. Resolved,
    /// app-family, or different-body authority is preserved and refused; an
    /// absent key is durably appended at most once.
    pub fn reconcile_generic_hitl_request_batch(
        &self,
        requests: Vec<RuntimeTransportEvent>,
    ) -> Vec<bool> {
        let mut accepted = vec![false; requests.len()];
        if requests.is_empty() || requests.len() > HITL_LIFECYCLE_RECONCILE_BATCH {
            return accepted;
        }
        let mut prepared = Vec::with_capacity(requests.len());
        for (index, request) in requests.into_iter().enumerate() {
            let RuntimeTransportEvent::HitlRequested {
                source, timestamp, ..
            } = &request
            else {
                continue;
            };
            let timestamp = *timestamp;
            if source != "user_request"
                || !hitl_lifecycle_event_is_admitted(&request)
                || app_owner_notification_expiry_ms(&request) != Ok(None)
            {
                continue;
            }
            let Some(key) = scoped_hitl_lifecycle_key(&request) else {
                continue;
            };
            let Some(body) = bounded_hitl_lifecycle_event_bytes(&request) else {
                continue;
            };
            prepared.push(PreparedGenericHitlRequest {
                index,
                key,
                request,
                timestamp,
                body,
            });
        }
        if prepared.is_empty() {
            return accepted;
        }

        let publication_guard = match self.acquire_hitl_lifecycle_publication_lock() {
            Ok(guard) => guard,
            Err(_) => return accepted,
        };
        let mut broadcasts = Vec::with_capacity(prepared.len());
        let Some(persistence) = self.hitl_lifecycle_persistence.as_ref() else {
            let lifecycle_guard = self
                .hitl_lifecycle_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for entry in prepared {
                if self
                    .app_owner_notification_lifecycle
                    .contains_key(&entry.key)
                    || self.resolved_hitl_requests.contains_key(&entry.key)
                {
                    continue;
                }
                if let Some(existing) = self.pending_hitl_requests.get(&entry.key) {
                    accepted[entry.index] = bounded_hitl_lifecycle_event_bytes(existing.value())
                        .as_deref()
                        == Some(entry.body.as_slice());
                    continue;
                }
                self.record_pending_hitl_lifecycle(&entry.request);
                accepted[entry.index] = true;
                broadcasts.push(entry.request);
            }
            drop(publication_guard);
            for event in broadcasts {
                self.broadcast_transport_event(event);
            }
            drop(lifecycle_guard);
            return accepted;
        };

        let reducer = match prepare_exact_hitl_lifecycle_reducer(persistence) {
            Ok(reducer) => reducer,
            Err(_) => {
                self.hitl_lifecycle_recovery_healthy
                    .store(false, Ordering::SeqCst);
                return accepted;
            },
        };
        let lifecycle_guard = self
            .hitl_lifecycle_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for entry in prepared {
            let existing = match reducer.existing_record(&entry.key) {
                Ok(existing) => existing,
                Err(_) => {
                    self.hitl_lifecycle_recovery_healthy
                        .store(false, Ordering::SeqCst);
                    break;
                },
            };
            match existing {
                Some(existing) if existing.state == HITL_REDUCER_GENERIC_PENDING => {
                    if existing.timestamp != entry.timestamp
                        || existing.absolute_expires_at_ms.is_some()
                        || existing.body.as_deref() != Some(entry.body.as_slice())
                    {
                        continue;
                    }
                    self.app_owner_notification_lifecycle.remove(&entry.key);
                    self.resolved_hitl_requests.remove(&entry.key);
                    self.record_pending_hitl_lifecycle(&entry.request);
                    accepted[entry.index] = true;
                },
                Some(_) => {},
                None => {
                    self.pending_hitl_requests.remove(&entry.key);
                    self.resolved_hitl_requests.remove(&entry.key);
                    self.app_owner_notification_lifecycle.remove(&entry.key);
                    if !self.append_hitl_lifecycle_journal_in_repaired_batch(&entry.request) {
                        break;
                    }
                    self.record_pending_hitl_lifecycle(&entry.request);
                    accepted[entry.index] = true;
                    broadcasts.push(entry.request);
                    if reducer
                        .upsert(
                            &entry.key,
                            HITL_REDUCER_GENERIC_PENDING,
                            entry.timestamp,
                            None,
                            Some(&entry.body),
                        )
                        .is_err()
                    {
                        self.hitl_lifecycle_recovery_healthy
                            .store(false, Ordering::SeqCst);
                        break;
                    }
                },
            }
        }
        drop(publication_guard);
        for event in broadcasts {
            self.broadcast_transport_event(event);
        }
        drop(lifecycle_guard);
        accepted
    }

    /// Reconcile one fixed-size page of already-durable generic UserRequest
    /// lifecycle debts against the shared journal. Each entry carries both the
    /// exact request and its resolution: a missing key is recovered by
    /// publishing request then resolution under the same cross-process lock,
    /// while an existing pending row must byte-match the reconstructed request.
    /// An exact V2 resolved proof is an idempotent receipt; app-family or
    /// differently sealed same-key authority is never consumed.
    pub fn reconcile_generic_hitl_resolution_batch(
        &self,
        lifecycle: Vec<(RuntimeTransportEvent, RuntimeTransportEvent)>,
    ) -> Vec<bool> {
        let mut accepted = vec![false; lifecycle.len()];
        if lifecycle.is_empty() || lifecycle.len() > HITL_LIFECYCLE_RECONCILE_BATCH {
            return accepted;
        }

        let mut prepared = Vec::with_capacity(lifecycle.len());
        for (index, (request, resolution)) in lifecycle.into_iter().enumerate() {
            let RuntimeTransportEvent::HitlRequested {
                source: request_source,
                timestamp: request_timestamp,
                ..
            } = &request
            else {
                continue;
            };
            let RuntimeTransportEvent::HitlResolved {
                source: resolution_source,
                timestamp: resolution_timestamp,
                ..
            } = &resolution
            else {
                continue;
            };
            let request_timestamp = *request_timestamp;
            let resolution_timestamp = *resolution_timestamp;
            if request_source != "user_request"
                || resolution_source != "user_request"
                || !hitl_lifecycle_event_is_admitted(&request)
                || !hitl_lifecycle_event_is_admitted(&resolution)
                || app_owner_notification_expiry_ms(&request) != Ok(None)
                || resolution_timestamp < request_timestamp
            {
                continue;
            }
            let Some(key) = scoped_hitl_lifecycle_key(&request) else {
                continue;
            };
            if scoped_hitl_lifecycle_key(&resolution).as_ref() != Some(&key) {
                continue;
            }
            let Some(request_body) = bounded_hitl_lifecycle_event_bytes(&request) else {
                continue;
            };
            let Some(resolution_fingerprint) = hitl_resolved_fingerprint(&resolution) else {
                continue;
            };
            prepared.push(PreparedGenericHitlResolution {
                index,
                key,
                request,
                request_timestamp,
                request_body,
                resolution,
                resolution_timestamp,
                resolution_fingerprint,
            });
        }
        if prepared.is_empty() {
            return accepted;
        }

        let publication_guard = match self.acquire_hitl_lifecycle_publication_lock() {
            Ok(guard) => guard,
            Err(_) => return accepted,
        };
        let mut broadcasts = Vec::with_capacity(prepared.len().saturating_mul(2));

        let Some(persistence) = self.hitl_lifecycle_persistence.as_ref() else {
            let lifecycle_guard = self
                .hitl_lifecycle_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for entry in prepared {
                if self
                    .app_owner_notification_lifecycle
                    .contains_key(&entry.key)
                {
                    continue;
                }
                if let Some(existing) = self.resolved_hitl_requests.get(&entry.key) {
                    accepted[entry.index] = existing.timestamp == entry.resolution_timestamp
                        && existing.authority
                            == HitlResolvedAuthority::Verified(entry.resolution_fingerprint);
                    continue;
                }
                if let Some(existing) = self.pending_hitl_requests.get(&entry.key) {
                    if bounded_hitl_lifecycle_event_bytes(existing.value()).as_deref()
                        != Some(entry.request_body.as_slice())
                    {
                        continue;
                    }
                } else {
                    self.record_pending_hitl_lifecycle(&entry.request);
                    broadcasts.push(entry.request);
                }
                self.record_pending_hitl_lifecycle(&entry.resolution);
                accepted[entry.index] = true;
                broadcasts.push(entry.resolution);
            }
            drop(publication_guard);
            for event in broadcasts {
                self.broadcast_transport_event(event);
            }
            drop(lifecycle_guard);
            return accepted;
        };

        let reducer = match prepare_exact_hitl_lifecycle_reducer(persistence) {
            Ok(reducer) => reducer,
            Err(_) => {
                self.hitl_lifecycle_recovery_healthy
                    .store(false, Ordering::SeqCst);
                return accepted;
            },
        };
        let lifecycle_guard = self
            .hitl_lifecycle_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        for entry in prepared {
            let existing = match reducer.existing_record(&entry.key) {
                Ok(existing) => existing,
                Err(_) => {
                    self.hitl_lifecycle_recovery_healthy
                        .store(false, Ordering::SeqCst);
                    break;
                },
            };
            match existing {
                Some(existing) if existing.state == HITL_REDUCER_GENERIC_RESOLVED => {
                    if existing.timestamp != entry.resolution_timestamp
                        || existing.absolute_expires_at_ms.is_some()
                        || existing.body.as_deref() != Some(&entry.resolution_fingerprint.0[..])
                    {
                        continue;
                    }
                    self.pending_hitl_requests.remove(&entry.key);
                    self.app_owner_notification_lifecycle.remove(&entry.key);
                    self.resolved_hitl_requests.insert(
                        entry.key,
                        GenericHitlResolvedState {
                            timestamp: entry.resolution_timestamp,
                            authority: HitlResolvedAuthority::Verified(
                                entry.resolution_fingerprint,
                            ),
                        },
                    );
                    accepted[entry.index] = true;
                },
                Some(existing) if existing.state == HITL_REDUCER_GENERIC_PENDING => {
                    if existing.timestamp != entry.request_timestamp
                        || existing.absolute_expires_at_ms.is_some()
                        || existing.body.as_deref() != Some(entry.request_body.as_slice())
                    {
                        continue;
                    }
                    self.app_owner_notification_lifecycle.remove(&entry.key);
                    self.resolved_hitl_requests.remove(&entry.key);
                    self.record_pending_hitl_lifecycle(&entry.request);
                    if !self.append_hitl_lifecycle_journal_in_repaired_batch(&entry.resolution) {
                        break;
                    }
                    self.record_pending_hitl_lifecycle(&entry.resolution);
                    accepted[entry.index] = true;
                    broadcasts.push(entry.resolution);
                    if reducer
                        .upsert(
                            &entry.key,
                            HITL_REDUCER_GENERIC_RESOLVED,
                            entry.resolution_timestamp,
                            None,
                            Some(&entry.resolution_fingerprint.0),
                        )
                        .is_err()
                    {
                        self.hitl_lifecycle_recovery_healthy
                            .store(false, Ordering::SeqCst);
                        break;
                    }
                },
                Some(_) => {},
                None => {
                    self.pending_hitl_requests.remove(&entry.key);
                    self.resolved_hitl_requests.remove(&entry.key);
                    self.app_owner_notification_lifecycle.remove(&entry.key);
                    if !self.append_hitl_lifecycle_journal_in_repaired_batch(&entry.request) {
                        break;
                    }
                    self.record_pending_hitl_lifecycle(&entry.request);
                    broadcasts.push(entry.request);
                    if reducer
                        .upsert(
                            &entry.key,
                            HITL_REDUCER_GENERIC_PENDING,
                            entry.request_timestamp,
                            None,
                            Some(&entry.request_body),
                        )
                        .is_err()
                    {
                        self.hitl_lifecycle_recovery_healthy
                            .store(false, Ordering::SeqCst);
                        break;
                    }
                    if !self.append_hitl_lifecycle_journal_in_repaired_batch(&entry.resolution) {
                        break;
                    }
                    self.record_pending_hitl_lifecycle(&entry.resolution);
                    accepted[entry.index] = true;
                    broadcasts.push(entry.resolution);
                    if reducer
                        .upsert(
                            &entry.key,
                            HITL_REDUCER_GENERIC_RESOLVED,
                            entry.resolution_timestamp,
                            None,
                            Some(&entry.resolution_fingerprint.0),
                        )
                        .is_err()
                    {
                        self.hitl_lifecycle_recovery_healthy
                            .store(false, Ordering::SeqCst);
                        break;
                    }
                },
            }
        }
        drop(publication_guard);
        for event in broadcasts {
            self.broadcast_transport_event(event);
        }
        drop(lifecycle_guard);
        accepted
    }

    /// Reconcile one scoped lifecycle event against process-local authority
    /// when no durable journal is configured. The second tuple member reports
    /// whether this call created a new lifecycle fact and should broadcast it.
    fn reconcile_scoped_hitl_without_persistence(
        &self,
        event: &RuntimeTransportEvent,
    ) -> (bool, bool) {
        let Some(key) = scoped_hitl_lifecycle_key(event) else {
            return (false, false);
        };
        match event {
            RuntimeTransportEvent::HitlRequested { timestamp, .. } => {
                match app_owner_notification_expiry_ms(event) {
                    Ok(Some(absolute_expires_at_ms)) => {
                        if absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis()
                            || self.resolved_hitl_requests.contains_key(&key)
                        {
                            return (false, false);
                        }
                        if let Some(existing) = self.app_owner_notification_lifecycle.get(&key) {
                            return (
                                existing.state == HitlLifecycleProofStateV1::PendingRedacted
                                    && existing.app_owner_generation.is_none()
                                    && existing.timestamp == *timestamp
                                    && existing.absolute_expires_at_ms == absolute_expires_at_ms,
                                false,
                            );
                        }
                        if self.pending_hitl_requests.contains_key(&key) {
                            return (false, false);
                        }
                    },
                    Ok(None) => {
                        if self.app_owner_notification_lifecycle.contains_key(&key) {
                            return (false, false);
                        }
                        // As in the persisted reducer: a run's next question
                        // reuses the resolved key and is admitted when it is
                        // newer than the resolution; a replay of the resolved
                        // request is not.
                        if let Some(resolved) = self.resolved_hitl_requests.get(&key) {
                            if *timestamp <= resolved.timestamp {
                                return (false, false);
                            }
                        }
                        if let Some(existing) = self.pending_hitl_requests.get(&key) {
                            let exact = bounded_hitl_lifecycle_event_bytes(existing.value())
                                .as_deref()
                                == bounded_hitl_lifecycle_event_bytes(event).as_deref();
                            return (exact, false);
                        }
                    },
                    Err(_) => return (false, false),
                }
                let appended = self.append_hitl_lifecycle_journal(event);
                (appended, appended)
            },
            RuntimeTransportEvent::HitlResolved {
                source, timestamp, ..
            } => {
                let Some(fingerprint) = hitl_resolved_fingerprint(event) else {
                    return (false, false);
                };
                let authority = HitlResolvedAuthority::Verified(fingerprint);
                if source == "app_owner_notification" {
                    let Some(existing) = self.app_owner_notification_lifecycle.get(&key) else {
                        return (false, false);
                    };
                    if existing.app_owner_generation.is_some() {
                        return (false, false);
                    }
                    if self.resolved_hitl_requests.contains_key(&key) {
                        return (false, false);
                    }
                    match existing.state {
                        HitlLifecycleProofStateV1::Resolved => (
                            existing.timestamp == *timestamp
                                && existing.absolute_expires_at_ms
                                    > chrono::Utc::now().timestamp_millis()
                                && existing.resolved_authority == Some(authority),
                            false,
                        ),
                        HitlLifecycleProofStateV1::PendingRedacted
                            if existing.absolute_expires_at_ms
                                > chrono::Utc::now().timestamp_millis()
                                && *timestamp >= existing.timestamp
                                && *timestamp < existing.absolute_expires_at_ms =>
                        {
                            drop(existing);
                            let appended = self.append_hitl_lifecycle_journal(event);
                            (appended, appended)
                        },
                        _ => (false, false),
                    }
                } else {
                    if self.app_owner_notification_lifecycle.contains_key(&key) {
                        return (false, false);
                    }
                    if let Some(existing) = self.resolved_hitl_requests.get(&key) {
                        return (
                            existing.timestamp == *timestamp && existing.authority == authority,
                            false,
                        );
                    }
                    let Some(existing) = self.pending_hitl_requests.get(&key) else {
                        return (false, false);
                    };
                    let RuntimeTransportEvent::HitlRequested {
                        timestamp: request_timestamp,
                        ..
                    } = existing.value()
                    else {
                        return (false, false);
                    };
                    if *timestamp < *request_timestamp {
                        return (false, false);
                    }
                    drop(existing);
                    let appended = self.append_hitl_lifecycle_journal(event);
                    (appended, appended)
                }
            },
            _ => (false, false),
        }
    }

    /// Reconcile one scoped lifecycle event against exact durable authority
    /// while the caller holds cross-process publication authority.
    /// The returned append bit prevents idempotent retries from rebroadcasting.
    fn reconcile_scoped_hitl_with_persistence(
        &self,
        event: &RuntimeTransportEvent,
    ) -> (bool, bool) {
        let Some(persistence) = self.hitl_lifecycle_persistence.as_ref() else {
            return self.reconcile_scoped_hitl_without_persistence(event);
        };
        let reducer = match prepare_exact_hitl_lifecycle_reducer(persistence) {
            Ok(reducer) => reducer,
            Err(_) => return (false, false),
        };
        let Some(key) = scoped_hitl_lifecycle_key(event) else {
            return (false, false);
        };
        let existing = match reducer.existing_record(&key) {
            Ok(existing) => existing,
            Err(_) => return (false, false),
        };

        // Cross-process publication authority keeps the reduction stable. The
        // process-local mutex is needed only for the final exact install,
        // append, and projection update, not the aggregate disk scan above.
        let _lifecycle_guard = self
            .hitl_lifecycle_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let outcome = match event {
            RuntimeTransportEvent::HitlRequested { timestamp, .. } => {
                match app_owner_notification_expiry_ms(event) {
                    Ok(Some(absolute_expires_at_ms)) => {
                        if persistence.authority_path.is_some() {
                            // V3 app authority is valid only when the durable
                            // UserRequest body owner supplies its opaque
                            // generation through the token-aware APIs.
                            return (false, false);
                        }
                        if absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis() {
                            return (false, false);
                        }
                        match existing {
                            Some(existing) if existing.state == HITL_REDUCER_APP_PENDING => (
                                existing.timestamp == *timestamp
                                    && existing.absolute_expires_at_ms
                                        == Some(absolute_expires_at_ms),
                                false,
                            ),
                            Some(_) => (false, false),
                            None => {
                                self.pending_hitl_requests.remove(&key);
                                self.resolved_hitl_requests.remove(&key);
                                self.app_owner_notification_lifecycle.remove(&key);
                                let appended =
                                    self.append_hitl_lifecycle_journal_in_repaired_batch(event);
                                (appended, appended)
                            },
                        }
                    },
                    Ok(None) => {
                        let Some(body) = bounded_hitl_lifecycle_event_bytes(event) else {
                            return (false, false);
                        };
                        match existing {
                            Some(existing) if existing.state == HITL_REDUCER_GENERIC_PENDING => (
                                existing.timestamp == *timestamp
                                    && existing.absolute_expires_at_ms.is_none()
                                    && existing.body.as_deref() == Some(body.as_slice()),
                                false,
                            ),
                            // A resolved key asked again, later: a run's next
                            // question (every pause of a run shares its pause
                            // key, so every ask after the first lands here).
                            // A request not newer than the resolution is the
                            // resolved one replayed, and stays refused.
                            Some(existing)
                                if existing.state == HITL_REDUCER_GENERIC_RESOLVED
                                    && *timestamp > existing.timestamp =>
                            {
                                self.pending_hitl_requests.remove(&key);
                                self.resolved_hitl_requests.remove(&key);
                                self.app_owner_notification_lifecycle.remove(&key);
                                let appended =
                                    self.append_hitl_lifecycle_journal_in_repaired_batch(event);
                                (appended, appended)
                            },
                            Some(_) => (false, false),
                            None => {
                                self.pending_hitl_requests.remove(&key);
                                self.resolved_hitl_requests.remove(&key);
                                self.app_owner_notification_lifecycle.remove(&key);
                                let appended =
                                    self.append_hitl_lifecycle_journal_in_repaired_batch(event);
                                (appended, appended)
                            },
                        }
                    },
                    Err(_) => (false, false),
                }
            },
            RuntimeTransportEvent::HitlResolved {
                source, timestamp, ..
            } => {
                let Some(fingerprint) = hitl_resolved_fingerprint(event) else {
                    return (false, false);
                };
                let authority = HitlResolvedAuthority::Verified(fingerprint);
                if source == "app_owner_notification" {
                    if persistence.authority_path.is_some() {
                        return (false, false);
                    }
                    match existing {
                        Some(existing) if existing.state == HITL_REDUCER_APP_RESOLVED => {
                            let Some(absolute_expires_at_ms) = existing.absolute_expires_at_ms
                            else {
                                return (false, false);
                            };
                            if absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis()
                                || existing.timestamp != *timestamp
                                || existing.body.as_deref() != Some(&fingerprint.0[..])
                            {
                                return (false, false);
                            }
                            self.pending_hitl_requests.remove(&key);
                            self.resolved_hitl_requests.remove(&key);
                            self.app_owner_notification_lifecycle.insert(
                                key.clone(),
                                AppOwnerNotificationLifecycleState {
                                    state: HitlLifecycleProofStateV1::Resolved,
                                    timestamp: *timestamp,
                                    absolute_expires_at_ms,
                                    resolved_authority: Some(authority),
                                    app_owner_generation: None,
                                },
                            );
                            self.arm_app_owner_notification_expiry(key, absolute_expires_at_ms);
                            (true, false)
                        },
                        Some(existing) if existing.state == HITL_REDUCER_APP_PENDING => {
                            let Some(absolute_expires_at_ms) = existing.absolute_expires_at_ms
                            else {
                                return (false, false);
                            };
                            if absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis()
                                || *timestamp < existing.timestamp
                                || *timestamp >= absolute_expires_at_ms
                            {
                                return (false, false);
                            }
                            self.pending_hitl_requests.remove(&key);
                            self.resolved_hitl_requests.remove(&key);
                            self.app_owner_notification_lifecycle.insert(
                                key.clone(),
                                AppOwnerNotificationLifecycleState {
                                    state: HitlLifecycleProofStateV1::PendingRedacted,
                                    timestamp: existing.timestamp,
                                    absolute_expires_at_ms,
                                    resolved_authority: None,
                                    app_owner_generation: None,
                                },
                            );
                            self.arm_app_owner_notification_expiry(
                                key.clone(),
                                absolute_expires_at_ms,
                            );
                            let appended =
                                self.append_hitl_lifecycle_journal_in_repaired_batch(event);
                            (appended, appended)
                        },
                        Some(_) | None => (false, false),
                    }
                } else {
                    match existing {
                        Some(existing) if existing.state == HITL_REDUCER_GENERIC_RESOLVED => (
                            existing.timestamp == *timestamp
                                && existing.absolute_expires_at_ms.is_none()
                                && existing.body.as_deref() == Some(&fingerprint.0[..]),
                            false,
                        ),
                        Some(existing) if existing.state == HITL_REDUCER_GENERIC_PENDING => {
                            if *timestamp < existing.timestamp {
                                return (false, false);
                            }
                            self.pending_hitl_requests.remove(&key);
                            self.resolved_hitl_requests.remove(&key);
                            self.app_owner_notification_lifecycle.remove(&key);
                            let appended =
                                self.append_hitl_lifecycle_journal_in_repaired_batch(event);
                            (appended, appended)
                        },
                        Some(_) | None => (false, false),
                    }
                }
            },
            _ => (false, false),
        };
        if outcome.0 {
            self.record_pending_hitl_lifecycle(event);
        }
        outcome
    }

    /// Called only while the lifecycle publication ordering guard is held.
    /// A request proof may have become durable just before its sealed body
    /// deadline elapsed; retain the content-free proof for compaction but do
    /// not let the body cross either live publication rail afterward.
    fn redact_app_owner_notification_request_if_expired(
        &self,
        event: &RuntimeTransportEvent,
    ) -> bool {
        if !matches!(event, RuntimeTransportEvent::HitlRequested { .. })
            || !matches!(
                app_owner_notification_expiry_ms(event),
                Ok(Some(expiry)) if expiry <= chrono::Utc::now().timestamp_millis()
            )
        {
            return false;
        }
        if let Some(key) = scoped_hitl_lifecycle_key(event) {
            self.pending_hitl_requests.remove(&key);
        }
        true
    }

    fn emit_transport_only_if_accepted(&self, event: RuntimeTransportEvent) -> bool {
        let event = self.sanitize_chat_message_presentation_for_transport(event);
        let event = self.enrich_scope_if_registered(event);
        let scoped_hitl = scoped_hitl_lifecycle_event(&event);
        if hitl_lifecycle_event(&event)
            && (!hitl_lifecycle_event_is_admitted(&event)
                || (!scoped_hitl && !hitl_lifecycle_wire_size_is_admitted(&event)))
        {
            error!(
                max_record_depth = MAX_RETAINED_JSON_DEPTH,
                max_record_nodes = MAX_HITL_LIFECYCLE_RECORD_NODES,
                "[MAGICIAN-RUNTIME-EVENTS] Rejected unbounded HITL lifecycle before transport"
            );
            discard_hitl_lifecycle_event_iteratively(event);
            return false;
        }
        if scoped_hitl {
            let _publication_guard = match self.acquire_hitl_lifecycle_publication_lock() {
                Ok(guard) => guard,
                Err(lock_error) => {
                    let _lifecycle_guard = self
                        .hitl_lifecycle_lock
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    self.settle_app_owner_notification_after_append_failure(&event);
                    error!(
                        error = %lock_error,
                        "[MAGICIAN-RUNTIME-EVENTS] Failed to acquire HITL lifecycle publication authority; event withheld"
                    );
                    discard_hitl_lifecycle_event_iteratively(event);
                    return false;
                },
            };
            let mut lifecycle_guard = self.hitl_lifecycle_persistence.is_none().then(|| {
                self.hitl_lifecycle_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            });
            let (accepted, appended) = if self.hitl_lifecycle_persistence.is_some() {
                self.reconcile_scoped_hitl_with_persistence(&event)
            } else {
                self.reconcile_scoped_hitl_without_persistence(&event)
            };
            if !accepted {
                discard_hitl_lifecycle_event_iteratively(event);
                return false;
            }
            if self.hitl_lifecycle_persistence.is_none() {
                self.record_pending_hitl_lifecycle(&event);
            } else {
                lifecycle_guard = Some(
                    self.hitl_lifecycle_lock
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                );
            }
            if self.redact_app_owner_notification_request_if_expired(&event) {
                drop(_publication_guard);
                discard_hitl_lifecycle_event_iteratively(event);
                drop(lifecycle_guard);
                return true;
            }
            drop(_publication_guard);
            if appended {
                self.broadcast_transport_event(event);
            } else {
                discard_hitl_lifecycle_event_iteratively(event);
            }
            drop(lifecycle_guard);
            return true;
        }
        self.broadcast_transport_event(event);
        true
    }

    fn broadcast_transport_event(&self, event: RuntimeTransportEvent) {
        self.observe_provider_health(&event);
        if is_app_owner_notification_transport_event(&event) {
            match &event {
                RuntimeTransportEvent::HitlRequested {
                    correlation_id,
                    source,
                    timestamp,
                    ..
                } => debug!(
                    event_type = "HitlRequested",
                    %correlation_id,
                    %source,
                    %timestamp,
                    "[MAGICIAN-RUNTIME-EVENTS] Withholding private app owner notification lifecycle from shared transport"
                ),
                RuntimeTransportEvent::HitlResolved {
                    correlation_id,
                    source,
                    outcome,
                    timestamp,
                    ..
                } => debug!(
                    event_type = "HitlResolved",
                    %correlation_id,
                    %source,
                    %outcome,
                    %timestamp,
                    "[MAGICIAN-RUNTIME-EVENTS] Withholding private app owner notification lifecycle from shared transport"
                ),
                _ => {},
            }
            // The durable UserRequest owner is the only consumer authorized to
            // retain or return an app-owner notification body. These events used
            // to enter the common Tokio broadcast ring and relied on every
            // subscriber filtering them after receipt. Apart from consuming
            // shared capacity, that made privacy depend on all future direct
            // subscribers remembering the predicate. Lifecycle reconciliation
            // has already completed before this point, and the owner surfaces
            // read UserRequestService directly, so consume the transport value
            // here rather than handing a private clone to generic subscribers.
            discard_hitl_lifecycle_event_iteratively(event);
            return;
        }

        debug!(
            "[MAGICIAN-RUNTIME-EVENTS] Broadcasting transport event: {:?}",
            event
        );

        // Send to all subscribers (errors indicate no active subscribers, which is OK)
        match self.sender.send(event) {
            Ok(receiver_count) => {
                if receiver_count > 0 {
                    debug!(
                        "[MAGICIAN-RUNTIME-EVENTS] Event sent to {} receiver(s)",
                        receiver_count
                    );
                }
            },
            Err(_) => {
                debug!("[MAGICIAN-RUNTIME-EVENTS] No active receivers for event");
            },
        }
    }

    fn record_pending_hitl_lifecycle(&self, event: &RuntimeTransportEvent) {
        let Some(key) = scoped_hitl_lifecycle_key(event) else {
            return;
        };
        match event {
            RuntimeTransportEvent::HitlRequested { timestamp, .. } => {
                self.resolved_hitl_requests.remove(&key);
                match app_owner_notification_expiry_ms(event) {
                    Ok(Some(absolute_expires_at_ms)) => {
                        self.app_owner_notification_lifecycle.insert(
                            key.clone(),
                            AppOwnerNotificationLifecycleState {
                                state: HitlLifecycleProofStateV1::PendingRedacted,
                                timestamp: *timestamp,
                                absolute_expires_at_ms,
                                resolved_authority: None,
                                app_owner_generation: None,
                            },
                        );
                        self.arm_app_owner_notification_expiry(key.clone(), absolute_expires_at_ms);
                    },
                    Ok(None) => {
                        self.app_owner_notification_lifecycle.remove(&key);
                    },
                    Err(_) => return,
                }
                if let Some(retained) = clone_hitl_lifecycle_event_iteratively(event) {
                    self.pending_hitl_requests.insert(key, retained);
                }
            },
            RuntimeTransportEvent::HitlResolved {
                source, timestamp, ..
            } => {
                let authority = hitl_resolved_fingerprint(event)
                    .map(HitlResolvedAuthority::Verified)
                    .unwrap_or(HitlResolvedAuthority::Unverified);
                self.pending_hitl_requests.remove(&key);
                if let Some(mut app_state) = self.app_owner_notification_lifecycle.get_mut(&key) {
                    if app_state.absolute_expires_at_ms > chrono::Utc::now().timestamp_millis() {
                        app_state.state = HitlLifecycleProofStateV1::Resolved;
                        app_state.timestamp = *timestamp;
                        app_state.resolved_authority = Some(authority);
                        self.resolved_hitl_requests.remove(&key);
                    } else {
                        drop(app_state);
                        self.app_owner_notification_lifecycle.remove(&key);
                        self.resolved_hitl_requests.remove(&key);
                    }
                } else if source != "app_owner_notification" {
                    self.resolved_hitl_requests.insert(
                        key,
                        GenericHitlResolvedState {
                            timestamp: *timestamp,
                            authority,
                        },
                    );
                }
            },
            _ => {},
        }
    }

    /// UserRequest resolution commits before publishing HitlResolved. If the
    /// lifecycle append is rejected afterward, the existing journal already
    /// contains only a redacted, expiry-bound proof. Evict the same-process
    /// full prompt and retain a content-free resolved marker so exact lookup
    /// cannot reopen it while persistence recovers.
    fn settle_app_owner_notification_after_append_failure(
        &self,
        event: &RuntimeTransportEvent,
    ) -> bool {
        let RuntimeTransportEvent::HitlResolved {
            source, timestamp, ..
        } = event
        else {
            return false;
        };
        if source != "app_owner_notification" {
            return false;
        }
        let Some(key) = scoped_hitl_lifecycle_key(event) else {
            return false;
        };
        if !self.app_owner_notification_lifecycle.contains_key(&key) {
            return false;
        }
        self.pending_hitl_requests.remove(&key);
        self.resolved_hitl_requests.remove(&key);
        let Some(mut state) = self.app_owner_notification_lifecycle.get_mut(&key) else {
            return false;
        };
        if state.absolute_expires_at_ms > chrono::Utc::now().timestamp_millis() {
            state.state = HitlLifecycleProofStateV1::Resolved;
            state.timestamp = *timestamp;
            state.resolved_authority = hitl_resolved_fingerprint(event)
                .map(HitlResolvedAuthority::Verified)
                .or(Some(HitlResolvedAuthority::Unverified));
        } else {
            drop(state);
            self.app_owner_notification_lifecycle.remove(&key);
        }
        true
    }

    /// Replay never reconstructs app notification content from the lifecycle
    /// journal. Legacy full rows are reduced to a redacted proof in memory and
    /// the startup compactor removes their body before producers are admitted.
    fn record_replayed_hitl_lifecycle(&self, event: RuntimeTransportEvent) -> bool {
        let Some(key) = scoped_hitl_lifecycle_key(&event) else {
            return false;
        };
        match &event {
            RuntimeTransportEvent::HitlRequested { timestamp, .. } => {
                match app_owner_notification_expiry_ms(&event) {
                    Ok(Some(absolute_expires_at_ms)) => {
                        let Some(expected_body) = bounded_hitl_lifecycle_event_bytes(&event) else {
                            self.hitl_lifecycle_recovery_healthy
                                .store(false, Ordering::SeqCst);
                            return true;
                        };
                        if self.resolved_hitl_requests.contains_key(&key)
                            || self
                                .pending_hitl_requests
                                .get(&key)
                                .is_some_and(|existing| {
                                    bounded_hitl_lifecycle_event_bytes(existing.value()).as_deref()
                                        != Some(expected_body.as_slice())
                                })
                            || self.app_owner_notification_lifecycle.get(&key).is_some_and(
                                |existing| {
                                    existing.state == HitlLifecycleProofStateV1::Resolved
                                        || existing.timestamp != *timestamp
                                        || existing.absolute_expires_at_ms != absolute_expires_at_ms
                                },
                            )
                        {
                            self.hitl_lifecycle_recovery_healthy
                                .store(false, Ordering::SeqCst);
                            return true;
                        }
                        self.pending_hitl_requests.remove(&key);
                        self.resolved_hitl_requests.remove(&key);
                        // Retain even an expired identity until the full replay
                        // finishes so a following legacy HitlResolved row is
                        // not misclassified as a permanent generic proof.
                        self.app_owner_notification_lifecycle.insert(
                            key.clone(),
                            AppOwnerNotificationLifecycleState {
                                state: HitlLifecycleProofStateV1::PendingRedacted,
                                timestamp: *timestamp,
                                absolute_expires_at_ms,
                                resolved_authority: None,
                                app_owner_generation: None,
                            },
                        );
                        self.index_app_owner_notification_expiry(key, absolute_expires_at_ms);
                        true
                    },
                    Ok(None) => {
                        let Some(expected_body) = bounded_hitl_lifecycle_event_bytes(&event) else {
                            self.hitl_lifecycle_recovery_healthy
                                .store(false, Ordering::SeqCst);
                            return true;
                        };
                        if self.app_owner_notification_lifecycle.contains_key(&key)
                            || self.resolved_hitl_requests.contains_key(&key)
                            || self
                                .pending_hitl_requests
                                .get(&key)
                                .is_some_and(|existing| {
                                    bounded_hitl_lifecycle_event_bytes(existing.value()).as_deref()
                                        != Some(expected_body.as_slice())
                                })
                        {
                            self.hitl_lifecycle_recovery_healthy
                                .store(false, Ordering::SeqCst);
                            return true;
                        }
                        self.app_owner_notification_lifecycle.remove(&key);
                        self.resolved_hitl_requests.remove(&key);
                        if let Some(retained) = clone_hitl_lifecycle_event_iteratively(&event) {
                            self.pending_hitl_requests.insert(key, retained);
                        }
                        false
                    },
                    Err(error_detail) => {
                        self.hitl_lifecycle_recovery_healthy
                            .store(false, Ordering::SeqCst);
                        error!(
                            detail = error_detail,
                            "[MAGICIAN-RUNTIME-EVENTS] Invalid app owner notification lifecycle recovered"
                        );
                        true
                    },
                }
            },
            RuntimeTransportEvent::HitlResolved {
                source, timestamp, ..
            } => {
                let authority = hitl_resolved_fingerprint(&event)
                    .map(HitlResolvedAuthority::Verified)
                    .unwrap_or(HitlResolvedAuthority::Unverified);
                let source_is_app = source == "app_owner_notification";
                let app_authority = self.app_owner_notification_lifecycle.contains_key(&key);
                if source_is_app != app_authority {
                    self.hitl_lifecycle_recovery_healthy
                        .store(false, Ordering::SeqCst);
                    warn!(
                        correlation_id = %key.2,
                        source,
                        "[MAGICIAN-RUNTIME-EVENTS] Refusing replayed app/generic lifecycle collision"
                    );
                    return true;
                }
                if source_is_app {
                    let Some(mut app_state) = self.app_owner_notification_lifecycle.get_mut(&key)
                    else {
                        return true;
                    };
                    if app_state.state == HitlLifecycleProofStateV1::Resolved {
                        if app_state.timestamp != *timestamp
                            || app_state.resolved_authority != Some(authority)
                        {
                            self.hitl_lifecycle_recovery_healthy
                                .store(false, Ordering::SeqCst);
                        }
                        return true;
                    }
                    self.pending_hitl_requests.remove(&key);
                    app_state.state = HitlLifecycleProofStateV1::Resolved;
                    app_state.timestamp = *timestamp;
                    app_state.resolved_authority = Some(authority);
                    self.resolved_hitl_requests.remove(&key);
                    true
                } else {
                    if let Some(existing) = self.resolved_hitl_requests.get(&key) {
                        if existing.timestamp != *timestamp || existing.authority != authority {
                            self.hitl_lifecycle_recovery_healthy
                                .store(false, Ordering::SeqCst);
                            // Rebuild from the preserved first authority so the
                            // conflicting complete row does not poison every
                            // later shared reduction indefinitely.
                            return true;
                        }
                        return false;
                    }
                    self.pending_hitl_requests.remove(&key);
                    self.resolved_hitl_requests.insert(
                        key,
                        GenericHitlResolvedState {
                            timestamp: *timestamp,
                            authority,
                        },
                    );
                    false
                }
            },
            _ => false,
        }
    }

    fn record_replayed_hitl_lifecycle_proof(&self, proof: HitlLifecycleProofV1) -> bool {
        let key = (proof.principal, proof.workspace, proof.correlation_id);
        match proof.absolute_expires_at_ms {
            Some(absolute_expires_at_ms) => {
                if self.pending_hitl_requests.contains_key(&key)
                    || self.resolved_hitl_requests.contains_key(&key)
                    || self
                        .app_owner_notification_lifecycle
                        .get(&key)
                        .is_some_and(|existing| {
                            existing.absolute_expires_at_ms != absolute_expires_at_ms
                                || match (existing.state, proof.state) {
                                    (
                                        HitlLifecycleProofStateV1::PendingRedacted,
                                        HitlLifecycleProofStateV1::PendingRedacted,
                                    ) => existing.timestamp != proof.timestamp,
                                    (
                                        HitlLifecycleProofStateV1::PendingRedacted,
                                        HitlLifecycleProofStateV1::Resolved,
                                    ) => false,
                                    (
                                        HitlLifecycleProofStateV1::Resolved,
                                        HitlLifecycleProofStateV1::Resolved,
                                    ) => {
                                        existing.timestamp != proof.timestamp
                                            || existing.resolved_authority
                                                != Some(HitlResolvedAuthority::Unverified)
                                    },
                                    (
                                        HitlLifecycleProofStateV1::Resolved,
                                        HitlLifecycleProofStateV1::PendingRedacted,
                                    ) => true,
                                }
                        })
                {
                    self.hitl_lifecycle_recovery_healthy
                        .store(false, Ordering::SeqCst);
                    return true;
                }
                self.pending_hitl_requests.remove(&key);
                self.resolved_hitl_requests.remove(&key);
                self.app_owner_notification_lifecycle.insert(
                    key.clone(),
                    AppOwnerNotificationLifecycleState {
                        state: proof.state,
                        timestamp: proof.timestamp,
                        absolute_expires_at_ms,
                        resolved_authority: (proof.state == HitlLifecycleProofStateV1::Resolved)
                            .then_some(HitlResolvedAuthority::Unverified),
                        app_owner_generation: None,
                    },
                );
                self.index_app_owner_notification_expiry(key, absolute_expires_at_ms);
                true
            },
            None => {
                if self.app_owner_notification_lifecycle.contains_key(&key)
                    || self
                        .resolved_hitl_requests
                        .get(&key)
                        .is_some_and(|existing| {
                            existing.timestamp != proof.timestamp
                                || existing.authority != HitlResolvedAuthority::Unverified
                        })
                {
                    self.hitl_lifecycle_recovery_healthy
                        .store(false, Ordering::SeqCst);
                    return true;
                }
                self.app_owner_notification_lifecycle.remove(&key);
                self.pending_hitl_requests.remove(&key);
                self.resolved_hitl_requests.insert(
                    key,
                    GenericHitlResolvedState {
                        timestamp: proof.timestamp,
                        authority: HitlResolvedAuthority::Unverified,
                    },
                );
                false
            },
        }
    }

    fn record_replayed_hitl_lifecycle_proof_v2(&self, proof: HitlLifecycleProofV2) -> bool {
        let Ok(fingerprint) = validate_hitl_lifecycle_proof_v2(&proof) else {
            return true;
        };
        let key = (proof.principal, proof.workspace, proof.correlation_id);
        let authority = HitlResolvedAuthority::Verified(fingerprint);
        match proof.absolute_expires_at_ms {
            Some(absolute_expires_at_ms) => {
                if self.pending_hitl_requests.contains_key(&key)
                    || self.resolved_hitl_requests.contains_key(&key)
                    || self
                        .app_owner_notification_lifecycle
                        .get(&key)
                        .is_some_and(|existing| {
                            existing.absolute_expires_at_ms != absolute_expires_at_ms
                                || (existing.state == HitlLifecycleProofStateV1::Resolved
                                    && (existing.timestamp != proof.timestamp
                                        || existing.resolved_authority != Some(authority)))
                        })
                {
                    self.hitl_lifecycle_recovery_healthy
                        .store(false, Ordering::SeqCst);
                    return true;
                }
                self.pending_hitl_requests.remove(&key);
                self.resolved_hitl_requests.remove(&key);
                self.app_owner_notification_lifecycle.insert(
                    key.clone(),
                    AppOwnerNotificationLifecycleState {
                        state: HitlLifecycleProofStateV1::Resolved,
                        timestamp: proof.timestamp,
                        absolute_expires_at_ms,
                        resolved_authority: Some(authority),
                        app_owner_generation: None,
                    },
                );
                self.index_app_owner_notification_expiry(key, absolute_expires_at_ms);
                true
            },
            None => {
                if self.app_owner_notification_lifecycle.contains_key(&key)
                    || self
                        .resolved_hitl_requests
                        .get(&key)
                        .is_some_and(|existing| {
                            existing.timestamp != proof.timestamp || existing.authority != authority
                        })
                {
                    self.hitl_lifecycle_recovery_healthy
                        .store(false, Ordering::SeqCst);
                    return true;
                }
                self.pending_hitl_requests.remove(&key);
                self.app_owner_notification_lifecycle.remove(&key);
                self.resolved_hitl_requests.insert(
                    key,
                    GenericHitlResolvedState {
                        timestamp: proof.timestamp,
                        authority,
                    },
                );
                false
            },
        }
    }

    fn acquire_hitl_lifecycle_publication_lock(
        &self,
    ) -> Result<Option<FileLockGuard>, ArtifactV2Error> {
        self.hitl_lifecycle_persistence
            .as_ref()
            .map(|persistence| {
                AgentStorage::acquire_file_lock_exclusive_sync(&persistence.path).map_err(|error| {
                    ArtifactV2Error::Runtime(format!(
                        "failed to acquire HITL lifecycle publication lock: {error}"
                    ))
                })
            })
            .transpose()
    }

    fn append_hitl_lifecycle_journal(&self, event: &RuntimeTransportEvent) -> bool {
        self.append_hitl_lifecycle_journal_inner(event, false, false, None)
    }

    #[cfg(test)]
    fn append_app_owner_notification_lifecycle_after_exact_reduction(
        &self,
        event: &RuntimeTransportEvent,
        generation: AppOwnerNotificationPublicationGeneration,
        absolute_expires_at_ms: i64,
    ) -> bool {
        self.append_hitl_lifecycle_journal_inner(
            event,
            true,
            false,
            Some(AppOwnerNotificationAppendAuthority {
                generation,
                absolute_expires_at_ms,
            }),
        )
    }

    /// The caller holds cross-process publication authority and has repaired
    /// the shared tail once for the current batch. Every successful append
    /// below commits a terminating newline, so repeating the reverse scan for
    /// each of the at-most-32 records would add work without strengthening the
    /// cooperative-writer invariant.
    fn append_hitl_lifecycle_journal_in_repaired_batch(
        &self,
        event: &RuntimeTransportEvent,
    ) -> bool {
        self.append_hitl_lifecycle_journal_inner(event, true, true, None)
    }

    fn append_app_owner_notification_lifecycle_in_repaired_batch(
        &self,
        event: &RuntimeTransportEvent,
        generation: AppOwnerNotificationPublicationGeneration,
        absolute_expires_at_ms: i64,
    ) -> bool {
        self.append_hitl_lifecycle_journal_inner(
            event,
            true,
            true,
            Some(AppOwnerNotificationAppendAuthority {
                generation,
                absolute_expires_at_ms,
            }),
        )
    }

    fn append_hitl_lifecycle_journal_inner(
        &self,
        event: &RuntimeTransportEvent,
        exact_reduction_authorized: bool,
        tail_repaired_under_publication_authority: bool,
        app_authority: Option<AppOwnerNotificationAppendAuthority>,
    ) -> bool {
        if !hitl_lifecycle_event_is_admitted(event) {
            error!(
                max_record_depth = MAX_RETAINED_JSON_DEPTH,
                max_record_nodes = MAX_HITL_LIFECYCLE_RECORD_NODES,
                "[MAGICIAN-RUNTIME-EVENTS] Rejected HITL lifecycle before serialization because its schema exceeds admission; event withheld"
            );
            return false;
        }
        match app_owner_notification_expiry_ms(event) {
            Ok(Some(expiry)) if expiry <= chrono::Utc::now().timestamp_millis() => {
                return false;
            },
            Err(error_detail) => {
                error!(
                    detail = error_detail,
                    "[MAGICIAN-RUNTIME-EVENTS] Rejected malformed app owner notification lifecycle"
                );
                return false;
            },
            Ok(_) => {},
        }
        let Some(persistence) = self.hitl_lifecycle_persistence.as_ref() else {
            // Scoped lifecycle events still enter the pending registry and
            // broadcast channel when no durable journal is configured. Keep
            // the same wire ceiling in that mode instead of accidentally
            // making persistence configuration the admission boundary.
            return hitl_lifecycle_wire_size_is_admitted(event);
        };
        if !exact_reduction_authorized
            && !self.hitl_lifecycle_recovery_healthy.load(Ordering::SeqCst)
        {
            error!(
                path = %persistence.path.display(),
                "[MAGICIAN-RUNTIME-EVENTS] HITL lifecycle recovery is unhealthy; event withheld"
            );
            return false;
        }
        let mut output = BoundedHitlLifecycleRecord::new();
        let encoded = match event {
            RuntimeTransportEvent::HitlRequested { timestamp, .. } => {
                match app_owner_notification_expiry_ms(event) {
                    Ok(Some(absolute_expires_at_ms)) => {
                        if absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis() {
                            return false;
                        }
                        let Some(key) = scoped_hitl_lifecycle_key(event) else {
                            return false;
                        };
                        if self.resolved_hitl_requests.contains_key(&key)
                            || (self.pending_hitl_requests.contains_key(&key)
                                && !self.app_owner_notification_lifecycle.contains_key(&key))
                            || self.app_owner_notification_lifecycle.get(&key).is_some_and(
                                |existing| {
                                    existing.state == HitlLifecycleProofStateV1::Resolved
                                        || existing.timestamp != *timestamp
                                        || existing.absolute_expires_at_ms != absolute_expires_at_ms
                                },
                            )
                        {
                            return false;
                        }
                        write_hitl_lifecycle_proof(
                            &mut output,
                            &hitl_lifecycle_proof(
                                &key,
                                HitlLifecycleProofStateV1::PendingRedacted,
                                *timestamp,
                                Some(absolute_expires_at_ms),
                            ),
                        )
                    },
                    Ok(None) => {
                        let Some(key) = scoped_hitl_lifecycle_key(event) else {
                            return false;
                        };
                        if self.app_owner_notification_lifecycle.contains_key(&key) {
                            return false;
                        }
                        write_hitl_lifecycle_event(&mut output, event)
                    },
                    Err(error_detail) => Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        error_detail,
                    )),
                }
            },
            RuntimeTransportEvent::HitlResolved {
                source, timestamp, ..
            } => {
                let Some(key) = scoped_hitl_lifecycle_key(event) else {
                    return false;
                };
                let app_expiry = self
                    .app_owner_notification_lifecycle
                    .get(&key)
                    .map(|state| state.absolute_expires_at_ms);
                match app_expiry {
                    Some(absolute_expires_at_ms)
                        if absolute_expires_at_ms > chrono::Utc::now().timestamp_millis() =>
                    {
                        if source != "app_owner_notification" {
                            return false;
                        }
                        let Some(fingerprint) = hitl_resolved_fingerprint(event) else {
                            return false;
                        };
                        write_hitl_lifecycle_proof_v2(
                            &mut output,
                            &hitl_lifecycle_proof_v2(
                                &key,
                                *timestamp,
                                Some(absolute_expires_at_ms),
                                fingerprint,
                            ),
                        )
                    },
                    Some(_) => return false,
                    None if source == "app_owner_notification" => return false,
                    None => {
                        let Some(fingerprint) = hitl_resolved_fingerprint(event) else {
                            return false;
                        };
                        write_hitl_lifecycle_proof_v2(
                            &mut output,
                            &hitl_lifecycle_proof_v2(&key, *timestamp, None, fingerprint),
                        )
                    },
                }
            },
            _ => write_hitl_lifecycle_event(&mut output, event),
        };
        let mut line = match encoded {
            Ok(()) if !output.exceeded => output.bytes,
            Ok(()) => {
                error!(
                    path = %persistence.path.display(),
                    max_record_bytes = MAX_HITL_LIFECYCLE_RECORD_BYTES,
                    "[MAGICIAN-RUNTIME-EVENTS] HITL lifecycle exceeds record byte admission; event withheld"
                );
                return false;
            },
            Err(serialize_error) => {
                error!(
                    error = %serialize_error,
                    max_record_bytes = MAX_HITL_LIFECYCLE_RECORD_BYTES,
                    "[MAGICIAN-RUNTIME-EVENTS] Failed bounded HITL lifecycle serialization; event withheld"
                );
                return false;
            },
        };
        if let Some(authority_path) = persistence.authority_path.as_ref() {
            let is_app_event = match event {
                RuntimeTransportEvent::HitlRequested { .. } => {
                    app_owner_notification_expiry_ms(event)
                        .ok()
                        .flatten()
                        .is_some()
                },
                RuntimeTransportEvent::HitlResolved { source, .. } => {
                    source == "app_owner_notification"
                },
                _ => false,
            };
            // V3 authority is generation-bound. Reject legacy/public app
            // mutation paths as an ordinary admission failure without
            // poisoning unrelated lifecycle recovery health.
            if is_app_event && app_authority.is_none() {
                return false;
            }
            let commit = (|| -> Result<(), ArtifactV2Error> {
                validate_hitl_lifecycle_legacy_fence(&persistence.path)?;
                let authority = HitlLifecycleDiskReducer::open_authority(authority_path)?;
                if !authority.authority_is_ready()? {
                    return Err(ArtifactV2Error::InvalidRequest(
                        "lifecycle authority is not durably ready".to_string(),
                    ));
                }
                authority.begin()?;
                if is_app_event {
                    let app_authority = app_authority.ok_or_else(|| {
                        ArtifactV2Error::InvalidRequest(
                            "tokenless app lifecycle mutation is forbidden after V3 cutover"
                                .to_string(),
                        )
                    })?;
                    if app_authority.absolute_expires_at_ms <= chrono::Utc::now().timestamp_millis()
                    {
                        return Err(ArtifactV2Error::InvalidRequest(
                            "expired app lifecycle mutation is forbidden".to_string(),
                        ));
                    }
                    let key = scoped_hitl_lifecycle_key(event).ok_or_else(|| {
                        ArtifactV2Error::InvalidRequest(
                            "app lifecycle mutation is missing its scope".to_string(),
                        )
                    })?;
                    let generation = app_authority.generation.canonical();
                    match event {
                        RuntimeTransportEvent::HitlRequested { timestamp, .. } => {
                            if app_owner_notification_expiry_ms(event).ok().flatten()
                                != Some(app_authority.absolute_expires_at_ms)
                            {
                                return Err(ArtifactV2Error::InvalidRequest(
                                    "app lifecycle append authority has a mismatched expiry"
                                        .to_string(),
                                ));
                            }
                            authority.upsert_app_with_generation(
                                &key,
                                HITL_REDUCER_APP_PENDING,
                                *timestamp,
                                app_authority.absolute_expires_at_ms,
                                None,
                                &generation,
                            )?;
                        },
                        RuntimeTransportEvent::HitlResolved { timestamp, .. } => {
                            if *timestamp >= app_authority.absolute_expires_at_ms
                                || authority.existing_record(&key)?.is_none()
                            {
                                return Err(ArtifactV2Error::InvalidRequest(
                                    "app resolution requires live pending authority".to_string(),
                                ));
                            }
                            let fingerprint =
                                hitl_resolved_fingerprint(event).ok_or_else(|| {
                                    ArtifactV2Error::InvalidRequest(
                                        "app resolution has no canonical fingerprint".to_string(),
                                    )
                                })?;
                            authority.upsert_app_with_generation(
                                &key,
                                HITL_REDUCER_APP_RESOLVED,
                                *timestamp,
                                app_authority.absolute_expires_at_ms,
                                Some(&fingerprint.0),
                                &generation,
                            )?;
                        },
                        _ => unreachable!("is_app_event only admits HITL lifecycle variants"),
                    }
                } else {
                    authority.ingest_event(event)?;
                }
                authority.finish()
            })();
            if let Err(append_error) = commit {
                if hitl_lifecycle_append_error_is_admission_rejection(&append_error) {
                    return false;
                }
                self.hitl_lifecycle_recovery_healthy
                    .store(false, Ordering::SeqCst);
                error!(
                    error = %append_error,
                    path = %authority_path.display(),
                    "[MAGICIAN-RUNTIME-EVENTS] Failed to commit keyed HITL lifecycle authority; event withheld"
                );
                return false;
            }
            return true;
        }
        line.push(b'\n');
        if !tail_repaired_under_publication_authority {
            match repair_unterminated_hitl_lifecycle_tail_before_append(&persistence.path) {
                Ok(true) => warn!(
                    path = %persistence.path.display(),
                    "[MAGICIAN-RUNTIME-EVENTS] Removed an unterminated peer-crash lifecycle tail before append"
                ),
                Ok(false) => {},
                Err(repair_error) => {
                    self.hitl_lifecycle_recovery_healthy
                        .store(false, Ordering::SeqCst);
                    error!(
                        error = %repair_error,
                        path = %persistence.path.display(),
                        "[MAGICIAN-RUNTIME-EVENTS] Refused lifecycle append because its existing crash tail was not safely repairable"
                    );
                    return false;
                },
            }
        }
        if let Err(append_error) = persistence
            .workspace
            .append_private_durable_path_sync(&persistence.path, &line)
        {
            self.hitl_lifecycle_recovery_healthy
                .store(false, Ordering::SeqCst);
            error!(
                error = %append_error,
                path = %persistence.path.display(),
                "[MAGICIAN-RUNTIME-EVENTS] Failed to durably append HITL lifecycle; event withheld"
            );
            return false;
        }
        true
    }

    fn hitl_lifecycle_compaction_error_is_cas_conflict(error: &ArtifactV2Error) -> bool {
        matches!(
            error,
            ArtifactV2Error::InvalidRequest(detail)
                if detail == WORKSPACE_DESTINATION_CAS_CONFLICT_DETAIL
        )
    }

    fn compact_hitl_lifecycle_journal_with_publication_authority(
        &self,
    ) -> Result<(), ArtifactV2Error> {
        let _publication_guard = self.acquire_hitl_lifecycle_publication_lock()?;
        warn!(
            conflicts = self
                .hitl_lifecycle_compaction_cas_conflicts
                .load(Ordering::SeqCst),
            "[MAGICIAN-RUNTIME-EVENTS] Lifecycle compaction entered bounded-conflict publication fallback"
        );
        let result = self.compact_hitl_lifecycle_journal_attempt(true);
        match &result {
            Ok(()) => self
                .hitl_lifecycle_compaction_cas_conflicts
                .store(0, Ordering::SeqCst),
            Err(error) if Self::hitl_lifecycle_compaction_error_is_cas_conflict(error) => {},
            Err(_) => self
                .hitl_lifecycle_compaction_cas_conflicts
                .store(0, Ordering::SeqCst),
        }
        result
    }

    /// Compact durable lifecycle authority. V3 deletes one fixed page of
    /// expired app proofs and vacuums a fixed page slice; its permanent generic
    /// rows are never scanned. The pre-cutover fallback retains the legacy
    /// streaming JSONL reduction/CAS rewrite solely so failed migrations remain
    /// fail closed rather than losing existing authority.
    fn compact_hitl_lifecycle_journal(&self) -> Result<(), ArtifactV2Error> {
        if self
            .hitl_lifecycle_compaction_cas_conflicts
            .load(Ordering::SeqCst)
            >= HITL_LIFECYCLE_COMPACTION_CAS_FALLBACK_THRESHOLD
        {
            return self.compact_hitl_lifecycle_journal_with_publication_authority();
        }
        let result = self.compact_hitl_lifecycle_journal_attempt(false);
        match result {
            Ok(()) => {
                self.hitl_lifecycle_compaction_cas_conflicts
                    .store(0, Ordering::SeqCst);
                Ok(())
            },
            Err(error) if Self::hitl_lifecycle_compaction_error_is_cas_conflict(&error) => {
                let previous = self
                    .hitl_lifecycle_compaction_cas_conflicts
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                        Some(
                            current
                                .saturating_add(1)
                                .min(HITL_LIFECYCLE_COMPACTION_CAS_FALLBACK_THRESHOLD),
                        )
                    })
                    .unwrap_or(HITL_LIFECYCLE_COMPACTION_CAS_FALLBACK_THRESHOLD);
                let conflicts = previous
                    .saturating_add(1)
                    .min(HITL_LIFECYCLE_COMPACTION_CAS_FALLBACK_THRESHOLD);
                if conflicts >= HITL_LIFECYCLE_COMPACTION_CAS_FALLBACK_THRESHOLD {
                    self.compact_hitl_lifecycle_journal_with_publication_authority()
                } else {
                    Err(error)
                }
            },
            Err(error) => {
                self.hitl_lifecycle_compaction_cas_conflicts
                    .store(0, Ordering::SeqCst);
                Err(error)
            },
        }
    }

    fn compact_hitl_lifecycle_journal_attempt(
        &self,
        publication_authority_already_held: bool,
    ) -> Result<(), ArtifactV2Error> {
        let Some(persistence) = self.hitl_lifecycle_persistence.as_ref() else {
            return Ok(());
        };
        // A prior worker cancellation or panic releases the process-local
        // slot. Compaction is the only producer of new work items, so it is
        // also the natural retry point for supervision.
        self.ensure_hitl_lifecycle_orphan_sweeper();
        if let Some(authority_path) = persistence.authority_path.as_ref() {
            let _publication_guard = if publication_authority_already_held {
                None
            } else {
                self.acquire_hitl_lifecycle_publication_lock()?
            };
            validate_hitl_lifecycle_legacy_fence(&persistence.path)?;
            let _lifecycle_guard = self
                .hitl_lifecycle_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let authority = HitlLifecycleDiskReducer::open_authority(authority_path)?;
            if !authority.authority_is_ready()? {
                return Err(ArtifactV2Error::InvalidRequest(
                    "lifecycle authority is not durably ready".to_string(),
                ));
            }
            authority.begin()?;
            authority.delete_expired_app_page(
                chrono::Utc::now().timestamp_millis(),
                APP_OWNER_NOTIFICATION_EXPIRY_BATCH,
            )?;
            authority.finish()?;
            authority.incremental_vacuum(64)?;
            let next_expiry = authority.minimum_app_expiry()?;
            drop(_lifecycle_guard);
            if let Some(next_expiry) = next_expiry {
                self.index_app_owner_notification_expiry(
                    (
                        String::new(),
                        String::new(),
                        "__app_owner_notification_compaction_retry".to_string(),
                    ),
                    next_expiry,
                );
            }
            return Ok(());
        }
        let reducer = HitlLifecycleDiskReducer::new(&persistence.path)?;
        let baseline_summary = reduce_hitl_lifecycle_journal_to_disk(&persistence.path, &reducer)?;
        if baseline_summary.complete_len != baseline_summary.file_len {
            let incomplete_bytes = baseline_summary.file_len - baseline_summary.complete_len;
            if incomplete_bytes > MAX_HITL_LIFECYCLE_RECORD_BYTES as u64 {
                self.hitl_lifecycle_recovery_healthy
                    .store(false, Ordering::SeqCst);
                return Err(ArtifactV2Error::InvalidRequest(format!(
                    "unterminated HITL lifecycle suffix exceeds the bounded {}-byte compaction ceiling",
                    MAX_HITL_LIFECYCLE_RECORD_BYTES
                )));
            }
            warn!(
                truncated_bytes = incomplete_bytes,
                path = %persistence.path.display(),
                "[MAGICIAN-RUNTIME-EVENTS] Lifecycle compaction omitted an incomplete crash tail"
            );
        }
        if baseline_summary.encountered_invalid_record {
            // The rebuilt journal removes unclassifiable bytes (which could be
            // a damaged legacy notification body), but unknown exact lookup
            // authority remains fail-closed for this process.
            self.hitl_lifecycle_recovery_healthy
                .store(false, Ordering::SeqCst);
        }
        let expected_destination_len = baseline_summary.file_len;
        let expected_destination_sha256 = baseline_summary.file_sha256;
        let (_staging_cleanup, mut staging) =
            HitlLifecycleCompactionStaging::new(&persistence.path)?;
        let staging_path = _staging_cleanup.path.clone();
        let mut digest = Sha256::new();
        let earliest_retained_app_expiry =
            stage_reduced_hitl_lifecycle(&reducer, &mut staging, &mut digest)?;
        staging.flush()?;
        staging.sync_all()?;
        drop(staging);
        let expected_sha256 = format!("{:x}", digest.finalize());
        if earliest_retained_app_expiry
            .is_some_and(|expiry| expiry <= chrono::Utc::now().timestamp_millis())
        {
            return Err(ArtifactV2Error::Runtime(
                "app owner notification expired during lifecycle compaction staging".to_string(),
            ));
        }
        // Lock order is cross-process publication authority first, then the
        // short in-process admission mutex. Scoped emitters use the same order,
        // so waiting for a peer never pins local admission and no inversion is
        // possible. The provider repeats destination identity/length/digest
        // validation immediately before durable atomic replacement.
        let _publication_guard = if publication_authority_already_held {
            None
        } else {
            self.acquire_hitl_lifecycle_publication_lock()?
        };
        let _lifecycle_guard = self
            .hitl_lifecycle_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if earliest_retained_app_expiry
            .is_some_and(|expiry| expiry <= chrono::Utc::now().timestamp_millis())
        {
            return Err(ArtifactV2Error::Runtime(
                "app owner notification expired before lifecycle compaction publication"
                    .to_string(),
            ));
        }
        persistence
            .workspace
            .copy_private_workspace_file_verified_atomic_path_sync_if_destination_matches(
                &staging_path,
                &persistence.path,
                &expected_sha256,
                expected_destination_len,
                &expected_destination_sha256,
            )?;
        Ok(())
    }

    fn arm_restored_app_owner_notification_expiries(&self) {
        let index_is_empty = self
            .app_owner_notification_expiry_index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty();
        if index_is_empty {
            if let Some(persistence) = self.hitl_lifecycle_persistence.as_ref() {
                if persistence.authority_path.is_some() {
                    let next_expiry = self
                        .acquire_hitl_lifecycle_publication_lock()
                        .map_err(|error| ArtifactV2Error::Runtime(error.to_string()))
                        .and_then(|_guard| {
                            prepare_exact_hitl_lifecycle_reducer(persistence)?.minimum_app_expiry()
                        });
                    match next_expiry {
                        Ok(Some(expiry)) => self.index_app_owner_notification_expiry(
                            (
                                String::new(),
                                String::new(),
                                "__app_owner_notification_compaction_retry".to_string(),
                            ),
                            expiry,
                        ),
                        Ok(None) => {},
                        Err(error) => {
                            self.hitl_lifecycle_recovery_healthy
                                .store(false, Ordering::SeqCst);
                            error!(
                                error = %error,
                                "[MAGICIAN-RUNTIME-EVENTS] Failed to arm keyed app lifecycle expiry"
                            );
                        },
                    }
                }
            }
        }
        if !self
            .app_owner_notification_expiry_index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
        {
            self.ensure_app_owner_notification_expiry_sweeper();
        }
    }

    /// Start one restartable background orphan worker after lifecycle recovery
    /// has left the startup publication critical section. Each cycle retains
    /// its directory cursor across fixed-size pages, and every destructive
    /// action additionally requires the item's cross-process lease.
    fn ensure_hitl_lifecycle_orphan_sweeper(&self) {
        let Some(persistence) = self.hitl_lifecycle_persistence.as_ref() else {
            return;
        };
        if self
            .hitl_lifecycle_orphan_sweeper_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.hitl_lifecycle_orphan_sweeper_started
                .store(false, Ordering::SeqCst);
            return;
        };
        let journal_path = persistence.path.clone();
        let sweeper_guard = HitlLifecycleOrphanSweeperGuard {
            started: Arc::clone(&self.hitl_lifecycle_orphan_sweeper_started),
        };
        let _sweeper = handle.spawn(async move {
            let _sweeper_guard = sweeper_guard;
            loop {
                sweep_hitl_lifecycle_orphans_once(&journal_path).await;
                tokio::time::sleep(tokio::time::Duration::from_secs(
                    HITL_LIFECYCLE_ORPHAN_SWEEP_INTERVAL_SECS,
                ))
                .await;
            }
        });
    }

    fn index_app_owner_notification_expiry(
        &self,
        key: HitlLifecycleKey,
        absolute_expires_at_ms: i64,
    ) {
        self.app_owner_notification_expiry_index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(absolute_expires_at_ms)
            .or_default()
            .insert(key);
    }

    fn index_app_owner_notification_compaction_retry(&self) {
        self.index_app_owner_notification_expiry(
            (
                String::new(),
                String::new(),
                "__app_owner_notification_compaction_retry".to_string(),
            ),
            chrono::Utc::now()
                .timestamp_millis()
                .saturating_add(APP_OWNER_NOTIFICATION_COMPACTION_RETRY_MS),
        );
    }

    fn arm_app_owner_notification_expiry(
        &self,
        key: HitlLifecycleKey,
        absolute_expires_at_ms: i64,
    ) {
        self.index_app_owner_notification_expiry(key, absolute_expires_at_ms);
        self.ensure_app_owner_notification_expiry_sweeper();
    }

    /// One shared bounded sweeper owns all app-notification proof expiries.
    /// Construction is also used by synchronous tests/tools, so absence of a
    /// Tokio handle is ordinary; the next runtime publication retries arming,
    /// while exact reads still enforce expiry synchronously.
    fn ensure_app_owner_notification_expiry_sweeper(&self) {
        if self
            .app_owner_notification_expiry_sweeper_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.app_owner_notification_expiry_sweeper_started
                .store(false, Ordering::SeqCst);
            return;
        };
        let broadcaster = self.clone();
        let mut sweeper_ownership = AppOwnerNotificationExpirySweeperGuard::new(Arc::clone(
            &self.app_owner_notification_expiry_sweeper_started,
        ));
        let _sweeper = handle.spawn(async move {
            loop {
                let now_ms = chrono::Utc::now().timestamp_millis();
                let next_expiry_ms = broadcaster
                    .app_owner_notification_expiry_index
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .first_key_value()
                    .map(|(expiry, _)| *expiry);
                let Some(next_expiry_ms) = next_expiry_ms else {
                    sweeper_ownership.release_for_handoff();
                    if !broadcaster
                        .app_owner_notification_expiry_index
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .is_empty()
                    {
                        broadcaster.ensure_app_owner_notification_expiry_sweeper();
                    }
                    return;
                };
                if next_expiry_ms > now_ms {
                    let sleep_ms = next_expiry_ms
                        .saturating_sub(now_ms)
                        // A newly indexed earlier expiry cannot wake this
                        // shared task, so poll at a bounded one-second ceiling.
                        // Exact lookup still refuses expired content at the
                        // millisecond boundary.
                        .min(1_000)
                        .max(1) as u64;
                    tokio::time::sleep(tokio::time::Duration::from_millis(sleep_ms)).await;
                    continue;
                }

                let batch_broadcaster = broadcaster.clone();
                match tokio::task::spawn_blocking(move || {
                    let mut expired = Vec::with_capacity(APP_OWNER_NOTIFICATION_EXPIRY_BATCH);
                    let more_due = {
                        let mut index = batch_broadcaster
                            .app_owner_notification_expiry_index
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        while expired.len() < APP_OWNER_NOTIFICATION_EXPIRY_BATCH {
                            let Some(expiry) = index.first_key_value().map(|(expiry, _)| *expiry)
                            else {
                                break;
                            };
                            if expiry > now_ms {
                                break;
                            }
                            let mut remove_bucket = false;
                            if let Some(keys) = index.get_mut(&expiry) {
                                while expired.len() < APP_OWNER_NOTIFICATION_EXPIRY_BATCH {
                                    let Some(key) = keys.iter().next().cloned() else {
                                        break;
                                    };
                                    keys.remove(&key);
                                    expired.push((expiry, key));
                                }
                                remove_bucket = keys.is_empty();
                            }
                            if remove_bucket {
                                index.remove(&expiry);
                            }
                        }
                        index
                            .first_key_value()
                            .is_some_and(|(expiry, _)| *expiry <= now_ms)
                    };
                    if expired.is_empty() {
                        return Ok::<(), ArtifactV2Error>(());
                    }
                    {
                        let _lifecycle_guard = batch_broadcaster
                            .hitl_lifecycle_lock
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        for (indexed_expiry, key) in expired {
                            let current_expiry = batch_broadcaster
                                .app_owner_notification_lifecycle
                                .get(&key)
                                .map(|state| state.absolute_expires_at_ms);
                            if current_expiry == Some(indexed_expiry) && indexed_expiry <= now_ms {
                                batch_broadcaster.pending_hitl_requests.remove(&key);
                                batch_broadcaster.resolved_hitl_requests.remove(&key);
                                batch_broadcaster
                                    .app_owner_notification_lifecycle
                                    .remove(&key);
                            }
                        }
                    }
                    // Drain a due burst through bounded map-removal batches,
                    // then coalesce it into one authoritative rewrite. Exact
                    // reads may already have evicted every popped row; the last
                    // batch still compacts their content-free durable proofs.
                    if more_due {
                        return Ok(());
                    }
                    if let Err(error) = batch_broadcaster.compact_hitl_lifecycle_journal() {
                        batch_broadcaster.index_app_owner_notification_compaction_retry();
                        return Err(error);
                    }
                    Ok(())
                })
                .await
                {
                    Ok(Ok(())) => {},
                    Ok(Err(compaction_error)) => warn!(
                        error = %compaction_error,
                        "[MAGICIAN-RUNTIME-EVENTS] Failed to remove expired app owner notification lifecycle proofs"
                    ),
                    Err(join_error) => warn!(
                        error = %join_error,
                        "[MAGICIAN-RUNTIME-EVENTS] App owner notification expiry worker failed"
                    ),
                }
            }
        });
    }

    fn expire_app_owner_notification_if_due(&self, key: &HitlLifecycleKey, now_ms: i64) -> bool {
        if self
            .app_owner_notification_lifecycle
            .get(key)
            .is_none_or(|state| state.absolute_expires_at_ms > now_ms)
        {
            return false;
        }
        let _guard = self
            .hitl_lifecycle_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self
            .app_owner_notification_lifecycle
            .get(key)
            .is_none_or(|state| state.absolute_expires_at_ms > now_ms)
        {
            return false;
        }
        self.pending_hitl_requests.remove(key);
        self.resolved_hitl_requests.remove(key);
        self.app_owner_notification_lifecycle.remove(key);
        // The shared indexed sweeper owns the synchronous provider rewrite on
        // a blocking worker. Exact reads only evict the body/state here, so a
        // request handler never performs physical cleanup I/O.
        true
    }

    fn enrich_scope_if_registered(&self, event: RuntimeTransportEvent) -> RuntimeTransportEvent {
        if let Some(execution_id) = event.execution_id_for_scope_enrichment() {
            if let Some(scope) = self
                .runtime_canonical_event_scopes
                .get(execution_id)
                .map(|entry| entry.clone())
            {
                return event.with_registered_scope_if_missing(&scope);
            }
        }
        event
    }

    /// Emit a runtime fact and mirror it into the canonical event sink
    /// when the event maps to a registered execution scope.
    pub fn emit(&self, event: RuntimeTransportEvent) {
        let event = self.sanitize_chat_message_presentation_for_transport(event);
        let event = self.enrich_scope_if_registered(event);
        let scoped_hitl = scoped_hitl_lifecycle_event(&event);
        if hitl_lifecycle_event(&event)
            && (!hitl_lifecycle_event_is_admitted(&event)
                || (!scoped_hitl && !hitl_lifecycle_wire_size_is_admitted(&event)))
        {
            error!(
                max_record_depth = MAX_RETAINED_JSON_DEPTH,
                max_record_nodes = MAX_HITL_LIFECYCLE_RECORD_NODES,
                "[MAGICIAN-RUNTIME-EVENTS] Rejected unbounded HITL lifecycle before canonical transport"
            );
            discard_hitl_lifecycle_event_iteratively(event);
            return;
        }
        let planning_progress = self.v3_planning_progress_for_event(&event);
        if scoped_hitl {
            let _publication_guard = match self.acquire_hitl_lifecycle_publication_lock() {
                Ok(guard) => guard,
                Err(lock_error) => {
                    let _lifecycle_guard = self
                        .hitl_lifecycle_lock
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    self.settle_app_owner_notification_after_append_failure(&event);
                    error!(
                        error = %lock_error,
                        "[MAGICIAN-RUNTIME-EVENTS] Failed to acquire HITL lifecycle publication authority; event withheld"
                    );
                    discard_hitl_lifecycle_event_iteratively(event);
                    return;
                },
            };
            let mut lifecycle_guard = self.hitl_lifecycle_persistence.is_none().then(|| {
                self.hitl_lifecycle_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            });
            let (accepted, appended) = if self.hitl_lifecycle_persistence.is_some() {
                self.reconcile_scoped_hitl_with_persistence(&event)
            } else {
                self.reconcile_scoped_hitl_without_persistence(&event)
            };
            if !accepted {
                discard_hitl_lifecycle_event_iteratively(event);
                return;
            }
            if self.hitl_lifecycle_persistence.is_none() {
                self.record_pending_hitl_lifecycle(&event);
            } else {
                lifecycle_guard = Some(
                    self.hitl_lifecycle_lock
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                );
            }
            if self.redact_app_owner_notification_request_if_expired(&event) {
                drop(_publication_guard);
                discard_hitl_lifecycle_event_iteratively(event);
                drop(lifecycle_guard);
                return;
            }
            drop(_publication_guard);
            if appended {
                self.emit_canonical_event(&event);
                self.broadcast_transport_event(event);
            } else {
                discard_hitl_lifecycle_event_iteratively(event);
                return;
            }
            drop(lifecycle_guard);
        } else {
            self.emit_canonical_event(&event);
            self.broadcast_transport_event(event);
        }
        if let Some(planning_progress) = planning_progress {
            self.emit_transport_only(planning_progress);
        }
    }

    /// Replay a durable outbox record through the exact canonical scope its
    /// producer recorded.
    ///
    /// A cold lifecycle projector must not use [`Self::emit`] for this job. Its
    /// process-local scope registry will normally be empty, and most runtime
    /// events do not carry all four canonical scope fields themselves; `emit`
    /// would still broadcast those events while silently omitting canonical
    /// persistence. This entry point neither consults nor mutates that registry.
    /// It also verifies that the event's canonical mapping names the recorded
    /// execution before either rail receives the event.
    pub fn emit_recorded_runtime_fact(
        &self,
        event: RuntimeTransportEvent,
        scope: CanonicalEventScope,
    ) -> Result<(), RecordedRuntimeFactRefused> {
        self.emit_recorded_runtime_fact_inner(event, scope, false, None)
            .map(|_| ())
    }

    /// Replay a durable canonical fact and return proof of its eventual
    /// canonical append. Cold outbox projectors must await this receipt before
    /// saving their own cursor; queue admission alone is not durability.
    pub fn emit_recorded_runtime_fact_with_receipt(
        &self,
        event: RuntimeTransportEvent,
        scope: CanonicalEventScope,
    ) -> Result<RuntimeCanonicalEventReceipt, RecordedRuntimeFactRefused> {
        self.emit_recorded_runtime_fact_inner(event, scope, true, None)?
            .ok_or_else(|| RecordedRuntimeFactRefused::CanonicalSinkRefused {
                detail: "canonical sink returned no durability receipt".to_string(),
            })
    }

    /// Replay a stateless journal record with host-owned source identity and
    /// return proof that persistence plus every registered observer completed.
    ///
    /// Crate-private deliberately: only the projector holding the authoritative
    /// [`EventKey`](crate::magician_v2::execution::agentic::run_loop::journal::EventKey)
    /// may attach this provenance. The mapped transport payload is never
    /// allowed to choose or preserve either identity field.
    pub(crate) fn emit_projected_runtime_fact_with_receipt(
        &self,
        event: RuntimeTransportEvent,
        scope: CanonicalEventScope,
        source_event_ref: &str,
    ) -> Result<RuntimeCanonicalEventReceipt, RecordedRuntimeFactRefused> {
        self.emit_recorded_runtime_fact_inner(event, scope, true, Some(source_event_ref))?
            .ok_or_else(|| RecordedRuntimeFactRefused::CanonicalSinkRefused {
                detail: "canonical sink returned no durability receipt".to_string(),
            })
    }

    fn emit_recorded_runtime_fact_inner(
        &self,
        event: RuntimeTransportEvent,
        scope: CanonicalEventScope,
        require_receipt: bool,
        source_event_ref: Option<&str>,
    ) -> Result<Option<RuntimeCanonicalEventReceipt>, RecordedRuntimeFactRefused> {
        let event = self.sanitize_chat_message_presentation_for_transport(event);
        let event = event.with_registered_scope_if_missing(&scope);
        // Validate bounded HITL shape before canonical mapping clones the
        // caller-controlled input schema into JSON payloads. Recorded facts
        // can arrive from cold durable projectors, so they must cross the same
        // depth/node gate before any recursive/value-amplifying work.
        let scoped_hitl = scoped_hitl_lifecycle_event(&event);
        if hitl_lifecycle_event(&event)
            && (!hitl_lifecycle_event_is_admitted(&event)
                || (!scoped_hitl && !hitl_lifecycle_wire_size_is_admitted(&event)))
        {
            error!(
                max_record_depth = MAX_RETAINED_JSON_DEPTH,
                max_record_nodes = MAX_HITL_LIFECYCLE_RECORD_NODES,
                "[MAGICIAN-RUNTIME-EVENTS] Refused recorded canonical HITL lifecycle before mapping"
            );
            discard_hitl_lifecycle_event_iteratively(event);
            return Err(RecordedRuntimeFactRefused::HitlAdmissionRefused);
        }
        let Some(mut mapped) = map_v2_realtime_event(&event) else {
            return Err(RecordedRuntimeFactRefused::NotCanonicalInThisBuild);
        };
        if mapped.execution_id != scope.execution_id {
            return Err(RecordedRuntimeFactRefused::ExecutionMismatch {
                recorded: scope.execution_id,
                mapped: mapped.execution_id,
            });
        }
        if let Some(source_event_ref) = source_event_ref {
            attach_projected_source_identity(
                &mut mapped.payload,
                source_event_ref,
                &scope.ui_thread_id,
            )
            .map_err(|detail| RecordedRuntimeFactRefused::CanonicalSinkRefused { detail })?;
        }
        let Some(sink) = self
            .runtime_canonical_event_sink
            .read()
            .expect("runtime_canonical_event_sink lock poisoned")
            .clone()
        else {
            return Err(RecordedRuntimeFactRefused::CanonicalSinkUnavailable);
        };

        let planning_progress = self.v3_planning_progress_for_event(&event);
        let receipt;
        if scoped_hitl {
            let _publication_guard = match self.acquire_hitl_lifecycle_publication_lock() {
                Ok(guard) => guard,
                Err(lock_error) => {
                    let _lifecycle_guard = self
                        .hitl_lifecycle_lock
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    self.settle_app_owner_notification_after_append_failure(&event);
                    error!(
                        error = %lock_error,
                        "[MAGICIAN-RUNTIME-EVENTS] Failed to acquire HITL lifecycle publication authority; recorded event withheld"
                    );
                    discard_hitl_lifecycle_event_iteratively(event);
                    return Err(RecordedRuntimeFactRefused::HitlJournalUnavailable);
                },
            };
            let mut lifecycle_guard = self.hitl_lifecycle_persistence.is_none().then(|| {
                self.hitl_lifecycle_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            });
            let (accepted, appended) = if self.hitl_lifecycle_persistence.is_some() {
                self.reconcile_scoped_hitl_with_persistence(&event)
            } else {
                self.reconcile_scoped_hitl_without_persistence(&event)
            };
            if !accepted {
                discard_hitl_lifecycle_event_iteratively(event);
                return Err(RecordedRuntimeFactRefused::HitlJournalUnavailable);
            }
            if self.hitl_lifecycle_persistence.is_none() {
                self.record_pending_hitl_lifecycle(&event);
            } else {
                lifecycle_guard = Some(
                    self.hitl_lifecycle_lock
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                );
            }
            if self.redact_app_owner_notification_request_if_expired(&event) {
                drop(_publication_guard);
                discard_hitl_lifecycle_event_iteratively(event);
                drop(lifecycle_guard);
                return Err(RecordedRuntimeFactRefused::HitlAdmissionRefused);
            }
            drop(_publication_guard);
            receipt = if require_receipt {
                Some(
                    sink.emit_with_receipt(scope, mapped.event_type, mapped.payload)
                        .map_err(|detail| RecordedRuntimeFactRefused::CanonicalSinkRefused {
                            detail,
                        })?,
                )
            } else {
                sink.emit(scope, mapped.event_type, mapped.payload);
                None
            };
            // A journal append may have succeeded before a previous canonical
            // sink attempt failed. Exact acceptance on retry must still
            // republish after the canonical attempt succeeds, or the transport
            // rail could remain withheld forever.
            let _ = appended;
            self.broadcast_transport_event(event);
            drop(lifecycle_guard);
        } else {
            receipt = if require_receipt {
                Some(
                    sink.emit_with_receipt(scope, mapped.event_type, mapped.payload)
                        .map_err(|detail| RecordedRuntimeFactRefused::CanonicalSinkRefused {
                            detail,
                        })?,
                )
            } else {
                sink.emit(scope, mapped.event_type, mapped.payload);
                None
            };
            self.broadcast_transport_event(event);
        }
        if let Some(planning_progress) = planning_progress {
            self.emit_transport_only(planning_progress);
        }
        Ok(receipt)
    }

    fn sanitize_chat_message_presentation_for_transport(
        &self,
        event: RuntimeTransportEvent,
    ) -> RuntimeTransportEvent {
        let RuntimeTransportEvent::ChatMessageReceived {
            session_id,
            mut message,
            principal,
            workspace,
            origin_channel,
            timestamp,
        } = event
        else {
            return event;
        };

        let Some(presentation) = message.presentation.take() else {
            return RuntimeTransportEvent::ChatMessageReceived {
                session_id,
                message,
                principal,
                workspace,
                origin_channel,
                timestamp,
            };
        };

        match StructuredResponseV1::attach_to_content(&message.content, presentation) {
            Ok(validated) => {
                message.presentation = Some(validated);
                RuntimeTransportEvent::ChatMessageReceived {
                    session_id,
                    message,
                    principal,
                    workspace,
                    origin_channel,
                    timestamp,
                }
            },
            Err(drop_reason) => {
                warn!(
                    session_id = session_id.as_str(),
                    reason = structured_response_drop_reason_code(&drop_reason),
                    "[MAGICIAN-RUNTIME-EVENTS] Dropping invalid structured chat presentation for transport"
                );
                message.presentation = None;
                RuntimeTransportEvent::ChatMessageReceived {
                    session_id,
                    message,
                    principal,
                    workspace,
                    origin_channel,
                    timestamp,
                }
            },
        }
    }

    fn emit_canonical_event(&self, event: &RuntimeTransportEvent) {
        if let Some(mapped) = map_v2_realtime_event(event) {
            // Prefer the live registration; fall back to the event's own
            // self-describing scope so late/post-restart durable facts
            // (notably `HitlResolved`) still persist to the origin
            // execution's `events.jsonl` even after the one-shot
            // execution finished and its in-memory scope registration is
            // gone. The registry HIT path is byte-identical to before;
            // the fallback fires ONLY on a registry MISS for an event
            // that self-describes all four scope identifiers.
            let scope = self
                .runtime_canonical_event_scopes
                .get(&mapped.execution_id)
                .map(|entry| entry.clone())
                .or_else(|| event.self_describing_canonical_scope());

            if let Some(scope) = scope {
                if let Some(sink) = self
                    .runtime_canonical_event_sink
                    .read()
                    .expect("runtime_canonical_event_sink lock poisoned")
                    .clone()
                {
                    sink.emit(scope, mapped.event_type, mapped.payload);
                }
            }
        }
    }

    fn v3_planning_progress_for_event(
        &self,
        event: &RuntimeTransportEvent,
    ) -> Option<RuntimeTransportEvent> {
        let execution_id = event.execution_id_for_scope_enrichment()?;
        if !execution_id.starts_with("planexec_") {
            return None;
        }
        let scope = self
            .v3_planning_transport_scopes
            .get(execution_id)
            .map(|entry| entry.clone())?;
        let (phase, detail, timestamp) = match event {
            RuntimeTransportEvent::MessageProcessingStarted { timestamp, .. } => (
                "starting".to_string(),
                Some("Planning request accepted.".to_string()),
                *timestamp,
            ),
            RuntimeTransportEvent::QueryAnalysisCompleted {
                intent,
                categories,
                timestamp,
                ..
            } => (
                "query_analysis".to_string(),
                Some(format!(
                    "Goal analysis completed: intent={}, categories={}.",
                    intent,
                    categories.len()
                )),
                *timestamp,
            ),
            RuntimeTransportEvent::LLMAnalysisStarted {
                provider,
                stage,
                timestamp,
                ..
            } => (
                stage.clone(),
                Some(format!("Analyzing {stage} with {provider}.")),
                *timestamp,
            ),
            RuntimeTransportEvent::LLMAnalysisCompleted {
                stage,
                duration_ms,
                timestamp,
                ..
            } => (
                stage.clone(),
                Some(format!("{stage} completed in {duration_ms}ms.")),
                *timestamp,
            ),
            RuntimeTransportEvent::LLMAnalysisFailed {
                stage,
                error_message,
                timestamp,
                ..
            } => (
                stage.clone(),
                Some(format!("{stage} failed: {error_message}")),
                *timestamp,
            ),
            RuntimeTransportEvent::ExplorationProgress {
                current_task,
                timestamp,
                ..
            } => (
                "exploration".to_string(),
                Some(current_task.clone()),
                *timestamp,
            ),
            RuntimeTransportEvent::AtomicPlanOutlineStarted {
                total_atomic_tools,
                timestamp,
                ..
            } => (
                "atomic_outline".to_string(),
                Some(format!("Analyzing {total_atomic_tools} atomic tools.")),
                *timestamp,
            ),
            RuntimeTransportEvent::AtomicPlanOutlineCompleted {
                goals_count,
                timestamp,
                ..
            } => (
                "atomic_outline".to_string(),
                Some(format!("Generated outline with {goals_count} goals.")),
                *timestamp,
            ),
            RuntimeTransportEvent::AtomicPlanExpansionStarted {
                goals_from_outline,
                timestamp,
                ..
            } => (
                "atomic_expansion".to_string(),
                Some(format!("Expanding {goals_from_outline} goals into steps.")),
                *timestamp,
            ),
            RuntimeTransportEvent::AtomicPlanGenerated {
                validation_status,
                attempt_number,
                timestamp,
                ..
            } => (
                "atomic_plan".to_string(),
                Some(format!(
                    "Generated atomic plan ({validation_status}, attempt {attempt_number})."
                )),
                *timestamp,
            ),
            RuntimeTransportEvent::ProcessingError {
                error_message,
                timestamp,
                ..
            } => ("error".to_string(), Some(error_message.clone()), *timestamp),
            RuntimeTransportEvent::MessageCompleted { timestamp, .. } => (
                "complete".to_string(),
                Some("Planning execution completed.".to_string()),
                *timestamp,
            ),
            _ => return None,
        };

        Some(RuntimeTransportEvent::V3PlanningProgress {
            principal: scope.principal,
            workspace: scope.workspace,
            task_id: scope.task_id,
            task_title: scope.task_title,
            agent_id: scope.agent_id,
            plan_id: scope.plan_id,
            ui_thread_id: scope.ui_thread_id,
            phase,
            detail,
            timestamp,
        })
    }

    /// Emit a generic agent event envelope to transport subscribers.
    pub fn emit_agent_transport_event(&self, event: AgentEventEnvelope) {
        self.emit_transport_only(RuntimeTransportEvent::AgentEvent { event });
    }

    /// Internal helper: build either a scoped or unscoped envelope and
    /// emit. Keeps the new event helpers terse.
    ///
    /// Standardization: every transport-event payload carries a
    /// canonical `timestamp_ms` (Unix epoch millis) field stamped here
    /// at emit time. Individual `emit_*` helpers may also carry their
    /// own semantic time fields (`started_at`, `finished_at`,
    /// `ended_at`) for clarity, but `timestamp_ms` is the universal
    /// sort key consumers can rely on. If a caller already populated
    /// `timestamp_ms`, we preserve it (lets specific emitters carry
    /// the original event time when re-emitting).
    /// Canonical "any caller can publish any event" entry point.
    ///
    /// Wraps the given `event_type` + `payload` into an
    /// `AgentEventEnvelope` (so the wire format matches every other
    /// GAUI / AGUI event) and dispatches through the broadcaster, plus
    /// the chat-fanout overlay when an `execution_id` in the payload
    /// has a registered chat-thread mirror.
    ///
    /// Use this from any code path that needs to publish a runtime
    /// event. The classification (category, severity, user_relevant)
    /// is looked up by `event_type` string against `GAUI_EVENT_TAXONOMY`
    /// at read time — no need to define new typed helpers per event.
    /// Adding a new `event_type` string means adding a row to that
    /// table so the UI knows how to bucket it.
    ///
    /// This is the *only* public emit primitive. Earlier releases also
    /// exposed typed helpers (`emit_plan_snapshot`, `emit_reasoning_*`,
    /// `emit_tool_call_*`) as sugar over the same internal path. Those
    /// were removed in v0.6.494 in favour of inline `emit_named(...)` +
    /// `serde_json::json!({...})` at the call site — KISS: one rail, one
    /// publish primitive, payload shape lives where the event is emitted.
    pub fn emit_named(
        &self,
        event_type: &str,
        agent_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
        payload: serde_json::Value,
    ) {
        self.emit_scoped_or_unscoped(event_type, agent_id, principal, workspace, payload);
    }

    fn emit_scoped_or_unscoped(
        &self,
        event_type: &str,
        agent_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
        mut payload: serde_json::Value,
    ) {
        if let Some(obj) = payload.as_object_mut() {
            obj.entry("timestamp_ms").or_insert_with(|| {
                serde_json::Value::Number(serde_json::Number::from(
                    chrono::Utc::now().timestamp_millis(),
                ))
            });
        }
        // Fast path: no chat fan-out registrations OR payload carries
        // neither execution_id nor task_id → emit once and return
        // without cloning.
        //
        // We look up fan-out targets under BOTH keys and union the
        // lists. Execution-id lookup serves delegated-agent runs
        // (registered with the delegate's execution_id at delegation
        // time). Task-id lookup serves chat-spawned tasks and
        // `subscribe_to_task` attachments — those register before any
        // execution_id has been allocated. Inner-loop events emitted
        // via `emit_named` always carry both keys in the payload (see
        // `execution/primitive/runner.rs:2006-2022`), so a task fan-
        // out hits even when the event's execution_id is unknown to
        // the chat side.
        // Capture both ids — `exec_id` is used by the slow-path
        // re-stamp loop below to namespace the call_id; `task_id`
        // drives the fan-out lookup.
        let exec_id = payload
            .get("execution_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let task_id_in_payload = payload
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let mut fanout_targets: Vec<ChatFanoutTarget> = Vec::new();
        if let Some(task_id) = task_id_in_payload.as_deref() {
            if let Some(entry) = self.chat_fanout_by_task.get(task_id) {
                for target in entry.value().iter() {
                    if fanout_targets
                        .iter()
                        .any(|existing| existing.chat_session_id == target.chat_session_id)
                    {
                        continue;
                    }
                    fanout_targets.push(target.clone());
                }
            }
        }

        // Stamp `chat_turn_id` onto the primary payload whenever any
        // registered fan-out target for this execution carries one.
        // Covers the case where the primary envelope (`agent_id+scope`)
        // matches the chat target — the clone-loop below skips that
        // pair to avoid duplication, so without this stamp the chat
        // session's own subscription would see the event with no
        // `chat_turn_id`. Idempotent: `entry().or_insert` preserves any
        // value already set at the call site.
        if let Some(turn_id) = fanout_targets
            .iter()
            .find_map(|t| t.chat_turn_id.as_deref())
        {
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("chat_turn_id".to_string())
                    .or_insert_with(|| serde_json::Value::String(turn_id.to_string()));
            }
        }

        if fanout_targets.is_empty() {
            let envelope = match (principal, workspace) {
                (Some(p), Some(w)) => {
                    AgentEventEnvelope::new_scoped(event_type, agent_id, p, w, payload)
                },
                _ => AgentEventEnvelope::new(event_type, agent_id, payload),
            };
            self.emit_agent_transport_event(envelope);
            return;
        }

        // Slow path: at least one chat session is listening to this
        // execution_id. Emit the original envelope first (so anyone
        // already listening to the delegate's agent_id+scope still
        // sees the event), then stamp a copy for each chat fan-out
        // target with the chat's agent_id+scope so its existing
        // scoped subscription picks the event up.
        let primary_payload = crate::magician_v2::json_traversal::clone_json_iteratively(&payload);
        let envelope = match (principal, workspace) {
            (Some(p), Some(w)) => {
                AgentEventEnvelope::new_scoped(event_type, agent_id, p, w, primary_payload)
            },
            _ => AgentEventEnvelope::new(event_type, agent_id, primary_payload),
        };
        self.emit_agent_transport_event(envelope);

        let primary_principal = principal.unwrap_or("");
        let primary_workspace = workspace.unwrap_or("");
        for target in fanout_targets {
            // Skip the stamped copy when the fan-out target identity
            // exactly matches the primary envelope identity (chat agent
            // delegating into a task owned by the same personal agent in
            // the same scope). The primary emit at line 2654 already
            // covers that subscription; emitting again would duplicate
            // every plan.snapshot / tool.call / reasoning event on the
            // chat surface.
            if target.chat_agent_id == agent_id
                && target.principal == primary_principal
                && target.workspace == primary_workspace
            {
                continue;
            }
            let mut payload_copy =
                crate::magician_v2::json_traversal::clone_json_iteratively(&payload);
            if let Some(obj) = payload_copy.as_object_mut() {
                obj.insert(
                    "chat_delivery_kind".to_string(),
                    serde_json::Value::String("inline_delegate".to_string()),
                );
                obj.insert(
                    "chat_session_id".to_string(),
                    serde_json::Value::String(target.chat_session_id.clone()),
                );
                obj.insert(
                    "origin_agent_id".to_string(),
                    serde_json::Value::String(agent_id.to_string()),
                );
                // Carry the spawning chat turn's correlation id onto
                // every fanned-out sub-agent event so the UI's
                // `RequestActivityCard` (subscribed via
                // `/events?chat_turn_id=`) groups Vera's tool calls /
                // reasoning / LLM emissions under the same per-request
                // umbrella as Presto's chat-side events. Skipped when
                // the spawning chat session didn't supply a turn id.
                if let Some(turn_id) = target.chat_turn_id.as_deref() {
                    obj.insert(
                        "chat_turn_id".to_string(),
                        serde_json::Value::String(turn_id.to_string()),
                    );
                }
                // Namespace `call_id` so two LLMs running in parallel
                // can't collide on tool.call.* events when their
                // providers happen to mint identical `call_…` ids.
                // UI dedupe keys cards on call_id; without this, a
                // delegate's tool-call card can overwrite the chat's
                // own card or vice-versa, AND two parallel delegates
                // to the same target agent (e.g. two `web-researcher`
                // delegates) would collide on each other.
                //
                // Use `execution_id` (already in payload, unique per
                // spawn) as the namespace so each delegate gets its
                // own distinct call_id space. The delegate's
                // `agent_id` is preserved separately under
                // `origin_agent_id` for telemetry / display.
                if let Some(orig_call_id) = obj
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
                {
                    // The fan-out fast path bails out when
                    // `payload.execution_id` is missing or doesn't match a
                    // registered delegate, so the slow path is only ever
                    // reached with `Some(exec_id)`. Use that as the
                    // namespace; debug_assert guards the invariant for
                    // future refactors.
                    debug_assert!(
                        exec_id.is_some(),
                        "fan-out slow path expected Some(execution_id); got None"
                    );
                    let namespace = exec_id.as_deref().unwrap_or(agent_id);
                    obj.insert(
                        "call_id".to_string(),
                        serde_json::Value::String(format!(
                            "delegate-{}/{}",
                            namespace, orig_call_id
                        )),
                    );
                    obj.insert(
                        "origin_call_id".to_string(),
                        serde_json::Value::String(orig_call_id),
                    );
                }
            }
            let fan_envelope = AgentEventEnvelope::new_scoped(
                event_type,
                &target.chat_agent_id,
                &target.principal,
                &target.workspace,
                payload_copy,
            );
            self.emit_agent_transport_event(fan_envelope);
        }
    }

    /// Hand out a cycle-safe [`ChatFanoutResolver`] over this
    /// broadcaster's chat fan-out + canonical-scope registries. Used by
    /// `ChatTurnEventSink` to recover the chat-turn id for inner-loop
    /// events that bypass the `emit_scoped_or_unscoped` re-stamp (typed
    /// `Agentic*` / tool / llm variants emitted via `emit` /
    /// `emit_transport_only`), so a delegated task the chat subscribed to
    /// still streams its progress instead of being silently dropped.
    pub fn chat_fanout_resolver(&self) -> ChatFanoutResolver {
        ChatFanoutResolver {
            chat_fanout_by_task: Arc::clone(&self.chat_fanout_by_task),
            canonical_scopes: Arc::clone(&self.runtime_canonical_event_scopes),
        }
    }

    /// Register a chat fan-out keyed on a task_id. Used by
    /// `dispatch_create_task`, `dispatch_capability_pack`,
    /// `dispatch_subscribe_to_task`, `dispatch_delegate_to_agent`,
    /// `dispatch_handover_to_agent` — every chat-spawned execution
    /// flows through a real V3 task, and inner-loop events always
    /// stamp both `task_id` and `execution_id` into their payload,
    /// so the fan-out pass picks them up via the task_id key.
    /// Dedupe-on-chat_session_id keeps steady-state emission at 1×.
    pub fn register_chat_fanout_for_task(&self, task_id: &str, target: ChatFanoutTarget) {
        let mut bucket = self
            .chat_fanout_by_task
            .entry(task_id.to_string())
            .or_default();
        if bucket
            .iter()
            .any(|t| t.chat_session_id == target.chat_session_id)
        {
            return;
        }
        bucket.push(target);
    }

    /// Tear down a chat session's fan-out for a specific task_id.
    /// Called on:
    ///  - terminal task status (the watched task is done — no more
    ///    events worth re-stamping)
    ///  - single-slot subscription replacement (chat subscribes to
    ///    another task, drop the prior one)
    ///  - `delete_session` / `clear_messages` cleanup
    pub fn unregister_chat_fanout_for_task(&self, task_id: &str, chat_session_id: &str) {
        if let Some(mut entry) = self.chat_fanout_by_task.get_mut(task_id) {
            entry.retain(|t| t.chat_session_id != chat_session_id);
        }
        self.chat_fanout_by_task
            .remove_if(task_id, |_, v| v.is_empty());
    }

    /// Subscribe to events
    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeTransportEvent> {
        self.sender.subscribe()
    }

    /// Resolve a live canonical request without relying on task/execution
    /// persistence. Scope is part of the key, so aliases cannot cross tenant
    /// boundaries. Resolutions remove the entry synchronously before their
    /// event reaches UI subscribers.
    pub fn pending_hitl_request(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
    ) -> Option<RuntimeTransportEvent> {
        match self.hitl_lifecycle_state(principal, workspace, correlation_id) {
            HitlLifecycleState::Pending(event) => Some(event),
            _ => None,
        }
    }

    /// Prove that the authoritative resolved state for this exact scoped key
    /// was produced by the same canonical `HitlResolved` event. Compacted V2
    /// proofs retain only this event fingerprint. Legacy V1/key-only proofs
    /// deliberately return `false`: they establish closure, but cannot safely
    /// authorize a publication-debt clear.
    pub fn resolved_hitl_lifecycle_matches(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
        expected: &RuntimeTransportEvent,
    ) -> bool {
        let expected_key = (
            principal.to_string(),
            workspace.to_string(),
            correlation_id.to_string(),
        );
        let Some(key) = scoped_hitl_lifecycle_key(expected) else {
            return false;
        };
        if key != expected_key {
            return false;
        }
        let Some(expected_fingerprint) = hitl_resolved_fingerprint(expected) else {
            return false;
        };
        let RuntimeTransportEvent::HitlResolved {
            timestamp: expected_timestamp,
            ..
        } = expected
        else {
            return false;
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        if self.expire_app_owner_notification_if_due(&key, now_ms)
            && self
                .hitl_lifecycle_persistence
                .as_ref()
                .is_none_or(|persistence| persistence.authority_path.is_none())
        {
            return false;
        }

        if let Some(persistence) = self.hitl_lifecycle_persistence.as_ref() {
            let _publication_guard = match self.acquire_hitl_lifecycle_publication_lock() {
                Ok(guard) => guard,
                Err(_) => return false,
            };
            let Ok(reducer) = prepare_exact_hitl_lifecycle_reducer(persistence) else {
                return false;
            };
            let Ok(Some(existing)) = reducer.existing_record(&key) else {
                return false;
            };
            // Lock acquisition and keyed SQLite reads may outlive a short
            // sealed TTL. Re-read the clock at the authority decision point.
            let authority_now_ms = chrono::Utc::now().timestamp_millis();
            let resolved_state = match existing.state {
                HITL_REDUCER_GENERIC_RESOLVED => {
                    existing.absolute_expires_at_ms.is_none()
                        && existing.app_owner_generation.is_none()
                },
                HITL_REDUCER_APP_RESOLVED => {
                    existing.absolute_expires_at_ms.is_some_and(|expiry| {
                        expiry > authority_now_ms && expiry > existing.timestamp
                    }) && existing
                        .app_owner_generation
                        .as_deref()
                        .and_then(AppOwnerNotificationPublicationGeneration::parse)
                        .is_some()
                },
                _ => false,
            };
            return resolved_state
                && existing.timestamp == *expected_timestamp
                && existing.body.as_deref() == Some(&expected_fingerprint.0[..]);
        }

        if let Some(state) = self.app_owner_notification_lifecycle.get(&key) {
            return state.state == HitlLifecycleProofStateV1::Resolved
                && state.resolved_authority
                    == Some(HitlResolvedAuthority::Verified(expected_fingerprint));
        }
        self.resolved_hitl_requests.get(&key).is_some_and(|state| {
            state.timestamp == *expected_timestamp
                && state.authority == HitlResolvedAuthority::Verified(expected_fingerprint)
        })
    }

    /// Fast exact receipt for generic UserRequest resolution debt. Live
    /// appends and startup V2 replay seed this verified cache; app-family and
    /// legacy V1/unverified authority fail closed.
    pub fn local_generic_hitl_resolution_matches(&self, expected: &RuntimeTransportEvent) -> bool {
        let RuntimeTransportEvent::HitlResolved {
            source, timestamp, ..
        } = expected
        else {
            return false;
        };
        if source != "user_request" {
            return false;
        }
        if self
            .hitl_lifecycle_persistence
            .as_ref()
            .is_some_and(|persistence| persistence.authority_path.is_some())
        {
            let Some((principal, workspace, correlation_id)) = scoped_hitl_lifecycle_key(expected)
            else {
                return false;
            };
            return self.resolved_hitl_lifecycle_matches(
                &principal,
                &workspace,
                &correlation_id,
                expected,
            );
        }
        let Some(key) = scoped_hitl_lifecycle_key(expected) else {
            return false;
        };
        let _lifecycle_guard = self
            .hitl_lifecycle_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !self.hitl_lifecycle_recovery_healthy.load(Ordering::SeqCst) {
            return false;
        }
        if self.app_owner_notification_lifecycle.contains_key(&key) {
            return false;
        }
        let Some(fingerprint) = hitl_resolved_fingerprint(expected) else {
            return false;
        };
        self.resolved_hitl_requests.get(&key).is_some_and(|state| {
            state.timestamp == *timestamp
                && state.authority == HitlResolvedAuthority::Verified(fingerprint)
        })
    }

    fn exact_hitl_lifecycle_state_from_authority(
        &self,
        key: &HitlLifecycleKey,
        now_ms: i64,
    ) -> Option<HitlLifecycleState> {
        let persistence = self.hitl_lifecycle_persistence.as_ref()?;
        persistence.authority_path.as_ref()?;
        let _publication_guard = match self.acquire_hitl_lifecycle_publication_lock() {
            Ok(guard) => guard,
            Err(_) => return Some(HitlLifecycleState::Unavailable),
        };
        let reducer = match prepare_exact_hitl_lifecycle_reducer(persistence) {
            Ok(reducer) => reducer,
            Err(_) => return Some(HitlLifecycleState::Unavailable),
        };
        let record = match reducer.existing_record(key) {
            Ok(record) => record,
            Err(_) => return Some(HitlLifecycleState::Unavailable),
        };
        let Some(record) = record else {
            return Some(HitlLifecycleState::Unknown);
        };
        // Do not let a long publication-lock wait turn a pre-lock timestamp
        // into post-expiry app authority.
        let now_ms = chrono::Utc::now().timestamp_millis().max(now_ms);
        let invalid = || {
            self.hitl_lifecycle_recovery_healthy
                .store(false, Ordering::SeqCst);
            HitlLifecycleState::Unavailable
        };
        Some(match record.state {
            HITL_REDUCER_GENERIC_PENDING => {
                if record.absolute_expires_at_ms.is_some() || record.app_owner_generation.is_some()
                {
                    return Some(invalid());
                }
                let Some(body) = record.body else {
                    return Some(invalid());
                };
                if body.len() > MAX_HITL_LIFECYCLE_RECORD_BYTES
                    || !json_bytes_nesting_is_bounded(&body, MAX_RETAINED_JSON_DEPTH)
                    || !json_bytes_nodes_are_bounded(&body, MAX_HITL_LIFECYCLE_RECORD_NODES)
                {
                    return Some(invalid());
                }
                let Ok(event) = serde_json::from_slice::<RuntimeTransportEvent>(&body) else {
                    return Some(invalid());
                };
                if scoped_hitl_lifecycle_key(&event).as_ref() != Some(key)
                    || !matches!(
                        &event,
                        RuntimeTransportEvent::HitlRequested { timestamp, .. }
                            if *timestamp == record.timestamp
                    )
                    || app_owner_notification_expiry_ms(&event) != Ok(None)
                {
                    return Some(invalid());
                }
                HitlLifecycleState::Pending(event)
            },
            HITL_REDUCER_GENERIC_RESOLVED => {
                if record.absolute_expires_at_ms.is_some()
                    || record.app_owner_generation.is_some()
                    || record.body.as_ref().is_some_and(|body| body.len() != 32)
                {
                    return Some(invalid());
                }
                HitlLifecycleState::Resolved
            },
            HITL_REDUCER_APP_PENDING => {
                let Some(expiry) = record.absolute_expires_at_ms else {
                    return Some(invalid());
                };
                let durable_generation = match record.app_owner_generation.as_deref() {
                    Some(value) => {
                        let Some(generation) =
                            AppOwnerNotificationPublicationGeneration::parse(value)
                        else {
                            return Some(invalid());
                        };
                        Some(generation)
                    },
                    None => None,
                };
                if expiry <= record.timestamp {
                    return Some(invalid());
                }
                if record.body.is_some() || expiry <= now_ms {
                    HitlLifecycleState::Unknown
                } else if let Some(local) = self.app_owner_notification_lifecycle.get(key) {
                    if local.state != HitlLifecycleProofStateV1::PendingRedacted
                        || local.timestamp != record.timestamp
                        || local.absolute_expires_at_ms != expiry
                        || local.app_owner_generation != durable_generation
                    {
                        HitlLifecycleState::Unknown
                    } else {
                        drop(local);
                        self.pending_hitl_requests
                            .get(key)
                            .and_then(|event| {
                                (app_owner_notification_expiry_ms(event.value())
                                    == Ok(Some(expiry))
                                    && scoped_hitl_lifecycle_key(event.value()).as_ref()
                                        == Some(key)
                                    && matches!(
                                        event.value(),
                                        RuntimeTransportEvent::HitlRequested {
                                            timestamp,
                                            ..
                                        } if *timestamp == record.timestamp
                                    ))
                                .then(|| clone_hitl_lifecycle_event_iteratively(event.value()))
                                .flatten()
                            })
                            .map(HitlLifecycleState::Pending)
                            .unwrap_or(HitlLifecycleState::Unknown)
                    }
                } else {
                    HitlLifecycleState::Unknown
                }
            },
            HITL_REDUCER_APP_RESOLVED => {
                let Some(expiry) = record.absolute_expires_at_ms else {
                    return Some(invalid());
                };
                if expiry <= record.timestamp {
                    return Some(invalid());
                }
                if record.body.as_ref().is_some_and(|body| body.len() != 32) {
                    return Some(invalid());
                }
                if let Some(value) = record.app_owner_generation.as_deref() {
                    if AppOwnerNotificationPublicationGeneration::parse(value).is_none() {
                        return Some(invalid());
                    }
                }
                if expiry <= now_ms {
                    HitlLifecycleState::Unknown
                } else {
                    HitlLifecycleState::Resolved
                }
            },
            _ => invalid(),
        })
    }

    /// Process-local projection of lifecycle state recovered or reconciled
    /// from the journal. Publication-debt owners that need current shared-disk
    /// authority use the exact batch reconciliation APIs instead. Known local
    /// keys remain usable after a later corrupt row; unknown keys fail closed
    /// while recovery health is false.
    pub fn hitl_lifecycle_state(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
    ) -> HitlLifecycleState {
        let key = (
            principal.to_string(),
            workspace.to_string(),
            correlation_id.to_string(),
        );
        let now_ms = chrono::Utc::now().timestamp_millis();
        if self.expire_app_owner_notification_if_due(&key, now_ms)
            && self
                .hitl_lifecycle_persistence
                .as_ref()
                .is_none_or(|persistence| persistence.authority_path.is_none())
        {
            return HitlLifecycleState::Unknown;
        }
        if let Some(state) = self.exact_hitl_lifecycle_state_from_authority(&key, now_ms) {
            return state;
        }
        if let Some(app_state) = self.app_owner_notification_lifecycle.get(&key) {
            match app_state.state {
                HitlLifecycleProofStateV1::Resolved => {
                    return HitlLifecycleState::Resolved;
                },
                HitlLifecycleProofStateV1::PendingRedacted => {
                    // A proof alone never reconstructs app-controlled content.
                    // Only a still-live body republished by UserRequestService
                    // after its own durable restore can satisfy exact lookup.
                    let absolute_expires_at_ms = app_state.absolute_expires_at_ms;
                    drop(app_state);
                    let Some(event) = self.pending_hitl_requests.get(&key) else {
                        return HitlLifecycleState::Unknown;
                    };
                    if app_owner_notification_expiry_ms(event.value())
                        != Ok(Some(absolute_expires_at_ms))
                        || absolute_expires_at_ms <= now_ms
                    {
                        return HitlLifecycleState::Unavailable;
                    }
                    return clone_hitl_lifecycle_event_iteratively(event.value())
                        .map(HitlLifecycleState::Pending)
                        .unwrap_or(HitlLifecycleState::Unavailable);
                },
            }
        }
        if let Some(event) = self.pending_hitl_requests.get(&key) {
            return clone_hitl_lifecycle_event_iteratively(event.value())
                .map(HitlLifecycleState::Pending)
                .unwrap_or(HitlLifecycleState::Unavailable);
        }
        if self.resolved_hitl_requests.contains_key(&key) {
            return HitlLifecycleState::Resolved;
        }
        if !self.hitl_lifecycle_recovery_healthy.load(Ordering::SeqCst) {
            return HitlLifecycleState::Unavailable;
        }
        HitlLifecycleState::Unknown
    }

    /// Get subscriber count
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// Helper: Broadcast message started
    pub fn message_started(&self, execution_id: &str, turn_id: &str, correlation_id: &str) {
        self.emit(RuntimeTransportEvent::MessageProcessingStarted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            turn_id: turn_id.to_string(),
            correlation_id: correlation_id.to_string(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast query analysis completed
    pub fn query_analysis_completed(
        &self,
        execution_id: &str,
        correlation_id: &str,
        complexity_score: f64,
        intent: String,
        categories: Vec<String>,
    ) {
        self.emit(RuntimeTransportEvent::QueryAnalysisCompleted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            complexity_score,
            intent,
            categories,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast strategy selected
    pub fn strategy_selected(
        &self,
        execution_id: &str,
        correlation_id: &str,
        strategy: StrategyType,
        confidence: f64,
        reason: String,
    ) {
        self.emit(RuntimeTransportEvent::StrategySelected {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            strategy,
            confidence,
            reason,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast exploration progress
    pub fn exploration_progress(
        &self,
        execution_id: &str,
        correlation_id: &str,
        nodes_explored: usize,
        current_depth: usize,
        best_score: f64,
        current_task: String,
        progress_percent: f64,
    ) {
        self.emit(RuntimeTransportEvent::ExplorationProgress {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            nodes_explored,
            current_depth,
            best_score,
            current_task,
            progress_percent,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast atomic plan outline started
    pub fn atomic_plan_outline_started(
        &self,
        execution_id: &str,
        correlation_id: &str,
        total_atomic_tools: usize,
    ) {
        self.emit(RuntimeTransportEvent::AtomicPlanOutlineStarted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            total_atomic_tools,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast atomic plan outline completed
    pub fn atomic_plan_outline_completed(
        &self,
        execution_id: &str,
        correlation_id: &str,
        goals_count: usize,
        confidence: f64,
    ) {
        self.emit(RuntimeTransportEvent::AtomicPlanOutlineCompleted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            goals_count,
            confidence,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast atomic plan expansion started
    pub fn atomic_plan_expansion_started(
        &self,
        execution_id: &str,
        correlation_id: &str,
        goals_from_outline: usize,
    ) {
        self.emit(RuntimeTransportEvent::AtomicPlanExpansionStarted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            goals_from_outline,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast atomic plan generated
    pub fn atomic_plan_generated(
        &self,
        execution_id: &str,
        correlation_id: &str,
        turn_id: &str,
        plan_graph: crate::magician_v2::strategy::PlanGraph,
        validation_status: String,
        attempt_number: u32,
    ) {
        self.emit(RuntimeTransportEvent::AtomicPlanGenerated {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            turn_id: turn_id.to_string(),
            plan_graph,
            validation_status,
            attempt_number,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast clarification queued event.
    ///
    /// Phase H4.1 — dual-emits canonical `HitlRequested`. H4.2/H4.3 —
    /// `task_id` and `question_text` are optional: callers that have
    /// them in scope pass them through so the canonical event carries
    /// proper task scope + prompt UI text; callers without them get the
    /// canonical event with `task_id: None` and an empty prompt (frontend
    /// surfaces still re-fetch the persisted `TaskPlanQuestion` for the
    /// full UI shape in either case).
    #[allow(clippy::too_many_arguments)]
    pub fn clarification_queued(
        &self,
        execution_id: &str,
        question_id: &str,
        blocker_type: impl Into<String>,
        channel: impl Into<String>,
        stage: impl Into<String>,
        urgency: f64,
        task_id: Option<&str>,
        question_text: Option<&str>,
        source_slot_id: Option<&str>,
    ) {
        let blocker_str: String = blocker_type.into();
        let channel_str: String = channel.into();
        let stage_str: String = stage.into();
        let timestamp = chrono::Utc::now().timestamp_millis();
        // Phase H6.2 — legacy `ClarificationQueued` typed emit
        // retired. Canonical `HitlRequested` below carries blocker /
        // channel / stage / urgency on `input_schema`; `pendingHitlStore`
        // + Plan Inspector consume canonical via H4 already.
        // Phase H4.1/H4.2/H4.3 — canonical emit.
        let mut input_schema = serde_json::json!({
            "stage": stage_str,
            "blocker_type": blocker_str,
            "channel": channel_str,
            "urgency": urgency,
        });
        if let Some(slot) = source_slot_id {
            if let Some(map) = input_schema.as_object_mut() {
                map.insert(
                    "source_slot_id".to_string(),
                    serde_json::Value::String(slot.to_string()),
                );
            }
        }
        self.emit(RuntimeTransportEvent::HitlRequested {
            correlation_id: question_id.to_string(),
            source: "clarification".to_string(),
            input_type: "text".to_string(),
            prompt: question_text.unwrap_or("").to_string(),
            hint: None,
            input_schema: Some(input_schema),
            task_id: task_id.map(|s| s.to_string()),
            execution_id: Some(execution_id.to_string()),
            agent_id: None,
            principal: None,
            workspace: None,
            timestamp,
        });
    }

    /// Helper: Broadcast clarification response received
    pub fn clarification_response_received(&self, execution_id: &str, question_id: &str) {
        let timestamp = chrono::Utc::now().timestamp_millis();
        // Phase H6.2 — legacy `ClarificationResponseReceived` emit
        // retired. Canonical `HitlResolved` below decrements
        // `pendingHitlStore` via `correlation_id == question_id`.
        // Phase H4 — canonical emit. The pendingHitlStore decrements
        // its pending count on this resolution; matches the
        // correlation_id from the corresponding HitlRequested.
        self.emit(RuntimeTransportEvent::HitlResolved {
            correlation_id: question_id.to_string(),
            source: "clarification".to_string(),
            outcome: "responded".to_string(),
            decision: None,
            task_id: None,
            execution_id: Some(execution_id.to_string()),
            agent_id: None,
            principal: None,
            workspace: None,
            timestamp,
        });
    }

    /// Helper: Broadcast clarification session snapshot metrics
    pub fn clarification_session_snapshot(
        &self,
        execution_id: &str,
        state: impl Into<String>,
        total_questions: usize,
        waiting_on_user: usize,
        queued: usize,
        answered: usize,
        cancelled: usize,
        pending_question_ids: Vec<String>,
        active_batch: Option<ClarificationBatchSnapshot>,
        last_question_asked_at: Option<i64>,
    ) {
        self.emit(RuntimeTransportEvent::ClarificationSessionSnapshot {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            state: state.into(),
            total_questions,
            waiting_on_user,
            queued,
            answered,
            cancelled,
            pending_question_ids,
            active_batch,
            last_question_asked_at,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast confidence delta snapshot after clarification answers
    pub fn clarification_confidence_snapshot(
        &self,
        execution_id: &str,
        question_id: &str,
        trigger: impl Into<String>,
        overall_confidence: f64,
        unresolved_count: usize,
        slot_deltas: Vec<ConfidenceSlotDelta>,
        question_created_at: i64,
        answered_at: i64,
    ) {
        self.emit(RuntimeTransportEvent::ClarificationConfidenceSnapshot {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            question_id: question_id.to_string(),
            trigger: trigger.into(),
            overall_confidence,
            unresolved_count,
            slot_deltas,
            question_created_at,
            answered_at,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast slot graph diff telemetry
    pub fn slot_graph_diff(
        &self,
        execution_id: &str,
        source: &str,
        inserted: usize,
        updated: usize,
        removed: usize,
        total_slots: usize,
    ) {
        self.emit(RuntimeTransportEvent::SlotGraphDiff {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            source: source.to_string(),
            inserted,
            updated,
            removed,
            total_slots,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast workflow resumed event
    pub fn workflow_resumed(
        &self,
        execution_id: &str,
        question_id: Option<String>,
        resume_mode: impl Into<String>,
        answered_count: usize,
        pending_count: usize,
    ) {
        self.emit(RuntimeTransportEvent::WorkflowResumed {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            question_id,
            resume_mode: resume_mode.into(),
            answered_count,
            pending_count,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast structured stage resume telemetry
    pub fn workflow_stage_resumed(
        &self,
        execution_id: &str,
        stage_name: impl Into<String>,
        stage_context: StageContext,
        attempt: u32,
        reused_checkpoint: bool,
        checkpoint_hash: Option<String>,
        reused_stages: Vec<String>,
    ) {
        self.emit(RuntimeTransportEvent::WorkflowStageResumed {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            stage_name: stage_name.into(),
            stage_context: stage_context.as_str().to_string(),
            attempt,
            reused_checkpoint,
            checkpoint_hash,
            reused_stages,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast observability alerts (stalled sessions, outages, etc.)
    pub fn observability_alert(
        &self,
        execution_id: &str,
        alert_type: impl Into<String>,
        details: Value,
    ) {
        self.emit(RuntimeTransportEvent::ObservabilityAlert {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            alert_type: alert_type.into(),
            details,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    pub fn clarification_metrics_snapshot(&self, snapshot: ClarificationMetricsSnapshot) {
        self.emit(RuntimeTransportEvent::ClarificationMetricsSnapshot {
            total_sessions_started: snapshot.total_sessions_started,
            total_sessions_completed: snapshot.total_sessions_completed,
            active_sessions: snapshot.active_sessions,
            avg_session_duration_ms: snapshot.avg_session_duration_ms,
            avg_questions_per_session: snapshot.avg_questions_per_session,
            guardrail_timeouts: snapshot.guardrail_timeouts,
            guardrail_question_caps: snapshot.guardrail_question_caps,
            guardrail_round_caps: snapshot.guardrail_round_caps,
            timestamp: snapshot.timestamp_ms,
        });
    }

    /// Helper: Broadcast workflow resume failure
    pub fn workflow_resume_failed(
        &self,
        execution_id: &str,
        question_id: Option<String>,
        error: impl Into<String>,
    ) {
        self.emit(RuntimeTransportEvent::WorkflowResumeFailed {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            question_id,
            error: error.into(),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast tool matching
    // NOTE: tool_matching helper removed - ToolMatching event superseded by ToolMatchingTierStarted/Completed

    /// Helper: Broadcast message completed
    pub fn message_completed(
        &self,
        execution_id: &str,
        turn_id: &str,
        correlation_id: &str,
        response: String,
        exploration_result: Option<&ExplorationResult>,
    ) {
        self.emit(RuntimeTransportEvent::MessageCompleted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            turn_id: turn_id.to_string(),
            correlation_id: correlation_id.to_string(),
            response,
            exploration_summary: exploration_result.map(ExplorationSummary::from),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast processing cancelled
    // NOTE: processing_cancelled helper removed - ExecutionCancelled covers cancellation

    /// Helper: Broadcast processing error
    pub fn processing_error(
        &self,
        execution_id: &str,
        correlation_id: &str,
        error_message: String,
        error_type: String,
    ) {
        self.emit(RuntimeTransportEvent::ProcessingError {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            error_message,
            error_type,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast LLM analysis started
    pub fn llm_analysis_started(
        &self,
        execution_id: &str,
        correlation_id: &str,
        provider: String,
        stage: String,
        query_length: usize,
    ) {
        // Broadcast event
        self.emit(RuntimeTransportEvent::LLMAnalysisStarted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            provider: provider.clone(),
            stage: stage.clone(),
            query_length,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });

        // Persist stage and provider to execution storage
        if let Some(store) = &self.conversation_store {
            let store = Arc::clone(store);
            let tid = execution_id.to_string();
            let stage_opt = Some(stage);
            let provider_opt = Some(provider);

            tokio::spawn(async move {
                if let Err(e) = store
                    .update_execution_processing_stage(&tid, stage_opt, provider_opt)
                    .await
                {
                    tracing::warn!("Failed to persist processing stage: {}", e);
                }
            });
        }
    }

    /// Helper: Broadcast LLM analysis completed
    pub fn llm_analysis_completed(
        &self,
        execution_id: &str,
        correlation_id: &str,
        provider: String,
        stage: String,
        response_length: usize,
        duration_ms: u64,
    ) {
        // Broadcast event
        self.emit(RuntimeTransportEvent::LLMAnalysisCompleted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            provider,
            stage,
            response_length,
            duration_ms,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });

        // Clear stage and provider from execution storage
        if let Some(store) = &self.conversation_store {
            let store = Arc::clone(store);
            let tid = execution_id.to_string();

            tokio::spawn(async move {
                if let Err(e) = store
                    .update_execution_processing_stage(&tid, None, None)
                    .await
                {
                    tracing::warn!("Failed to clear processing stage: {}", e);
                }
            });
        }
    }

    /// Helper: Broadcast LLM analysis failed
    pub fn llm_analysis_failed(
        &self,
        execution_id: &str,
        correlation_id: &str,
        provider: String,
        stage: String,
        error_type: String,
        error_message: String,
    ) {
        // Broadcast event
        self.emit(RuntimeTransportEvent::LLMAnalysisFailed {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            provider,
            stage,
            error_type,
            error_message,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });

        // Clear stage and provider from execution storage
        if let Some(store) = &self.conversation_store {
            let store = Arc::clone(store);
            let tid = execution_id.to_string();

            tokio::spawn(async move {
                if let Err(e) = store
                    .update_execution_processing_stage(&tid, None, None)
                    .await
                {
                    tracing::warn!("Failed to clear processing stage: {}", e);
                }
            });
        }
    }

    /// Helper: Send heartbeat
    pub fn heartbeat(&self) {
        self.emit(RuntimeTransportEvent::Heartbeat {
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast tool matching tier started
    pub fn tool_matching_tier_started(
        &self,
        execution_id: &str,
        correlation_id: &str,
        tier_number: u8,
        tier_name: String,
        description: String,
    ) {
        self.emit(RuntimeTransportEvent::ToolMatchingTierStarted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            tier_number,
            tier_name,
            description,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast tool matching tier completed
    pub fn tool_matching_tier_completed(
        &self,
        execution_id: &str,
        correlation_id: &str,
        tier_number: u8,
        tier_name: String,
        candidates_count: usize,
        duration_ms: u64,
        top_candidates: Vec<TierCandidate>,
    ) {
        self.emit(RuntimeTransportEvent::ToolMatchingTierCompleted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            tier_number,
            tier_name,
            candidates_count,
            duration_ms,
            top_candidates,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast category fuzzy matching
    // NOTE: category_fuzzy_matching helper removed - superseded by ExplorationProgress

    /// Helper: Broadcast slot extraction started
    pub fn slot_extraction_started(
        &self,
        execution_id: &str,
        correlation_id: &str,
        message_length: usize,
    ) {
        self.emit(RuntimeTransportEvent::SlotExtractionStarted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            message_length,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast individual slot extracted
    pub fn slot_extracted(
        &self,
        execution_id: &str,
        correlation_id: &str,
        slot_id: String,
        slot_type: String,
        confidence: f64,
    ) {
        self.emit(RuntimeTransportEvent::SlotExtracted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            slot_id,
            slot_type,
            confidence,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast slot enrichment started
    pub fn slot_enrichment_started(
        &self,
        execution_id: &str,
        correlation_id: &str,
        total_slots: usize,
        enricher_count: usize,
    ) {
        self.emit(RuntimeTransportEvent::SlotEnrichmentStarted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            total_slots,
            enricher_count,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast slot enrichment completed
    pub fn slot_enrichment_completed(
        &self,
        execution_id: &str,
        correlation_id: &str,
        total_slots: usize,
        slots_changed: usize,
        invocations: usize,
        errors_count: usize,
    ) {
        self.emit(RuntimeTransportEvent::SlotEnrichmentCompleted {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            total_slots,
            slots_changed,
            invocations,
            errors_count,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast slot confidence updated
    pub fn slot_confidence_updated(
        &self,
        execution_id: &str,
        correlation_id: &str,
        slot_id: String,
        old_confidence: f64,
        new_confidence: f64,
        source: String,
    ) {
        self.emit(RuntimeTransportEvent::SlotConfidenceUpdated {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            slot_id,
            old_confidence,
            new_confidence,
            source,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast clarified task ready
    pub fn clarified_task_ready(
        &self,
        execution_id: &str,
        correlation_id: &str,
        clarified_task: String,
        objectives_count: usize,
        constraints_count: usize,
        confidence: f64,
    ) {
        self.emit(RuntimeTransportEvent::ClarifiedTaskReady {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            correlation_id: correlation_id.to_string(),
            clarified_task,
            objectives_count,
            constraints_count,
            confidence,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    // Progressive Elicitation Helper Methods

    /// Helper: Broadcast parameter inference attempted
    pub fn parameter_inference_attempted(
        &self,
        execution_id: &str,
        parameter_name: String,
        priority: String,
    ) {
        self.emit(RuntimeTransportEvent::ParameterInferenceAttempted {
            execution_id: execution_id.to_string(),
            parameter_name,
            priority,
            principal: None,
            workspace: None,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast parameter successfully inferred
    pub fn parameter_inferred(
        &self,
        execution_id: &str,
        parameter_name: String,
        inferred_value: serde_json::Value,
        confidence: f64,
        method: String,
    ) {
        self.emit(RuntimeTransportEvent::ParameterInferred {
            execution_id: execution_id.to_string(),
            parameter_name,
            inferred_value,
            confidence,
            method,
            principal: None,
            workspace: None,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast parameter inference failed
    pub fn parameter_inference_failed(
        &self,
        execution_id: &str,
        parameter_name: String,
        confidence: f64,
        reason: String,
    ) {
        self.emit(RuntimeTransportEvent::ParameterInferenceFailed {
            execution_id: execution_id.to_string(),
            parameter_name,
            confidence,
            reason,
            principal: None,
            workspace: None,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast parameter discovery attempted
    pub fn parameter_discovery_attempted(
        &self,
        execution_id: &str,
        parameter_name: String,
        discovery_method: String,
    ) {
        self.emit(RuntimeTransportEvent::ParameterDiscoveryAttempted {
            execution_id: execution_id.to_string(),
            parameter_name,
            discovery_method,
            principal: None,
            workspace: None,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast parameter discovered successfully
    pub fn parameter_discovered(
        &self,
        execution_id: &str,
        parameter_name: String,
        discovered_value: serde_json::Value,
        confidence: f64,
        discovery_method: String,
        external_actions_performed: bool,
    ) {
        self.emit(RuntimeTransportEvent::ParameterDiscovered {
            execution_id: execution_id.to_string(),
            parameter_name,
            discovered_value,
            confidence,
            discovery_method,
            external_actions_performed,
            principal: None,
            workspace: None,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast parameter discovery failed
    pub fn parameter_discovery_failed(
        &self,
        execution_id: &str,
        parameter_name: String,
        reason: String,
    ) {
        self.emit(RuntimeTransportEvent::ParameterDiscoveryFailed {
            execution_id: execution_id.to_string(),
            parameter_name,
            reason,
            principal: None,
            workspace: None,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast sub-goal requested
    pub fn sub_goal_requested(
        &self,
        execution_id: &str,
        plan_id: &str,
        parent_step_id: &str,
        sub_goal: &str,
        budget_iterations: usize,
        depth: usize,
    ) {
        self.emit(RuntimeTransportEvent::SubGoalRequested {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            plan_id: plan_id.to_string(),
            parent_step_id: parent_step_id.to_string(),
            sub_goal: sub_goal.to_string(),
            budget_iterations,
            depth,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// Helper: Broadcast sub-goal outcome
    pub fn sub_goal_outcome(
        &self,
        execution_id: &str,
        plan_id: &str,
        parent_step_id: &str,
        sub_goal: &str,
        outcome: String,
        iterations_used: usize,
        duration_ms: u64,
    ) {
        self.emit(RuntimeTransportEvent::SubGoalOutcome {
            execution_id: execution_id.to_string(),
            principal: None,
            workspace: None,
            plan_id: plan_id.to_string(),
            parent_step_id: parent_step_id.to_string(),
            sub_goal: sub_goal.to_string(),
            outcome,
            iterations_used,
            duration_ms,
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    // Pipeline lifecycle event helpers (M7)

    pub fn pipeline_started(
        &self,
        workflow_id: &str,
        chain_id: &str,
        max_iterations: u32,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) {
        self.emit(RuntimeTransportEvent::PipelineStarted {
            workflow_id: workflow_id.to_string(),
            chain_id: chain_id.to_string(),
            max_iterations,
            principal: principal.map(str::to_string),
            workspace: workspace.map(str::to_string),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    pub fn pipeline_step_started(
        &self,
        workflow_id: &str,
        step_id: &str,
        agent_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) {
        self.emit(RuntimeTransportEvent::PipelineStepStarted {
            workflow_id: workflow_id.to_string(),
            step_id: step_id.to_string(),
            agent_id: agent_id.to_string(),
            principal: principal.map(str::to_string),
            workspace: workspace.map(str::to_string),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    pub fn pipeline_step_completed(
        &self,
        workflow_id: &str,
        step_id: &str,
        agent_id: &str,
        outcome_kind: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) {
        self.emit(RuntimeTransportEvent::PipelineStepCompleted {
            workflow_id: workflow_id.to_string(),
            step_id: step_id.to_string(),
            agent_id: agent_id.to_string(),
            outcome_kind: outcome_kind.to_string(),
            principal: principal.map(str::to_string),
            workspace: workspace.map(str::to_string),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    pub fn pipeline_completed(
        &self,
        workflow_id: &str,
        chain_id: &str,
        steps_executed: usize,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) {
        self.emit(RuntimeTransportEvent::PipelineCompleted {
            workflow_id: workflow_id.to_string(),
            chain_id: chain_id.to_string(),
            steps_executed,
            principal: principal.map(str::to_string),
            workspace: workspace.map(str::to_string),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    pub fn pipeline_failed(
        &self,
        workflow_id: &str,
        chain_id: &str,
        reason: &str,
        steps_executed: usize,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) {
        self.emit(RuntimeTransportEvent::PipelineFailed {
            workflow_id: workflow_id.to_string(),
            chain_id: chain_id.to_string(),
            reason: reason.to_string(),
            steps_executed,
            principal: principal.map(str::to_string),
            workspace: workspace.map(str::to_string),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }
}

fn structured_response_drop_reason_code(reason: &StructuredResponseDropReason) -> &'static str {
    match reason {
        StructuredResponseDropReason::UnsupportedSchema => "unsupported_schema",
        StructuredResponseDropReason::UnsupportedVersion => "unsupported_version",
        StructuredResponseDropReason::MissingSchema => "missing_schema",
        StructuredResponseDropReason::MissingBlocks => "missing_blocks",
        StructuredResponseDropReason::TooManyBlocks => "too_many_blocks",
        StructuredResponseDropReason::TooManyActions => "too_many_actions",
        StructuredResponseDropReason::TooManyTableColumns => "too_many_table_columns",
        StructuredResponseDropReason::TooManyTableRows => "too_many_table_rows",
        StructuredResponseDropReason::TooManyListLikeItems => "too_many_list_like_items",
        StructuredResponseDropReason::TextTooLarge => "text_too_large",
        StructuredResponseDropReason::PresentationTooLarge => "presentation_too_large",
        StructuredResponseDropReason::UnsupportedUrl => "unsupported_url",
        StructuredResponseDropReason::InvalidText => "invalid_text",
        StructuredResponseDropReason::InvalidNumericValue => "invalid_numeric_value",
        StructuredResponseDropReason::InvalidConfidence => "invalid_confidence",
        StructuredResponseDropReason::MissingPlainText => "missing_plain_text",
        StructuredResponseDropReason::PlainTextMismatch => "plain_text_mismatch",
        StructuredResponseDropReason::TableShapeMismatch => "table_shape_mismatch",
        StructuredResponseDropReason::DuplicateTableColumnKey => "duplicate_table_column_key",
        StructuredResponseDropReason::InvalidFieldLength => "invalid_field_length",
        StructuredResponseDropReason::UnsafeControlCharacter => "unsafe_control_character",
        StructuredResponseDropReason::ServerActionsUnsupported => "server_actions_unsupported",
    }
}

impl magician_pty::PtyEventSink for RuntimeTransportBroadcaster {
    fn emit_pty_chunk(&self, event: magician_pty::PtyChunkEvent) {
        self.emit_transport_only(RuntimeTransportEvent::InteractivePtyChunk {
            session_id: event.session_id,
            principal: event.principal,
            workspace: event.workspace,
            ui_thread_id: event.ui_thread_id,
            program: event.program,
            offset_start: event.offset_start,
            offset_end: event.offset_end,
            bytes_b64: event.bytes_b64,
            timestamp_ms: event.timestamp_ms,
        });
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {

    /// A temp directory with no symlinked path component.
    ///
    /// macOS resolves `std::env::temp_dir()` to `/var/folders/…`, and `/var` is
    /// a symlink to `/private/var`. The HITL lifecycle authority opens its
    /// SQLite file with `SQLITE_OPEN_NOFOLLOW` and validates private-path
    /// identity, so it correctly refuses a path reached through a symlink —
    /// the test then fails as "unable to open database file", which reads like
    /// a missing file rather than the hardening doing its job.
    ///
    /// Canonicalising the root keeps that hardening under test instead of
    /// weakening it. `magician-api`'s `canonical_tempdir` exists for the same
    /// reason.
    fn canonical_workspace_tempdir() -> tempfile::TempDir {
        let root = std::fs::canonicalize(std::env::temp_dir()).expect("canonical temporary root");
        tempfile::tempdir_in(root).expect("temporary workspace")
    }

    use super::*;

    /// Drift gate: every `ArtifactV2EventType` variant must have a row in
    /// `GAUI_EVENT_TAXONOMY`, otherwise the runtime emits events whose
    /// `event_type` string falls through `taxonomyFor()` to the catch-all
    /// `observability / info / false` and the `/events` UI shows them
    /// uncategorized.
    ///
    /// Adding a new variant to `ArtifactV2EventType` without a matching
    /// row here fails this test loudly.
    #[test]
    fn gaui_taxonomy_covers_every_artifact_v2_event_type() {
        use crate::magician_v2::artifact_v2::events::ArtifactV2EventType;
        let table_keys: std::collections::HashSet<&'static str> =
            GAUI_EVENT_TAXONOMY.iter().map(|(name, _)| *name).collect();
        let missing: Vec<&'static str> = ArtifactV2EventType::ALL
            .iter()
            .map(|et| et.as_str())
            .filter(|name| !table_keys.contains(name))
            .collect();
        assert!(
            missing.is_empty(),
            "GAUI_EVENT_TAXONOMY is missing rows for these ArtifactV2EventType \
             variants — add them in `realtime_events.rs` and re-run \
             `make event-taxonomy-codegen`: {missing:?}"
        );
    }

    /// Drift gate: every `RuntimeAgentEventType` variant must have a row in
    /// `GAUI_EVENT_TAXONOMY`. Catches new variants that forget to
    /// register with the taxonomy table.
    #[test]
    fn gaui_taxonomy_covers_every_runtime_agent_event_type() {
        let table_keys: std::collections::HashSet<&'static str> =
            GAUI_EVENT_TAXONOMY.iter().map(|(name, _)| *name).collect();
        let missing: Vec<&'static str> = RuntimeAgentEventType::ALL
            .iter()
            .map(|et| et.as_str())
            .filter(|name| !table_keys.contains(name))
            .collect();
        assert!(
            missing.is_empty(),
            "GAUI_EVENT_TAXONOMY is missing rows for these RuntimeAgentEventType \
             variants: {missing:?}"
        );
    }

    /// Reverse drift gate: every `GAUI_EVENT_TAXONOMY` row outside of
    /// `ArtifactV2EventType` must correspond to a `RuntimeAgentEventType`
    /// variant. Catches taxonomy rows added with bare string literals
    /// instead of via the typed enum (the pattern that motivated this
    /// refactor).
    ///
    /// # The direction all three gates in this module share, and do not cover
    ///
    /// "Reverse" is relative to its two neighbours, not absolute. This one
    /// checks *row → variant* and they check *variant → row*; between them the
    /// enum and the table are made to agree with each other, and **none of the
    /// three looks at what the code emits**. A name that never becomes either a
    /// row or a variant is invisible to all of them, and every surface router in
    /// `progress_channel_seam::surface_routing` fails CLOSED on an unknown
    /// name — so such an event is journalled, projected, and silently dropped.
    /// `loop.outbox.gap` shipped in exactly that state.
    ///
    /// The *emitted-string → row* direction lives in
    /// `magician/tests/emitted_event_names_contract.rs`, which is a source scan
    /// because `emit_named` takes `&str` and there is nothing to exhaust. Its
    /// limits — forwarded names it cannot follow, and a shrink-only register of
    /// knowns it found already broken — are written up there.
    #[test]
    fn every_runtime_agent_taxonomy_row_has_a_typed_variant() {
        use crate::magician_v2::artifact_v2::events::ArtifactV2EventType;
        let canonical_keys: std::collections::HashSet<&'static str> = ArtifactV2EventType::ALL
            .iter()
            .map(|et| et.as_str())
            .collect();
        let typed_keys: std::collections::HashSet<&'static str> = RuntimeAgentEventType::ALL
            .iter()
            .map(|et| et.as_str())
            .collect();
        let untyped: Vec<&'static str> = GAUI_EVENT_TAXONOMY
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| !canonical_keys.contains(name) && !typed_keys.contains(name))
            .collect();
        assert!(
            untyped.is_empty(),
            "These `GAUI_EVENT_TAXONOMY` rows have no typed source \
             (neither `ArtifactV2EventType` nor `RuntimeAgentEventType`). Add \
             a variant for each in `RuntimeAgentEventType` so emit sites can \
             use `RuntimeAgentEventType::Variant.as_str()` instead of bare \
             string literals: {untyped:?}"
        );
    }

    /// Drift gate for `EVENT_TAXONOMY_TABLE`, in the direction no other
    /// guard covers.
    ///
    /// The two `taxonomies!` invocations are supposed to be identical,
    /// but only one of them is checked by anything. The invocation in
    /// this file expands to a `match` over `RuntimeTransportEvent`, so
    /// rustc verifies every name both ways. The mirror in
    /// `magician-event-taxonomy/src/lib.rs` expands to a
    /// `&[(&str, EventTaxonomy)]` built with `stringify!` — no enum is in
    /// scope there, so a row naming an event that does not exist compiles
    /// fine. `scripts/event_taxonomy_check.py` hashes the Rust sources to
    /// detect a forgotten codegen run and never inspects the table's
    /// contents, so it cannot see the orphan either. The orphan then
    /// reaches the generated TS mirror and `KNOWN_EVENT_TYPES`, where it
    /// becomes a filter chip for an event that will never arrive.
    ///
    /// `TaskArchived` sat in the table this way, phantom and unnoticed.
    #[test]
    fn every_taxonomy_table_row_has_a_transport_variant() {
        let real: std::collections::HashSet<&'static str> =
            TAGGED_TRANSPORT_EVENT_VARIANTS.iter().copied().collect();
        let orphans: Vec<&'static str> = EVENT_TAXONOMY_TABLE
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| !real.contains(name))
            .collect();
        assert!(
            orphans.is_empty(),
            "These `EVENT_TAXONOMY_TABLE` rows in `magician-event-taxonomy` \
             name no `RuntimeTransportEvent` variant, so they ship a phantom \
             event type to the TS mirror. Either add the variant + a matching \
             `taxonomies!` line in `realtime_events.rs`, or delete the row and \
             re-run `make event-taxonomy-codegen`: {orphans:?}"
        );
    }

    /// The same gate in the other direction: a variant tagged here but
    /// missing from the mirror never reaches the TS taxonomy, so the UI
    /// falls back to `observability / info / false` for it.
    #[test]
    fn every_tagged_transport_variant_has_a_taxonomy_table_row() {
        let table: std::collections::HashSet<&'static str> =
            EVENT_TAXONOMY_TABLE.iter().map(|(name, _)| *name).collect();
        let missing: Vec<&'static str> = TAGGED_TRANSPORT_EVENT_VARIANTS
            .iter()
            .copied()
            .filter(|name| !table.contains(name))
            .collect();
        assert!(
            missing.is_empty(),
            "These `RuntimeTransportEvent` variants are tagged in \
             `realtime_events.rs` but absent from `EVENT_TAXONOMY_TABLE` in \
             `magician-event-taxonomy`. Copy the `taxonomies!` lines across \
             and re-run `make event-taxonomy-codegen`: {missing:?}"
        );
    }

    /// The third direction: same names on both sides, different values.
    ///
    /// The two gates above only prove the two `taxonomies!` invocations
    /// list the same event names. Neither looks at the
    /// `(category, severity, user_relevant)` triple, so editing a row in
    /// one file and forgetting the other — promoting an event to `Warn`,
    /// or flipping `user_relevant` — passes both. The `SOURCE_HASH` gate
    /// can't catch it either; it only knows "Rust changed, codegen didn't
    /// re-run", not "the two tables disagree with each other".
    ///
    /// The consequence is quiet and one-sided: the runtime routes on the
    /// value in `realtime_events.rs` (via `taxonomy_lookup`), while every
    /// consumer downstream of the generated TS mirror — filter chips, the
    /// notification overlay's `user_relevant` gate, severity thresholds —
    /// routes on the value in `magician-event-taxonomy`. The same event
    /// would then be `Warn` on the server and `Info` in the browser.
    #[test]
    fn taxonomy_table_values_match_transport_tags() {
        let table: std::collections::HashMap<&'static str, EventTaxonomy> =
            EVENT_TAXONOMY_TABLE.iter().copied().collect();
        let divergent: Vec<String> = TAGGED_TRANSPORT_EVENT_TAXONOMIES
            .iter()
            .filter_map(|(name, tagged)| {
                let mirrored = table.get(name)?;
                (mirrored != tagged)
                    .then(|| format!("{name}: realtime_events={tagged:?} vs mirror={mirrored:?}"))
            })
            .collect();
        assert!(
            divergent.is_empty(),
            "These events are tagged in both `taxonomies!` invocations but \
             with different taxonomy values. The runtime uses the \
             `realtime_events.rs` value and every TS consumer uses the \
             `magician-event-taxonomy` one, so they must agree. Reconcile \
             the rows and re-run `make event-taxonomy-codegen`: {divergent:?}"
        );
    }

    #[tokio::test]
    async fn test_event_broadcasting() {
        let broadcaster = RuntimeTransportBroadcaster::new(100);
        let mut receiver = broadcaster.subscribe();

        broadcaster.message_started("thread1", "turn1", "corr1");

        let event = receiver.recv().await.unwrap();
        match event {
            RuntimeTransportEvent::MessageProcessingStarted { execution_id, .. } => {
                assert_eq!(execution_id, "thread1");
            },
            _ => panic!("Wrong event type"),
        }
    }

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let broadcaster = RuntimeTransportBroadcaster::new(100);
        let mut rx1 = broadcaster.subscribe();
        let mut rx2 = broadcaster.subscribe();

        assert_eq!(broadcaster.subscriber_count(), 2);

        broadcaster.heartbeat();

        let event1 = rx1.recv().await.unwrap();
        let event2 = rx2.recv().await.unwrap();

        assert!(matches!(event1, RuntimeTransportEvent::Heartbeat { .. }));
        assert!(matches!(event2, RuntimeTransportEvent::Heartbeat { .. }));
    }

    #[tokio::test]
    async fn workflow_stage_resumed_event_carries_metadata() {
        let broadcaster = RuntimeTransportBroadcaster::new(10);
        let mut receiver = broadcaster.subscribe();

        broadcaster.workflow_stage_resumed(
            "thread-resume",
            "execution.asset_reconcile",
            StageContext::ExecutionCycle,
            3,
            true,
            Some("abc123".into()),
            vec!["execution.tool_runner".into()],
        );

        match receiver.recv().await.unwrap() {
            RuntimeTransportEvent::WorkflowStageResumed {
                stage_name,
                stage_context,
                attempt,
                reused_checkpoint,
                reused_stages,
                checkpoint_hash,
                ..
            } => {
                assert_eq!(stage_name, "execution.asset_reconcile");
                assert_eq!(stage_context, StageContext::ExecutionCycle.as_str());
                assert_eq!(attempt, 3);
                assert!(reused_checkpoint);
                assert_eq!(reused_stages, vec!["execution.tool_runner".to_string()]);
                assert_eq!(checkpoint_hash.as_deref(), Some("abc123"));
            },
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[tokio::test]
    async fn emit_enriches_parameter_discovery_events_from_registered_execution_scope() {
        let broadcaster = RuntimeTransportBroadcaster::new(10);
        broadcaster.register_runtime_canonical_event_scope(CanonicalEventScope {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            task_id: "task-1".to_string(),
            execution_id: "exec-1".to_string(),
            ui_thread_id: "general".to_string(),
        });
        let mut receiver = broadcaster.subscribe();

        broadcaster.emit(RuntimeTransportEvent::ParameterDiscoveryAttempted {
            execution_id: "exec-1".to_string(),
            parameter_name: "target_url".to_string(),
            discovery_method: "AutonomousDiscovery".to_string(),
            principal: None,
            workspace: None,
            timestamp: 1,
        });

        match receiver.recv().await.unwrap() {
            RuntimeTransportEvent::ParameterDiscoveryAttempted {
                principal,
                workspace,
                ..
            } => {
                assert_eq!(principal.as_deref(), Some("alpha"));
                assert_eq!(workspace.as_deref(), Some("prod"));
            },
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[tokio::test]
    async fn emit_enriches_legacy_execution_events_from_registered_execution_scope() {
        let broadcaster = RuntimeTransportBroadcaster::new(10);
        broadcaster.register_runtime_canonical_event_scope(CanonicalEventScope {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            task_id: "task-1".to_string(),
            execution_id: "exec-1".to_string(),
            ui_thread_id: "general".to_string(),
        });
        let mut receiver = broadcaster.subscribe();

        broadcaster.emit(RuntimeTransportEvent::ExecutionStarted {
            execution_id: "exec-1".to_string(),
            principal: None,
            workspace: None,
            plan_id: "plan-1".to_string(),
            steps_total: 3,
            timestamp: 1,
        });

        match receiver.recv().await.unwrap() {
            RuntimeTransportEvent::ExecutionStarted {
                principal,
                workspace,
                ..
            } => {
                assert_eq!(principal.as_deref(), Some("alpha"));
                assert_eq!(workspace.as_deref(), Some("prod"));
            },
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[tokio::test]
    async fn emit_enriches_execution_status_events_from_registered_execution_scope() {
        let broadcaster = RuntimeTransportBroadcaster::new(10);
        broadcaster.register_runtime_canonical_event_scope(CanonicalEventScope {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            task_id: "task-1".to_string(),
            execution_id: "exec-1".to_string(),
            ui_thread_id: "general".to_string(),
        });
        let mut receiver = broadcaster.subscribe();

        broadcaster.emit(RuntimeTransportEvent::ExecutionStatusChanged {
            execution_id: "exec-1".to_string(),
            principal: None,
            workspace: None,
            task_id: Some("task-1".to_string()),
            root_execution_id: Some("exec-1".to_string()),
            previous_status: "planning".to_string(),
            new_status: "running".to_string(),
            reason: None,
            timestamp: 1,
        });

        match receiver.recv().await.unwrap() {
            RuntimeTransportEvent::ExecutionStatusChanged {
                principal,
                workspace,
                ..
            } => {
                assert_eq!(principal.as_deref(), Some("alpha"));
                assert_eq!(workspace.as_deref(), Some("prod"));
            },
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[tokio::test]
    async fn emit_enriches_observability_alerts_from_registered_execution_scope() {
        let broadcaster = RuntimeTransportBroadcaster::new(10);
        broadcaster.register_runtime_canonical_event_scope(CanonicalEventScope {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            task_id: "task-1".to_string(),
            execution_id: "exec-1".to_string(),
            ui_thread_id: "general".to_string(),
        });
        let mut receiver = broadcaster.subscribe();

        broadcaster.emit(RuntimeTransportEvent::ObservabilityAlert {
            execution_id: "exec-1".to_string(),
            principal: None,
            workspace: None,
            alert_type: "stalled".to_string(),
            details: serde_json::json!({ "reason": "waiting_too_long" }),
            timestamp: 1,
        });

        match receiver.recv().await.unwrap() {
            RuntimeTransportEvent::ObservabilityAlert {
                principal,
                workspace,
                ..
            } => {
                assert_eq!(principal.as_deref(), Some("alpha"));
                assert_eq!(workspace.as_deref(), Some("prod"));
            },
            other => panic!("Unexpected event: {:?}", other),
        }
    }

    #[tokio::test]
    async fn planexec_progress_emits_task_scoped_v3_planning_progress() {
        let broadcaster = RuntimeTransportBroadcaster::new(10);
        broadcaster.register_runtime_canonical_event_scope(CanonicalEventScope {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            task_id: "task-1".to_string(),
            execution_id: "planexec_1".to_string(),
            ui_thread_id: "general".to_string(),
        });
        broadcaster.register_v3_planning_transport_scope(
            "planexec_1".to_string(),
            "alpha".to_string(),
            "prod".to_string(),
            "task-1".to_string(),
            "Check if Jordan is up".to_string(),
            "personal-assistant".to_string(),
            "plan-1".to_string(),
            "general".to_string(),
        );
        let mut receiver = broadcaster.subscribe();

        broadcaster.emit(RuntimeTransportEvent::ExplorationProgress {
            execution_id: "planexec_1".to_string(),
            principal: None,
            workspace: None,
            correlation_id: "corr-1".to_string(),
            nodes_explored: 1,
            current_depth: 0,
            best_score: 0.7,
            current_task: "Generated atomic plan with 5 steps".to_string(),
            progress_percent: 80.0,
            timestamp: 42,
        });

        match receiver.recv().await.unwrap() {
            RuntimeTransportEvent::ExplorationProgress {
                principal,
                workspace,
                ..
            } => {
                assert_eq!(principal.as_deref(), Some("alpha"));
                assert_eq!(workspace.as_deref(), Some("prod"));
            },
            other => panic!("Unexpected first event: {:?}", other),
        }
        match receiver.recv().await.unwrap() {
            RuntimeTransportEvent::V3PlanningProgress {
                principal,
                workspace,
                task_id,
                task_title,
                plan_id,
                phase,
                detail,
                timestamp,
                ..
            } => {
                assert_eq!(principal, "alpha");
                assert_eq!(workspace, "prod");
                assert_eq!(task_id, "task-1");
                assert_eq!(task_title, "Check if Jordan is up");
                assert_eq!(plan_id, "plan-1");
                assert_eq!(phase, "exploration");
                assert_eq!(
                    detail.as_deref(),
                    Some("Generated atomic plan with 5 steps")
                );
                assert_eq!(timestamp, 42);
            },
            other => panic!("Unexpected second event: {:?}", other),
        }
    }

    // NOTE: observation_failed_event_carries_error_details test removed.
    // ObservationFailed event was removed - agentic execution handles observation errors implicitly.

    /// Fake canonical sink that records every durable write so tests can
    /// assert whether (and with what scope) `emit` persisted an event.
    #[derive(Default)]
    struct RecordingCanonicalSink {
        writes: std::sync::Mutex<
            Vec<(
                crate::magician_v2::artifact_v2::CanonicalEventScope,
                crate::magician_v2::artifact_v2::events::ArtifactV2EventType,
            )>,
        >,
    }

    impl crate::magician_v2::artifact_v2::RuntimeCanonicalEventSink for RecordingCanonicalSink {
        fn emit(
            &self,
            scope: crate::magician_v2::artifact_v2::CanonicalEventScope,
            event_type: crate::magician_v2::artifact_v2::events::ArtifactV2EventType,
            _payload: Value,
        ) {
            self.writes
                .lock()
                .expect("recording sink poisoned")
                .push((scope, event_type));
        }
    }

    struct BlockingCanonicalSink {
        writes: std::sync::Mutex<Vec<crate::magician_v2::artifact_v2::events::ArtifactV2EventType>>,
        request_entered: (std::sync::Mutex<bool>, std::sync::Condvar),
        release_request: (std::sync::Mutex<bool>, std::sync::Condvar),
    }

    impl Default for BlockingCanonicalSink {
        fn default() -> Self {
            Self {
                writes: std::sync::Mutex::new(Vec::new()),
                request_entered: (std::sync::Mutex::new(false), std::sync::Condvar::new()),
                release_request: (std::sync::Mutex::new(false), std::sync::Condvar::new()),
            }
        }
    }

    impl BlockingCanonicalSink {
        fn wait_until_request_entered(&self) {
            let (lock, signal) = &self.request_entered;
            let mut entered = lock.lock().expect("request-entered lock poisoned");
            while !*entered {
                entered = signal.wait(entered).expect("request-entered wait poisoned");
            }
        }

        fn release_request(&self) {
            let (lock, signal) = &self.release_request;
            *lock.lock().expect("request-release lock poisoned") = true;
            signal.notify_all();
        }
    }

    impl crate::magician_v2::artifact_v2::RuntimeCanonicalEventSink for BlockingCanonicalSink {
        fn emit(
            &self,
            _scope: crate::magician_v2::artifact_v2::CanonicalEventScope,
            event_type: crate::magician_v2::artifact_v2::events::ArtifactV2EventType,
            _payload: Value,
        ) {
            let blocks = event_type
                == crate::magician_v2::artifact_v2::events::ArtifactV2EventType::HitlRequested;
            self.writes
                .lock()
                .expect("blocking sink writes poisoned")
                .push(event_type);
            if !blocks {
                return;
            }
            let (entered_lock, entered_signal) = &self.request_entered;
            *entered_lock.lock().expect("request-entered lock poisoned") = true;
            entered_signal.notify_all();
            let (release_lock, release_signal) = &self.release_request;
            let mut released = release_lock.lock().expect("request-release lock poisoned");
            while !*released {
                released = release_signal
                    .wait(released)
                    .expect("request-release wait poisoned");
            }
        }
    }

    fn self_describing_hitl_resolved() -> RuntimeTransportEvent {
        RuntimeTransportEvent::HitlResolved {
            correlation_id: "ccp-abc".to_string(),
            source: "diff_approval".to_string(),
            outcome: "responded".to_string(),
            decision: Some("apply".to_string()),
            task_id: Some("task-1".to_string()),
            execution_id: Some("exec-1".to_string()),
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 1,
        }
    }

    fn requested_prerequisite_for(resolution: &RuntimeTransportEvent) -> RuntimeTransportEvent {
        let RuntimeTransportEvent::HitlResolved {
            correlation_id,
            source,
            task_id,
            execution_id,
            agent_id,
            principal,
            workspace,
            timestamp,
            ..
        } = resolution
        else {
            panic!("resolution prerequisite requires HitlResolved");
        };
        RuntimeTransportEvent::HitlRequested {
            correlation_id: correlation_id.clone(),
            source: source.clone(),
            input_type: "choice".to_string(),
            prompt: "Pending prerequisite".to_string(),
            hint: None,
            input_schema: None,
            task_id: task_id.clone(),
            execution_id: execution_id.clone(),
            agent_id: agent_id.clone(),
            principal: principal.clone(),
            workspace: workspace.clone(),
            timestamp: timestamp.saturating_sub(1),
        }
    }

    fn taskless_hitl_requested(correlation_id: &str) -> RuntimeTransportEvent {
        RuntimeTransportEvent::HitlRequested {
            correlation_id: correlation_id.to_string(),
            source: "bot_auth".to_string(),
            input_type: "choice".to_string(),
            prompt: "Connect account".to_string(),
            hint: None,
            input_schema: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 100,
        }
    }

    fn app_owner_notification_requested(
        correlation_id: &str,
        timestamp: i64,
        absolute_expires_at_ms: i64,
    ) -> RuntimeTransportEvent {
        RuntimeTransportEvent::HitlRequested {
            correlation_id: correlation_id.to_string(),
            source: "user_request".to_string(),
            input_type: "notification".to_string(),
            prompt: "private notification body".to_string(),
            hint: None,
            input_schema: Some(serde_json::json!({
                "context": {
                    "app_owner_notification": true,
                    "absolute_expires_at_ms": absolute_expires_at_ms,
                },
            })),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp,
        }
    }

    fn app_owner_generation(seed: u32) -> AppOwnerNotificationPublicationGeneration {
        AppOwnerNotificationPublicationGeneration::parse(&format!(
            "00000000-0000-4000-8000-{seed:012x}"
        ))
        .expect("canonical test app-owner generation")
    }

    fn generic_user_request_lifecycle(
        correlation_id: &str,
        request_timestamp: i64,
        resolution_timestamp: i64,
    ) -> (RuntimeTransportEvent, RuntimeTransportEvent) {
        let request = RuntimeTransportEvent::HitlRequested {
            correlation_id: correlation_id.to_string(),
            source: "user_request".to_string(),
            input_type: "choice".to_string(),
            prompt: "Choose".to_string(),
            hint: None,
            input_schema: Some(serde_json::json!({"options": []})),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: request_timestamp,
        };
        let resolution = RuntimeTransportEvent::HitlResolved {
            correlation_id: correlation_id.to_string(),
            source: "user_request".to_string(),
            outcome: "responded".to_string(),
            decision: Some("approve".to_string()),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: resolution_timestamp,
        };
        (request, resolution)
    }

    fn abandoned_hitl_lifecycle_work_item(
        journal_path: &std::path::Path,
        kind: HitlLifecycleWorkKind,
    ) -> std::path::PathBuf {
        let root = ensure_hitl_lifecycle_work_root(journal_path).expect("create work root");
        let path = root.join(format!("{}{}", kind.prefix(), Uuid::new_v4().simple()));
        let guard = AgentStorage::acquire_file_lock_exclusive_sync(&path)
            .expect("acquire simulated creator lease");
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(&path).expect("create simulated work item");
        drop(guard);
        path
    }

    #[test]
    fn lifecycle_orphan_cleanup_never_reclaims_a_live_leased_item() {
        let tmp = canonical_workspace_tempdir();
        let journal_path = tmp.path().join(HITL_LIFECYCLE_JOURNAL_FILENAME);
        let item = HitlLifecycleWorkItem::new(&journal_path, HitlLifecycleWorkKind::Compaction)
            .expect("create leased work item");
        let path = item.path.clone();
        cleanup_hitl_lifecycle_work_item(&path, HitlLifecycleWorkKind::Compaction);
        assert!(path.is_dir(), "a live creator lease must suppress cleanup");
        assert!(AgentStorage::file_lock_path(&path).is_file());
        drop(item);
        assert!(!path.exists());
        assert!(!AgentStorage::file_lock_path(&path).exists());
    }

    #[test]
    fn lifecycle_work_teardown_retains_retryable_lease_when_known_file_remains() {
        let tmp = canonical_workspace_tempdir();
        let journal_path = tmp.path().join(HITL_LIFECYCLE_JOURNAL_FILENAME);
        let item = HitlLifecycleWorkItem::new(&journal_path, HitlLifecycleWorkKind::Reducer)
            .expect("create leased work item");
        let path = item.path.clone();
        std::fs::write(path.join("reducer.sqlite3"), b"retry cleanup")
            .expect("leave known work file");

        // The generic work-item owner cannot remove a non-empty directory.
        // Its unlocked sentinel must remain so the non-creating sweeper probe
        // can claim and finish cleanup on a later slice.
        drop(item);
        assert!(path.is_dir());
        assert!(AgentStorage::file_lock_path(&path).is_file());
        cleanup_hitl_lifecycle_work_item(&path, HitlLifecycleWorkKind::Reducer);
        assert!(!path.exists());
        assert!(!AgentStorage::file_lock_path(&path).exists());
    }

    #[test]
    fn lifecycle_orphan_cleanup_reclaims_only_known_dead_item_contents() {
        let tmp = canonical_workspace_tempdir();
        let journal_path = tmp.path().join(HITL_LIFECYCLE_JOURNAL_FILENAME);

        let dead =
            abandoned_hitl_lifecycle_work_item(&journal_path, HitlLifecycleWorkKind::Reducer);
        std::fs::write(dead.join("reducer.sqlite3"), b"dead reducer")
            .expect("write known reducer file");
        cleanup_hitl_lifecycle_work_item(&dead, HitlLifecycleWorkKind::Reducer);
        assert!(!dead.exists());
        assert!(!AgentStorage::file_lock_path(&dead).exists());

        let unexpected =
            abandoned_hitl_lifecycle_work_item(&journal_path, HitlLifecycleWorkKind::Compaction);
        std::fs::write(unexpected.join("unexpected"), b"preserve").expect("write unexpected file");
        cleanup_hitl_lifecycle_work_item(&unexpected, HitlLifecycleWorkKind::Compaction);
        assert!(unexpected.join("unexpected").is_file());
        assert!(AgentStorage::file_lock_path(&unexpected).is_file());
    }

    #[tokio::test]
    async fn lifecycle_orphan_sweep_eventually_covers_more_than_one_page() {
        let tmp = canonical_workspace_tempdir();
        let journal_path = tmp.path().join(HITL_LIFECYCLE_JOURNAL_FILENAME);
        for _ in 0..HITL_LIFECYCLE_ORPHAN_SWEEP_SLICE.saturating_add(5) {
            let dead = abandoned_hitl_lifecycle_work_item(
                &journal_path,
                HitlLifecycleWorkKind::Compaction,
            );
            std::fs::write(dead.join("staging.tmp"), b"dead staging").expect("write dead staging");
        }

        let mut remaining = usize::MAX;
        for _ in 0..HITL_LIFECYCLE_ORPHAN_SWEEP_SLICE.saturating_add(6) {
            sweep_hitl_lifecycle_orphans_once(&journal_path).await;
            remaining = std::fs::read_dir(hitl_lifecycle_work_root(&journal_path))
                .expect("read swept work root")
                .count();
            if remaining == 0 {
                break;
            }
        }
        assert_eq!(remaining, 0, "bounded rescans must cover every orphan");
    }

    #[test]
    fn legacy_lifecycle_preappend_repairs_peer_crash_tail_before_import() {
        use std::io::Write as _;

        let tmp = canonical_workspace_tempdir();
        let path = tmp.path().join(HITL_LIFECYCLE_JOURNAL_FILENAME);
        let first = taskless_hitl_requested("before-peer-crash");
        let mut first_line = serde_json::to_vec(&first).expect("serialize first legacy row");
        first_line.push(b'\n');
        std::fs::write(&path, first_line).expect("write first legacy row");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("make legacy journal private");
        }

        let mut peer = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open peer journal");
        peer.write_all(b"{\"event_type\":\"HitlRequested\"")
            .expect("write simulated crash fragment");
        peer.sync_all().expect("sync simulated crash fragment");
        drop(peer);

        assert!(repair_unterminated_hitl_lifecycle_tail_before_append(&path)
            .expect("repair bounded legacy crash tail"));
        let second = taskless_hitl_requested("after-peer-crash");
        let mut second_line = serde_json::to_vec(&second).expect("serialize second legacy row");
        second_line.push(b'\n');
        let mut peer = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("reopen repaired legacy journal");
        peer.write_all(&second_line)
            .expect("append second legacy row");
        peer.sync_all().expect("sync second legacy row");
        let bytes = std::fs::read(&path).expect("read repaired journal");
        assert!(bytes.ends_with(b"\n"));
        let records = bytes
            .split(|byte| *byte == b'\n')
            .filter(|record| !record.is_empty())
            .map(|record| {
                serde_json::from_slice::<RuntimeTransportEvent>(record)
                    .expect("every committed row remains independently valid")
            })
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2);
        assert_eq!(
            scoped_hitl_lifecycle_key(&records[1]).map(|key| key.2),
            Some("after-peer-crash".to_string()),
        );
    }

    #[test]
    fn lifecycle_preappend_refuses_unterminated_tail_beyond_record_ceiling() {
        let tmp = canonical_workspace_tempdir();
        let path = tmp.path().join(HITL_LIFECYCLE_JOURNAL_FILENAME);
        let file = std::fs::File::create(&path).expect("create lifecycle journal");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .expect("make journal private");
        }
        let over_bound = (MAX_HITL_LIFECYCLE_RECORD_BYTES as u64).saturating_add(1);
        file.set_len(over_bound)
            .expect("create sparse over-bound crash tail");
        file.sync_all().expect("sync sparse crash tail");
        drop(file);

        assert!(repair_unterminated_hitl_lifecycle_tail_before_append(&path).is_err());
        assert_eq!(
            std::fs::metadata(&path).expect("journal metadata").len(),
            over_bound,
        );
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_startup_and_compaction_repair_and_preserve_private_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let path = workspace.base_root().join(HITL_LIFECYCLE_JOURNAL_FILENAME);
        let mut body = BoundedHitlLifecycleRecord::new();
        write_hitl_lifecycle_event(
            &mut body,
            &taskless_hitl_requested("legacy-readable-journal"),
        )
        .expect("serialize lifecycle row");
        body.bytes.push(b'\n');
        std::fs::write(&path, &body.bytes).expect("write legacy journal");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("make legacy journal readable");

        let broadcaster =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace.clone());
        assert!(path.is_dir(), "legacy writer pathname becomes a fence");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("downgrade fence metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
        );
        let authority_path = workspace
            .base_root()
            .join(HITL_LIFECYCLE_AUTHORITY_FILENAME);
        assert_eq!(
            std::fs::metadata(&authority_path)
                .expect("authority metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
        );
        assert!(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .is_err(),
            "an older JSONL writer is durably fenced"
        );
        broadcaster
            .compact_hitl_lifecycle_journal()
            .expect("compact private lifecycle authority");
        assert_eq!(
            std::fs::metadata(&authority_path)
                .expect("compacted authority metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
        );
    }

    #[test]
    fn lifecycle_pre_ready_fence_reimports_private_crash_archive() {
        use std::io::Write as _;

        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let journal_path = workspace.base_root().join(HITL_LIFECYCLE_JOURNAL_FILENAME);
        let archive_path = workspace
            .base_root()
            .join(HITL_LIFECYCLE_IMPORT_ARCHIVE_FILENAME);
        let authority_path = workspace
            .base_root()
            .join(HITL_LIFECYCLE_AUTHORITY_FILENAME);
        create_empty_hitl_lifecycle_import_archive(&archive_path)
            .expect("create private crash archive");
        let request = taskless_hitl_requested("restartable-import");
        let mut line = serde_json::to_vec(&request).expect("serialize archived request");
        line.push(b'\n');
        let mut archive = std::fs::OpenOptions::new()
            .append(true)
            .open(&archive_path)
            .expect("open crash archive");
        archive.write_all(&line).expect("append archived request");
        archive.sync_all().expect("sync archived request");
        create_hitl_lifecycle_legacy_fence(&journal_path)
            .expect("install pre-ready downgrade fence");
        drop(
            HitlLifecycleDiskReducer::open_authority(&authority_path)
                .expect("create unready authority"),
        );

        let recovered =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace);
        assert!(matches!(
            recovered.hitl_lifecycle_state("alpha", "prod", "restartable-import"),
            HitlLifecycleState::Pending(_)
        ));
        assert!(journal_path.is_dir());
        assert!(!archive_path.exists());
        assert!(HitlLifecycleDiskReducer::open_authority(&authority_path)
            .expect("open imported authority")
            .authority_is_ready()
            .expect("read imported readiness"));
    }

    #[test]
    fn heap_framed_hitl_writer_matches_derived_serde_wire_bytes() {
        let requested = RuntimeTransportEvent::HitlRequested {
            correlation_id: "request-\"escaped".to_string(),
            source: "agentic".to_string(),
            input_type: "choice".to_string(),
            prompt: "Choose\ncarefully".to_string(),
            hint: Some("hint".to_string()),
            input_schema: Some(serde_json::json!({
                "type": "choice",
                "options": [{"id": "a", "label": "Alpha"}],
            })),
            task_id: Some("task-1".to_string()),
            execution_id: Some("exec-1".to_string()),
            agent_id: Some("agent-1".to_string()),
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 123,
        };
        let resolved = self_describing_hitl_resolved();
        let minimal_requested = taskless_hitl_requested("minimal-request");
        let minimal_resolved = RuntimeTransportEvent::HitlResolved {
            correlation_id: "minimal-resolution".to_string(),
            source: "user_request".to_string(),
            outcome: "dismissed".to_string(),
            decision: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: None,
            workspace: None,
            timestamp: 124,
        };

        for event in [&requested, &resolved, &minimal_requested, &minimal_resolved] {
            let expected = serde_json::to_vec(event).expect("legacy derived wire");
            let mut actual = Vec::new();
            write_hitl_lifecycle_event(&mut actual, event).expect("heap-framed HITL wire");
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn resolved_fingerprint_matches_exact_event_and_survives_compaction() {
        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let broadcaster =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace.clone());
        let expected = RuntimeTransportEvent::HitlResolved {
            correlation_id: "user-request-1".to_string(),
            source: "user_request".to_string(),
            outcome: "responded".to_string(),
            decision: Some("approve".to_string()),
            task_id: Some("task-1".to_string()),
            execution_id: Some("execution-1".to_string()),
            agent_id: Some("agent-1".to_string()),
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 123,
        };
        assert_eq!(
            broadcaster
                .reconcile_generic_hitl_request_batch(vec![requested_prerequisite_for(&expected),]),
            vec![true],
        );
        assert!(broadcaster.emit_hitl_lifecycle_if_accepted(expected.clone()));
        assert!(broadcaster.resolved_hitl_lifecycle_matches(
            "alpha",
            "prod",
            "user-request-1",
            &expected,
        ));
        assert!(broadcaster.local_generic_hitl_resolution_matches(&expected));

        let mut conflicting = expected.clone();
        if let RuntimeTransportEvent::HitlResolved { decision, .. } = &mut conflicting {
            *decision = Some("deny".to_string());
        }
        assert!(!broadcaster.resolved_hitl_lifecycle_matches(
            "alpha",
            "prod",
            "user-request-1",
            &conflicting,
        ));
        assert!(!broadcaster.local_generic_hitl_resolution_matches(&conflicting));
        assert!(!broadcaster.resolved_hitl_lifecycle_matches(
            "alpha",
            "other",
            "user-request-1",
            &expected,
        ));

        broadcaster
            .compact_hitl_lifecycle_journal()
            .expect("compact exact resolved proof");
        drop(broadcaster);
        let restarted =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace);
        assert!(restarted.resolved_hitl_lifecycle_matches(
            "alpha",
            "prod",
            "user-request-1",
            &expected,
        ));
        assert!(restarted.local_generic_hitl_resolution_matches(&expected));
        assert!(!restarted.resolved_hitl_lifecycle_matches(
            "alpha",
            "prod",
            "user-request-1",
            &conflicting,
        ));
        // The in-memory recovery-health flag does NOT suppress a durable
        // answer, and that is the design rather than an oversight. The
        // lifecycle journal is committed before registry, canonical and
        // broadcast visibility, so when an authority exists it IS the truth;
        // the flag guards the in-memory fallback taken when there is no
        // authority to read, and it still gates authority creation.
        //
        // This assertion previously read the other way. It could, because the
        // authority never opened: its temp path ran through a symlink and
        // `SQLITE_OPEN_NOFOLLOW` refused it, so every restart silently took the
        // fallback. With the authority actually opening, the boundary is
        // visible and worth pinning explicitly.
        restarted
            .hitl_lifecycle_recovery_healthy
            .store(false, Ordering::SeqCst);
        assert!(
            restarted.local_generic_hitl_resolution_matches(&expected),
            "a durable authority answer is not suppressed by in-memory recovery health"
        );
    }

    #[test]
    fn generic_resolution_batch_recovers_request_once_and_refuses_conflicts() {
        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let owner =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace.clone());
        // Construct the peer before publication so its in-memory projection is
        // intentionally stale; shared-disk exact authority must still win.
        let stale_peer =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace.clone());
        let (request, resolution) = generic_user_request_lifecycle("batch-generic", 100, 101);
        let mut owner_receiver = owner.subscribe();

        assert_eq!(
            owner.reconcile_generic_hitl_resolution_batch(vec![(
                request.clone(),
                resolution.clone(),
            )]),
            vec![true],
        );
        assert!(matches!(
            owner_receiver.try_recv(),
            Ok(RuntimeTransportEvent::HitlRequested { .. })
        ));
        assert!(matches!(
            owner_receiver.try_recv(),
            Ok(RuntimeTransportEvent::HitlResolved { .. })
        ));
        let authority_path = workspace
            .base_root()
            .join(HITL_LIFECYCLE_AUTHORITY_FILENAME);
        let committed_rows = HitlLifecycleDiskReducer::open_authority(&authority_path)
            .expect("open keyed authority")
            .connection()
            .expect("authority connection")
            .query_row("SELECT COUNT(*) FROM lifecycle", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count keyed authority rows");

        let mut peer_receiver = stale_peer.subscribe();
        assert_eq!(
            stale_peer.reconcile_generic_hitl_resolution_batch(vec![(
                request.clone(),
                resolution.clone(),
            )]),
            vec![true],
        );
        assert!(matches!(
            peer_receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert_eq!(
            HitlLifecycleDiskReducer::open_authority(&authority_path)
                .expect("reopen keyed authority")
                .connection()
                .expect("authority connection")
                .query_row("SELECT COUNT(*) FROM lifecycle", [], |row| row
                    .get::<_, i64>(0))
                .expect("recount keyed authority rows"),
            committed_rows,
            "an exact peer retry must not create duplicate keyed authority",
        );

        let mut conflicting_resolution = resolution;
        if let RuntimeTransportEvent::HitlResolved { timestamp, .. } = &mut conflicting_resolution {
            *timestamp = timestamp.saturating_add(1);
        }
        assert_eq!(
            stale_peer
                .reconcile_generic_hitl_resolution_batch(vec![(request, conflicting_resolution,)]),
            vec![false],
        );
        assert_eq!(
            HitlLifecycleDiskReducer::open_authority(&authority_path)
                .expect("reopen authority after conflict")
                .connection()
                .expect("authority connection")
                .query_row("SELECT COUNT(*) FROM lifecycle", [], |row| row
                    .get::<_, i64>(0))
                .expect("recount authority after conflict"),
            committed_rows,
        );
    }

    #[test]
    fn generic_resolution_batch_preserves_different_pending_and_app_authority() {
        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let broadcaster =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace);
        let (first_request, first_resolution) =
            generic_user_request_lifecycle("pending-conflict", 100, 101);
        assert_eq!(
            broadcaster.reconcile_generic_hitl_request_batch(vec![first_request.clone()]),
            vec![true],
        );
        let mut different_request = first_request.clone();
        if let RuntimeTransportEvent::HitlRequested { prompt, .. } = &mut different_request {
            *prompt = "Different".to_string();
        }
        assert_eq!(
            broadcaster.reconcile_generic_hitl_resolution_batch(vec![(
                different_request,
                first_resolution,
            )]),
            vec![false],
        );
        let pending = broadcaster
            .pending_hitl_request("alpha", "prod", "pending-conflict")
            .expect("first pending authority remains");
        assert_eq!(
            bounded_hitl_lifecycle_event_bytes(&pending),
            bounded_hitl_lifecycle_event_bytes(&first_request),
        );

        let now_ms = chrono::Utc::now().timestamp_millis();
        let app_request = app_owner_notification_requested(
            "app-collision",
            now_ms,
            now_ms.saturating_add(60_000),
        );
        let app_ticket = broadcaster.reconcile_app_owner_notification_request_batch(vec![(
            app_request,
            app_owner_generation(10),
        )]);
        assert!(app_ticket.accepted(0));
        drop(app_ticket);
        let (generic_request, generic_resolution) =
            generic_user_request_lifecycle("app-collision", now_ms, now_ms.saturating_add(1));
        assert_eq!(
            broadcaster.reconcile_generic_hitl_resolution_batch(vec![(
                generic_request,
                generic_resolution,
            )]),
            vec![false],
        );
        assert!(broadcaster.app_owner_notification_lifecycle.contains_key(&(
            "alpha".to_string(),
            "prod".to_string(),
            "app-collision".to_string(),
        )));
    }

    #[test]
    fn legacy_resolved_proof_is_closed_but_not_exact_authority() {
        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let key = (
            "alpha".to_string(),
            "prod".to_string(),
            "legacy-user-request".to_string(),
        );
        let proof = hitl_lifecycle_proof(&key, HitlLifecycleProofStateV1::Resolved, 123, None);
        let mut encoded = serde_json::to_vec(&proof).expect("serialize legacy proof");
        encoded.push(b'\n');
        std::fs::write(
            workspace.base_root().join(HITL_LIFECYCLE_JOURNAL_FILENAME),
            encoded,
        )
        .expect("write legacy proof");

        let broadcaster =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace);
        let expected = RuntimeTransportEvent::HitlResolved {
            correlation_id: key.2.clone(),
            source: "user_request".to_string(),
            outcome: "responded".to_string(),
            decision: Some("approve".to_string()),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some(key.0.clone()),
            workspace: Some(key.1.clone()),
            timestamp: 123,
        };
        assert!(matches!(
            broadcaster.hitl_lifecycle_state(&key.0, &key.1, &key.2),
            HitlLifecycleState::Resolved
        ));
        assert!(!broadcaster.resolved_hitl_lifecycle_matches(&key.0, &key.1, &key.2, &expected,));
    }

    #[test]
    fn app_pending_and_resolution_are_atomic_exact_and_content_free() {
        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let now_ms = chrono::Utc::now().timestamp_millis();
        let expiry_ms = now_ms.saturating_add(60_000);
        let expected =
            app_owner_notification_requested("atomic-app-notification", now_ms, expiry_ms);
        let resolution = RuntimeTransportEvent::HitlResolved {
            correlation_id: "atomic-app-notification".to_string(),
            source: "app_owner_notification".to_string(),
            outcome: "responded".to_string(),
            decision: Some("private response".to_string()),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: now_ms.saturating_add(1),
        };
        let broadcaster =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace.clone());
        let mut receiver = broadcaster.subscribe();
        assert!(
            broadcaster.ensure_and_resolve_app_owner_notification_lifecycle(
                &expected,
                resolution.clone(),
                app_owner_generation(1),
            )
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        let authority_bytes = std::fs::read(
            workspace
                .base_root()
                .join(HITL_LIFECYCLE_AUTHORITY_FILENAME),
        )
        .expect("read atomic app lifecycle authority");
        assert!(!authority_bytes
            .windows(b"private notification body".len())
            .any(|window| window == b"private notification body"));
        assert!(!authority_bytes
            .windows(b"private response".len())
            .any(|window| window == b"private response"));
        drop(broadcaster);

        let restarted =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace);
        assert!(
            !restarted.ensure_and_resolve_app_owner_notification_lifecycle(
                &expected,
                resolution.clone(),
                app_owner_generation(99),
            )
        );
        assert!(
            restarted.ensure_and_resolve_app_owner_notification_lifecycle(
                &expected,
                resolution.clone(),
                app_owner_generation(1),
            )
        );
        let mut conflicting = resolution;
        if let RuntimeTransportEvent::HitlResolved { decision, .. } = &mut conflicting {
            *decision = Some("different response".to_string());
        }
        assert!(
            !restarted.ensure_and_resolve_app_owner_notification_lifecycle(
                &expected,
                conflicting,
                app_owner_generation(1),
            )
        );
    }

    #[test]
    fn app_request_batch_separates_redacted_proof_from_exact_live_publication() {
        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let broadcaster =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace.clone());
        let mut receiver = broadcaster.subscribe();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let expiry_ms = now_ms.saturating_add(60_000);
        let request = app_owner_notification_requested("guarded-app-request", now_ms, expiry_ms);
        assert!(
            !broadcaster.emit_hitl_lifecycle_if_accepted(request.clone()),
            "V3 app authority requires the durable UserRequest generation"
        );
        assert!(!broadcaster.append_hitl_lifecycle_journal_in_repaired_batch(&request));

        let generation = app_owner_generation(2);
        let mut publication_ticket = broadcaster
            .reconcile_app_owner_notification_request_batch(vec![(request.clone(), generation)]);
        assert!(publication_ticket.accepted(0));
        let tokenless_resolution = RuntimeTransportEvent::HitlResolved {
            correlation_id: "guarded-app-request".to_string(),
            source: "app_owner_notification".to_string(),
            outcome: "responded".to_string(),
            decision: Some("private response".to_string()),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: now_ms.saturating_add(1),
        };
        assert!(
            !broadcaster.emit_hitl_lifecycle_if_accepted(tokenless_resolution.clone()),
            "V3 app resolution cannot bypass generation-bound reconciliation"
        );
        assert!(!broadcaster.append_hitl_lifecycle_journal_in_repaired_batch(&tokenless_resolution));
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        let authority_bytes = std::fs::read(
            workspace
                .base_root()
                .join(HITL_LIFECYCLE_AUTHORITY_FILENAME),
        )
        .expect("read redacted app request proof");
        assert!(!authority_bytes
            .windows(b"private notification body".len())
            .any(|window| window == b"private notification body"));

        let foreign_broadcaster = RuntimeTransportBroadcaster::new(8);
        assert!(
            !foreign_broadcaster.broadcast_reconciled_app_owner_notification_request(
                &mut publication_ticket,
                0,
                request.clone(),
            )
        );
        let mut conflicting = request.clone();
        if let RuntimeTransportEvent::HitlRequested { prompt, .. } = &mut conflicting {
            *prompt = "different private notification body".to_string();
        }
        assert!(
            !broadcaster.broadcast_reconciled_app_owner_notification_request(
                &mut publication_ticket,
                0,
                conflicting,
            )
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert!(publication_ticket._publication_guard.is_some());
        assert!(publication_ticket.accepted(0));
        assert_eq!(
            publication_ticket.authorized_requests[0],
            bounded_hitl_lifecycle_event_bytes(&request)
                .as_deref()
                .map(app_owner_notification_request_authorization),
        );
        let retained = broadcaster
            .app_owner_notification_lifecycle
            .get(&(
                "alpha".to_string(),
                "prod".to_string(),
                "guarded-app-request".to_string(),
            ))
            .expect("reconciled app request lifecycle");
        assert_eq!(retained.state, HitlLifecycleProofStateV1::PendingRedacted);
        assert_eq!(retained.timestamp, now_ms);
        assert_eq!(retained.absolute_expires_at_ms, expiry_ms);
        assert_eq!(retained.app_owner_generation, Some(generation));
        drop(retained);
        assert!(
            broadcaster.broadcast_reconciled_app_owner_notification_request(
                &mut publication_ticket,
                0,
                request.clone(),
            )
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert!(broadcaster
            .pending_hitl_request("alpha", "prod", "guarded-app-request")
            .is_none());
        assert_eq!(
            broadcaster
                .app_owner_notification_lifecycle
                .get(&(
                    "alpha".to_string(),
                    "prod".to_string(),
                    "guarded-app-request".to_string(),
                ))
                .map(|state| state.app_owner_generation),
            Some(Some(generation)),
        );
        assert!(
            !broadcaster.broadcast_reconciled_app_owner_notification_request(
                &mut publication_ticket,
                0,
                request.clone(),
            )
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        drop(publication_ticket);
        let wrong_generation_ticket = broadcaster.reconcile_app_owner_notification_request_batch(
            vec![(request.clone(), app_owner_generation(99))],
        );
        assert!(!wrong_generation_ticket.accepted(0));
        drop(wrong_generation_ticket);

        let resolved_first_request = app_owner_notification_requested(
            "resolved-before-request-publication",
            now_ms.saturating_add(1),
            expiry_ms,
        );
        let resolved_first = RuntimeTransportEvent::HitlResolved {
            correlation_id: "resolved-before-request-publication".to_string(),
            source: "app_owner_notification".to_string(),
            outcome: "responded".to_string(),
            decision: Some("private response".to_string()),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: now_ms.saturating_add(2),
        };
        assert_eq!(
            broadcaster.reconcile_app_owner_notification_lifecycle_batch(vec![(
                resolved_first_request.clone(),
                resolved_first,
                app_owner_generation(3),
            )]),
            vec![true],
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        let mut refused_ticket =
            broadcaster.reconcile_app_owner_notification_request_batch(vec![(
                resolved_first_request.clone(),
                app_owner_generation(3),
            )]);
        assert!(!refused_ticket.accepted(0));
        assert!(
            !broadcaster.broadcast_reconciled_app_owner_notification_request(
                &mut refused_ticket,
                0,
                resolved_first_request,
            )
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn admitted_hitl_schema_serializes_on_a_small_stack_without_recursive_serde() {
        std::thread::Builder::new()
            .name("hitl-schema-wire-small-stack".to_string())
            .stack_size(128 * 1024)
            .spawn(|| {
                let mut schema = Value::Null;
                for _ in 0..MAX_RETAINED_JSON_DEPTH.saturating_sub(2) {
                    schema = Value::Array(vec![schema]);
                }
                let event = RuntimeTransportEvent::HitlRequested {
                    correlation_id: "deep-admitted-hitl".to_string(),
                    source: "agentic".to_string(),
                    input_type: "text".to_string(),
                    prompt: "Question".to_string(),
                    hint: None,
                    input_schema: Some(schema),
                    task_id: None,
                    execution_id: None,
                    agent_id: None,
                    principal: Some("alpha".to_string()),
                    workspace: Some("prod".to_string()),
                    timestamp: 100,
                };
                assert!(hitl_lifecycle_event_is_admitted(&event));
                let mut output = BoundedHitlLifecycleRecord::new();
                write_hitl_lifecycle_event(&mut output, &event)
                    .expect("admitted schema should serialize");
                assert!(!output.exceeded);
                assert!(json_bytes_nesting_is_bounded(
                    &output.bytes,
                    MAX_RETAINED_JSON_DEPTH,
                ));
                let retained =
                    clone_hitl_lifecycle_event_iteratively(&event).expect("HITL lifecycle clone");
                discard_hitl_lifecycle_event_iteratively(retained);
                discard_hitl_lifecycle_event_iteratively(event);
            })
            .expect("spawn small-stack HITL writer regression")
            .join()
            .expect("heap-framed HITL writer must not overflow");
    }

    /// FIX #1 — a `HitlResolved` that self-describes all four scope
    /// identifiers must persist to the canonical sink even when NO
    /// in-memory execution scope is registered (the post-restart /
    /// finished-one-shot-execution case). Without this the resolution
    /// never reaches the origin execution's `events.jsonl` and the
    /// attention projection rebuilds the resolved card forever.
    #[test]
    fn emit_persists_self_describing_hitl_resolved_without_registered_scope() {
        let broadcaster = RuntimeTransportBroadcaster::new(10);
        let sink = Arc::new(RecordingCanonicalSink::default());
        broadcaster.set_runtime_canonical_event_sink(sink.clone());
        // Deliberately DO NOT register any canonical event scope.

        let resolved = self_describing_hitl_resolved();
        broadcaster.record_pending_hitl_lifecycle(&requested_prerequisite_for(&resolved));
        broadcaster.emit(resolved);

        let writes = sink.writes.lock().expect("recording sink poisoned");
        assert_eq!(writes.len(), 1, "expected exactly one durable write");
        let (scope, event_type) = &writes[0];
        assert_eq!(
            *event_type,
            crate::magician_v2::artifact_v2::events::ArtifactV2EventType::HitlResolved
        );
        assert_eq!(scope.principal, "alpha");
        assert_eq!(scope.workspace, "prod");
        assert_eq!(scope.task_id, "task-1");
        assert_eq!(scope.execution_id, "exec-1");
        // ui_thread_id is not carried on the event; defaulted to empty.
        assert_eq!(scope.ui_thread_id, "");
    }

    /// FIX #1 — a `HitlResolved` that is MISSING a scope identifier does
    /// not self-describe, so on a registry miss it must NOT synthesize a
    /// bogus target log. The write is skipped (no unambiguous target).
    #[test]
    fn emit_skips_non_self_describing_hitl_resolved_without_registered_scope() {
        let broadcaster = RuntimeTransportBroadcaster::new(10);
        let sink = Arc::new(RecordingCanonicalSink::default());
        broadcaster.set_runtime_canonical_event_sink(sink.clone());

        // Missing principal/workspace → cannot self-describe a scope.
        broadcaster.emit(RuntimeTransportEvent::HitlResolved {
            correlation_id: "ccp-abc".to_string(),
            source: "diff_approval".to_string(),
            outcome: "responded".to_string(),
            decision: Some("apply".to_string()),
            task_id: Some("task-1".to_string()),
            execution_id: Some("exec-1".to_string()),
            agent_id: None,
            principal: None,
            workspace: None,
            timestamp: 1,
        });

        let writes = sink.writes.lock().expect("recording sink poisoned");
        assert!(
            writes.is_empty(),
            "non-self-describing event must not persist a durable write"
        );
    }

    #[test]
    fn recorded_runtime_fact_uses_exact_scope_without_a_live_registration() {
        let broadcaster = RuntimeTransportBroadcaster::new(10);
        let sink = Arc::new(RecordingCanonicalSink::default());
        broadcaster.set_runtime_canonical_event_sink(sink.clone());
        // Deliberately no `register_runtime_canonical_event_scope`: this is the
        // cold terminal-projector shape.
        let scope = CanonicalEventScope {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            task_id: "task-1".to_string(),
            execution_id: "exec-1".to_string(),
            ui_thread_id: "general".to_string(),
        };
        broadcaster
            .emit_recorded_runtime_fact(
                RuntimeTransportEvent::ExecutionStatusChanged {
                    execution_id: "exec-1".to_string(),
                    principal: None,
                    workspace: None,
                    task_id: Some("task-1".to_string()),
                    root_execution_id: Some("exec-1".to_string()),
                    previous_status: "running".to_string(),
                    new_status: "completed".to_string(),
                    reason: None,
                    timestamp: 1,
                },
                scope.clone(),
            )
            .expect("the recorded route is exact");

        let writes = sink.writes.lock().expect("recording sink poisoned");
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, scope);
    }

    #[test]
    fn recorded_runtime_fact_refuses_an_execution_mismatch_before_broadcast() {
        let broadcaster = RuntimeTransportBroadcaster::new(10);
        let sink = Arc::new(RecordingCanonicalSink::default());
        broadcaster.set_runtime_canonical_event_sink(sink.clone());
        let mut receiver = broadcaster.subscribe();
        let error = broadcaster
            .emit_recorded_runtime_fact(
                RuntimeTransportEvent::ExecutionStatusChanged {
                    execution_id: "exec-from-event".to_string(),
                    principal: None,
                    workspace: None,
                    task_id: Some("task-1".to_string()),
                    root_execution_id: Some("exec-from-event".to_string()),
                    previous_status: "running".to_string(),
                    new_status: "completed".to_string(),
                    reason: None,
                    timestamp: 1,
                },
                CanonicalEventScope {
                    principal: "alpha".to_string(),
                    workspace: "prod".to_string(),
                    task_id: "task-1".to_string(),
                    execution_id: "exec-from-envelope".to_string(),
                    ui_thread_id: "general".to_string(),
                },
            )
            .expect_err("a mismatched recorded route must fail closed");

        assert!(matches!(
            error,
            RecordedRuntimeFactRefused::ExecutionMismatch { .. }
        ));
        assert!(
            sink.writes
                .lock()
                .expect("recording sink poisoned")
                .is_empty(),
            "the canonical sink must not receive a mismatched route"
        );
        assert!(
            receiver.try_recv().is_err(),
            "a mismatch must not leak onto the transport rail"
        );
    }

    /// FIX #1 — the registry HIT path is unchanged: a registered scope
    /// still drives the durable write (byte-identical to pre-fix
    /// behavior), and the registered scope — including its real
    /// `ui_thread_id` — is used rather than the self-describing fallback.
    #[test]
    fn emit_prefers_registered_scope_over_self_describing_fallback() {
        let broadcaster = RuntimeTransportBroadcaster::new(10);
        let sink = Arc::new(RecordingCanonicalSink::default());
        broadcaster.set_runtime_canonical_event_sink(sink.clone());
        broadcaster.register_runtime_canonical_event_scope(CanonicalEventScope {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            task_id: "task-1".to_string(),
            execution_id: "exec-1".to_string(),
            ui_thread_id: "general".to_string(),
        });

        let resolved = self_describing_hitl_resolved();
        broadcaster.record_pending_hitl_lifecycle(&requested_prerequisite_for(&resolved));
        broadcaster.emit(resolved);

        let writes = sink.writes.lock().expect("recording sink poisoned");
        assert_eq!(writes.len(), 1);
        // ui_thread_id comes from the REGISTERED scope, proving the
        // registry hit path won (the event itself carries no thread id).
        assert_eq!(writes[0].0.ui_thread_id, "general");
    }

    #[test]
    fn canonical_hitl_registry_resolves_taskless_requests_by_scope_until_resolution() {
        let broadcaster = RuntimeTransportBroadcaster::new(10);
        let requested = RuntimeTransportEvent::HitlRequested {
            correlation_id: "bot_auth:alpha:prod:gmail".to_string(),
            source: "bot_auth".to_string(),
            input_type: "choice".to_string(),
            prompt: "Connect Gmail".to_string(),
            hint: Some("Sign in to continue".to_string()),
            input_schema: Some(serde_json::json!({
                "type": "choice",
                "options": [{"id": "open_auth_flow", "label": "Connect"}]
            })),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 100,
        };
        broadcaster.emit(requested.clone());

        let pending = broadcaster
            .pending_hitl_request("alpha", "prod", "bot_auth:alpha:prod:gmail")
            .expect("request should remain pending in its exact scope");
        assert_eq!(
            serde_json::to_value(pending).expect("pending event should serialize"),
            serde_json::to_value(requested).expect("requested event should serialize")
        );
        assert!(broadcaster
            .pending_hitl_request("alpha", "other", "bot_auth:alpha:prod:gmail")
            .is_none());

        broadcaster.emit(RuntimeTransportEvent::HitlResolved {
            correlation_id: "bot_auth:alpha:prod:gmail".to_string(),
            source: "bot_auth".to_string(),
            outcome: "responded".to_string(),
            decision: Some("authenticated".to_string()),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 101,
        });

        assert!(broadcaster
            .pending_hitl_request("alpha", "prod", "bot_auth:alpha:prod:gmail")
            .is_none());
    }

    #[test]
    fn pending_hitl_journal_survives_restart_without_transport_log_replay() {
        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let resolved_id = "bot_auth:alpha:prod:gmail";
        let pending_id = "bot_auth:alpha:prod:calendar";
        let first =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace.clone());
        first.emit(taskless_hitl_requested(resolved_id));
        first.emit(taskless_hitl_requested(pending_id));
        first.emit(RuntimeTransportEvent::HitlResolved {
            correlation_id: resolved_id.to_string(),
            source: "bot_auth".to_string(),
            outcome: "responded".to_string(),
            decision: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 101,
        });
        drop(first);

        let restarted =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace.clone());
        assert!(restarted
            .pending_hitl_request("alpha", "prod", resolved_id)
            .is_none());
        assert!(restarted
            .pending_hitl_request("alpha", "prod", pending_id)
            .is_some());
        assert!(restarted
            .pending_hitl_request("alpha", "other", pending_id)
            .is_none());

        restarted.emit(RuntimeTransportEvent::HitlResolved {
            correlation_id: pending_id.to_string(),
            source: "bot_auth".to_string(),
            outcome: "dismissed".to_string(),
            decision: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 102,
        });
        drop(restarted);
        let after_resolution =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace);
        assert!(after_resolution
            .pending_hitl_request("alpha", "prod", pending_id)
            .is_none());
        assert!(matches!(
            after_resolution.hitl_lifecycle_state("alpha", "prod", pending_id),
            HitlLifecycleState::Resolved
        ));
    }

    /// Every pause of one run shares its pause key, so a run's second
    /// question arrives under a correlation id that was resolved for its
    /// first. A request newer than the resolution is that next question and
    /// is admitted as a fresh pending request; one not newer is the resolved
    /// request replayed and stays refused. Asserted on both the in-memory
    /// registry and the persisted journal (across a restart).
    #[test]
    fn a_resolved_key_asked_again_later_is_pending_again_and_a_replay_is_not() {
        let id = "agent:android-operator:goal:cycle";
        let requested_at = |timestamp: i64| {
            let mut event = taskless_hitl_requested(id);
            if let RuntimeTransportEvent::HitlRequested {
                timestamp: at,
                prompt,
                ..
            } = &mut event
            {
                *at = timestamp;
                *prompt = format!("question at {timestamp}");
            }
            event
        };
        let resolved_at = |timestamp: i64| RuntimeTransportEvent::HitlResolved {
            correlation_id: id.to_string(),
            source: "agentic".to_string(),
            outcome: "responded".to_string(),
            decision: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp,
        };

        let in_memory = RuntimeTransportBroadcaster::new(8);
        in_memory.emit(requested_at(100));
        in_memory.emit(resolved_at(110));
        assert!(in_memory
            .pending_hitl_request("alpha", "prod", id)
            .is_none());
        // The resolved request replayed: refused.
        in_memory.emit(requested_at(100));
        assert!(in_memory
            .pending_hitl_request("alpha", "prod", id)
            .is_none());
        // The run's next question: pending again.
        in_memory.emit(requested_at(120));
        let pending = in_memory
            .pending_hitl_request("alpha", "prod", id)
            .expect("the next question is pending");
        assert!(matches!(
            pending,
            RuntimeTransportEvent::HitlRequested { timestamp: 120, .. }
        ));
        in_memory.emit(resolved_at(130));
        assert!(in_memory
            .pending_hitl_request("alpha", "prod", id)
            .is_none());

        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let durable =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace.clone());
        durable.emit(requested_at(100));
        durable.emit(resolved_at(110));
        durable.emit(requested_at(100));
        assert!(durable.pending_hitl_request("alpha", "prod", id).is_none());
        durable.emit(requested_at(120));
        assert!(durable.pending_hitl_request("alpha", "prod", id).is_some());
        drop(durable);
        let restarted =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace);
        let pending = restarted
            .pending_hitl_request("alpha", "prod", id)
            .expect("the next question survives a restart as pending");
        assert!(matches!(
            pending,
            RuntimeTransportEvent::HitlRequested { timestamp: 120, .. }
        ));
        restarted.emit(resolved_at(130));
        assert!(restarted
            .pending_hitl_request("alpha", "prod", id)
            .is_none());
        assert!(matches!(
            restarted.hitl_lifecycle_state("alpha", "prod", id),
            HitlLifecycleState::Resolved
        ));
    }

    #[test]
    fn large_hitl_history_replays_incrementally_and_preserves_latest_state() {
        use std::io::Write;

        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let correlation_id = "bot_auth:alpha:prod:large-history";
        let requested = serde_json::to_vec(&taskless_hitl_requested(correlation_id))
            .expect("serialize request lifecycle");
        let resolved = serde_json::to_vec(&RuntimeTransportEvent::HitlResolved {
            correlation_id: correlation_id.to_string(),
            source: "bot_auth".to_string(),
            outcome: "responded".to_string(),
            decision: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 101,
        })
        .expect("serialize resolution lifecycle");
        let path = workspace.base_root().join(HITL_LIFECYCLE_JOURNAL_FILENAME);
        let mut journal = std::io::BufWriter::new(File::create(path).expect("create journal"));
        for _ in 0..25_000 {
            journal.write_all(&requested).expect("request record");
            journal.write_all(b"\n").expect("request delimiter");
            journal.write_all(&resolved).expect("resolution record");
            journal.write_all(b"\n").expect("resolution delimiter");
        }
        journal.write_all(&requested).expect("final request");
        journal.write_all(b"\n").expect("final delimiter");
        journal.flush().expect("flush large journal");

        let recovered =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace);
        // Resolved, NOT Pending, and the trailing request is why.
        //
        // Resolution is terminal for a correlation id: the reducer refuses a
        // request against an already-resolved record and flags it invalid
        // rather than reopening the lifecycle. So the final request record
        // cannot move this state, and neither could the 24,999 request records
        // after the first pair -- which is worth knowing about this fixture,
        // because its real subject is that a 50,001-record journal replays at
        // all, not that later records win.
        let state = recovered.hitl_lifecycle_state("alpha", "prod", correlation_id);
        assert!(
            matches!(state, HitlLifecycleState::Resolved { .. }),
            "a resolved lifecycle is terminal; a trailing request must not reopen it, got {state:?}"
        );
    }

    #[test]
    fn oversized_hitl_record_is_skipped_without_hiding_later_known_transitions() {
        use std::io::Write;

        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let resolved_id = "bot_auth:alpha:prod:resolved-after-oversize";
        let pending_id = "bot_auth:alpha:prod:pending-after-oversize";
        let path = workspace.base_root().join(HITL_LIFECYCLE_JOURNAL_FILENAME);
        let mut journal = std::io::BufWriter::new(File::create(path).expect("create journal"));
        serde_json::to_writer(&mut journal, &taskless_hitl_requested(resolved_id))
            .expect("initial request");
        journal.write_all(b"\n").expect("request delimiter");
        journal
            .write_all(&vec![b'x'; MAX_HITL_LIFECYCLE_RECORD_BYTES + 1])
            .expect("oversized record");
        journal.write_all(b"\n").expect("oversized delimiter");
        serde_json::to_writer(
            &mut journal,
            &RuntimeTransportEvent::HitlResolved {
                correlation_id: resolved_id.to_string(),
                source: "bot_auth".to_string(),
                outcome: "responded".to_string(),
                decision: None,
                task_id: None,
                execution_id: None,
                agent_id: None,
                principal: Some("alpha".to_string()),
                workspace: Some("prod".to_string()),
                timestamp: 101,
            },
        )
        .expect("later resolution");
        journal.write_all(b"\n").expect("resolution delimiter");
        serde_json::to_writer(&mut journal, &taskless_hitl_requested(pending_id))
            .expect("later request");
        journal.write_all(b"\n").expect("pending delimiter");
        journal.flush().expect("flush journal");

        let recovered =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace);
        assert!(matches!(
            recovered.hitl_lifecycle_state("alpha", "prod", resolved_id),
            HitlLifecycleState::Resolved
        ));
        assert!(matches!(
            recovered.hitl_lifecycle_state("alpha", "prod", pending_id),
            HitlLifecycleState::Pending(_)
        ));
        assert!(matches!(
            recovered.hitl_lifecycle_state("alpha", "prod", "unknown-id"),
            HitlLifecycleState::Unavailable
        ));
    }

    #[test]
    fn excessive_depth_record_fails_closed_but_replay_continues() {
        use std::io::Write;

        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let known_id = "bot_auth:alpha:prod:after-deep-record";
        let path = workspace.base_root().join(HITL_LIFECYCLE_JOURNAL_FILENAME);
        let mut journal = std::io::BufWriter::new(File::create(path).expect("create journal"));
        let deep = format!(
            "{}null{}\n",
            "[".repeat(MAX_RETAINED_JSON_DEPTH + 1),
            "]".repeat(MAX_RETAINED_JSON_DEPTH + 1),
        );
        journal.write_all(deep.as_bytes()).expect("deep record");
        serde_json::to_writer(&mut journal, &taskless_hitl_requested(known_id))
            .expect("later request");
        journal.write_all(b"\n").expect("request delimiter");
        journal.flush().expect("flush journal");

        let recovered =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace);
        assert!(matches!(
            recovered.hitl_lifecycle_state("alpha", "prod", known_id),
            HitlLifecycleState::Pending(_)
        ));
        assert!(matches!(
            recovered.hitl_lifecycle_state("alpha", "prod", "unknown-id"),
            HitlLifecycleState::Unavailable
        ));
    }

    #[test]
    fn deep_hitl_schema_is_rejected_and_drained_on_a_small_stack() {
        std::thread::Builder::new()
            .name("hitl-schema-admission".to_string())
            .stack_size(256 * 1024)
            .spawn(|| {
                let broadcaster = RuntimeTransportBroadcaster::new(8);
                let mut receiver = broadcaster.subscribe();
                let mut schema = Value::Null;
                for _ in 0..10_000 {
                    schema = Value::Array(vec![schema]);
                }
                broadcaster.emit(RuntimeTransportEvent::HitlRequested {
                    correlation_id: "deep-hitl".to_string(),
                    source: "agentic".to_string(),
                    input_type: "text".to_string(),
                    prompt: "Question".to_string(),
                    hint: None,
                    input_schema: Some(schema),
                    task_id: None,
                    execution_id: None,
                    agent_id: None,
                    principal: Some("alpha".to_string()),
                    workspace: Some("prod".to_string()),
                    timestamp: 100,
                });
                assert!(matches!(
                    receiver.try_recv(),
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                ));
            })
            .expect("spawn small-stack HITL regression")
            .join()
            .expect("deep HITL rejection drains iteratively");
    }

    #[test]
    fn rejected_unwritten_hitl_does_not_poison_later_valid_lifecycle_persistence() {
        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let broadcaster =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace.clone());
        let mut schema = Value::Null;
        for _ in 0..(MAX_RETAINED_JSON_DEPTH + 1) {
            schema = Value::Array(vec![schema]);
        }
        broadcaster.emit(RuntimeTransportEvent::HitlRequested {
            correlation_id: "rejected-deep-hitl".to_string(),
            source: "agentic".to_string(),
            input_type: "text".to_string(),
            prompt: "Rejected".to_string(),
            hint: None,
            input_schema: Some(schema),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 100,
        });
        let valid_id = "bot_auth:alpha:prod:valid-after-rejection";
        broadcaster.emit(taskless_hitl_requested(valid_id));
        drop(broadcaster);

        let recovered =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace);
        assert!(matches!(
            recovered.hitl_lifecycle_state("alpha", "prod", valid_id),
            HitlLifecycleState::Pending(_)
        ));
        assert!(matches!(
            recovered.hitl_lifecycle_state("alpha", "prod", "rejected-deep-hitl"),
            HitlLifecycleState::Unknown
        ));
    }

    #[test]
    fn oversized_unscoped_hitl_is_rejected_before_transport_fanout() {
        let broadcaster = RuntimeTransportBroadcaster::new(8);
        let mut receiver = broadcaster.subscribe();
        broadcaster.emit_transport_only(RuntimeTransportEvent::HitlRequested {
            correlation_id: "oversized-unscoped-hitl".to_string(),
            source: "agentic".to_string(),
            input_type: "text".to_string(),
            prompt: "x".repeat(MAX_HITL_LIFECYCLE_RECORD_BYTES),
            hint: None,
            input_schema: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: None,
            workspace: None,
            timestamp: 100,
        });
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn oversized_scoped_hitl_is_rejected_without_durable_persistence() {
        let broadcaster = RuntimeTransportBroadcaster::new(8);
        let mut receiver = broadcaster.subscribe();
        let correlation_id = "oversized-scoped-hitl";
        broadcaster.emit_transport_only(RuntimeTransportEvent::HitlRequested {
            correlation_id: correlation_id.to_string(),
            source: "agentic".to_string(),
            input_type: "text".to_string(),
            prompt: "x".repeat(MAX_HITL_LIFECYCLE_RECORD_BYTES),
            hint: None,
            input_schema: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 100,
        });

        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            broadcaster.hitl_lifecycle_state("alpha", "prod", correlation_id),
            HitlLifecycleState::Unknown
        ));
    }

    #[test]
    fn truncated_hitl_journal_tail_is_repaired_before_the_next_append() {
        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let correlation_id = "bot_auth:alpha:prod:gmail";
        let request = serde_json::to_string(&taskless_hitl_requested(correlation_id))
            .expect("serialize request lifecycle");
        let truncated_resolution = r#"{"event_type":"hitl_resolved","correlation_id":"bot_auth"#;
        std::fs::write(
            workspace.base_root().join(HITL_LIFECYCLE_JOURNAL_FILENAME),
            format!("{request}\n{truncated_resolution}"),
        )
        .expect("write truncated lifecycle fixture");

        let recovered =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace.clone());
        assert!(matches!(
            recovered.hitl_lifecycle_state("alpha", "prod", correlation_id),
            HitlLifecycleState::Pending(_)
        ));
        recovered.emit(RuntimeTransportEvent::HitlResolved {
            correlation_id: correlation_id.to_string(),
            source: "bot_auth".to_string(),
            outcome: "responded".to_string(),
            decision: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 101,
        });
        drop(recovered);

        let restarted =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace);
        assert!(matches!(
            restarted.hitl_lifecycle_state("alpha", "prod", correlation_id),
            HitlLifecycleState::Resolved
        ));
    }

    #[test]
    fn corrupt_hitl_journal_preserves_known_state_and_fails_unknown_lookups_closed() {
        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let known_id = "bot_auth:alpha:prod:gmail";
        let valid = serde_json::to_string(&taskless_hitl_requested(known_id))
            .expect("serialize valid lifecycle");
        let resolved = serde_json::to_string(&RuntimeTransportEvent::HitlResolved {
            correlation_id: known_id.to_string(),
            source: "bot_auth".to_string(),
            outcome: "responded".to_string(),
            decision: None,
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 101,
        })
        .expect("serialize later lifecycle resolution");
        std::fs::write(
            workspace.base_root().join(HITL_LIFECYCLE_JOURNAL_FILENAME),
            format!("{valid}\n{{not-json}}\n{resolved}\n"),
        )
        .expect("write corrupt lifecycle fixture");

        let broadcaster =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace);
        assert!(matches!(
            broadcaster.hitl_lifecycle_state("alpha", "prod", known_id),
            HitlLifecycleState::Resolved
        ));
        assert!(matches!(
            broadcaster.hitl_lifecycle_state("alpha", "prod", "unknown-id"),
            HitlLifecycleState::Unavailable
        ));
    }

    #[test]
    fn failed_hitl_authority_commit_withholds_event_and_marks_recovery_unavailable() {
        let tmp = canonical_workspace_tempdir();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let broadcaster =
            RuntimeTransportBroadcaster::new(8).with_hitl_lifecycle_persistence(workspace.clone());
        // Replace the closed keyed store with a directory to make reopening
        // fail without relying on platform-specific permission changes.
        let authority_path = workspace
            .base_root()
            .join(HITL_LIFECYCLE_AUTHORITY_FILENAME);
        std::fs::remove_file(&authority_path).expect("remove keyed authority fixture");
        std::fs::create_dir(&authority_path).expect("create blocked authority path");
        let mut receiver = broadcaster.subscribe();
        let correlation_id = "bot_auth:alpha:prod:gmail";
        broadcaster.emit(taskless_hitl_requested(correlation_id));
        broadcaster.emit(taskless_hitl_requested("bot_auth:alpha:prod:calendar"));

        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            broadcaster.hitl_lifecycle_state("alpha", "prod", correlation_id),
            HitlLifecycleState::Unavailable
        ));
    }

    #[test]
    fn concurrent_hitl_canonical_registry_and_broadcast_order_stay_atomic() {
        let broadcaster = RuntimeTransportBroadcaster::new(8);
        let sink = Arc::new(BlockingCanonicalSink::default());
        broadcaster.set_runtime_canonical_event_sink(sink.clone());
        broadcaster.register_runtime_canonical_event_scope(CanonicalEventScope {
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            task_id: "task-1".to_string(),
            execution_id: "exec-1".to_string(),
            ui_thread_id: "general".to_string(),
        });
        let mut receiver = broadcaster.subscribe();
        let correlation_id = "pause-concurrent".to_string();
        let requested = RuntimeTransportEvent::HitlRequested {
            correlation_id: correlation_id.clone(),
            source: "agentic".to_string(),
            input_type: "text".to_string(),
            prompt: "Need input".to_string(),
            hint: None,
            input_schema: None,
            task_id: Some("task-1".to_string()),
            execution_id: Some("exec-1".to_string()),
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 100,
        };
        let resolved = RuntimeTransportEvent::HitlResolved {
            correlation_id: correlation_id.clone(),
            source: "agentic".to_string(),
            outcome: "responded".to_string(),
            decision: None,
            task_id: Some("task-1".to_string()),
            execution_id: Some("exec-1".to_string()),
            agent_id: None,
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 101,
        };

        let request_broadcaster = broadcaster.clone();
        let request_thread = std::thread::spawn(move || request_broadcaster.emit(requested));
        sink.wait_until_request_entered();
        assert!(matches!(
            broadcaster.hitl_lifecycle_lock.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ));
        let resolution_broadcaster = broadcaster.clone();
        let resolution_thread = std::thread::spawn(move || resolution_broadcaster.emit(resolved));
        sink.release_request();
        request_thread.join().expect("request emitter panicked");
        resolution_thread
            .join()
            .expect("resolution emitter panicked");

        assert!(matches!(
            receiver.try_recv().expect("request broadcast"),
            RuntimeTransportEvent::HitlRequested { .. }
        ));
        assert!(matches!(
            receiver.try_recv().expect("resolution broadcast"),
            RuntimeTransportEvent::HitlResolved { .. }
        ));
        assert!(broadcaster
            .pending_hitl_request("alpha", "prod", &correlation_id)
            .is_none());
        assert_eq!(
            *sink.writes.lock().expect("blocking sink writes poisoned"),
            vec![
                crate::magician_v2::artifact_v2::events::ArtifactV2EventType::HitlRequested,
                crate::magician_v2::artifact_v2::events::ArtifactV2EventType::HitlResolved,
            ]
        );
    }

    #[test]
    fn llm_correlation_rebinds_only_legacy_scope_and_preserves_call_identity() {
        let receipt = magicllm::LlmTraceReceipt::direct(magicllm::LlmTraceContext::legacy(
            Some("trace-root"),
            magicllm::LlmWorkloadClass::System,
        ));
        let correlation = LlmEventCorrelation::scoped(
            &receipt,
            "principal-a",
            "workspace-a",
            magicllm::LlmWorkloadClass::ForegroundChat,
        )
        .expect("legacy scope can be authoritatively rebound")
        .with_chat_turn("session-a", "turn-a");

        assert_eq!(correlation.trace_id, "trace-root");
        assert_eq!(correlation.llm_call_id, receipt.context.llm_call_id);
        assert_eq!(correlation.scope_resolution, "explicit");
        assert_eq!(correlation.workload_class, "foreground_chat");
        assert_eq!(correlation.chat_session_id.as_deref(), Some("session-a"));
        assert_eq!(correlation.chat_turn_id.as_deref(), Some("turn-a"));
    }

    #[test]
    fn llm_correlation_rejects_cross_scope_identity_collision() {
        let receipt = magicllm::LlmTraceReceipt::direct(magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("principal-a", "workspace-a"),
            magicllm::LlmWorkloadClass::ForegroundChat,
        ));

        assert!(LlmEventCorrelation::scoped(
            &receipt,
            "principal-b",
            "workspace-a",
            magicllm::LlmWorkloadClass::ForegroundChat,
        )
        .is_none());
    }

    #[test]
    fn llm_correlation_projection_mode_is_wire_visible_and_backward_compatible() {
        let mut correlation = LlmEventCorrelation::direct(
            "principal-a",
            "workspace-a",
            magicllm::LlmWorkloadClass::AutonomousTask,
        );
        correlation.iteration_id = Some("exec-1:step-1:7".to_string());
        correlation.prompt_projection_mode = Some("rebootstrap".to_string());

        let mut encoded = serde_json::to_value(&correlation).expect("serialize correlation");
        assert_eq!(encoded["iteration_id"], "exec-1:step-1:7");
        assert_eq!(encoded["prompt_projection_mode"], "rebootstrap");

        encoded
            .as_object_mut()
            .expect("correlation object")
            .remove("prompt_projection_mode");
        let legacy: LlmEventCorrelation =
            serde_json::from_value(encoded).expect("deserialize historical correlation");
        assert_eq!(legacy.iteration_id.as_deref(), Some("exec-1:step-1:7"));
        assert_eq!(legacy.prompt_projection_mode, None);
    }

    fn activity_started_with(
        workload_class: Option<&str>,
        agent_id: Option<&str>,
    ) -> RuntimeTransportEvent {
        RuntimeTransportEvent::ActivityStarted {
            activity_id: "1".to_string(),
            parent_activity_id: None,
            name: "unit".to_string(),
            target: "magician::demo".to_string(),
            kind: "background".to_string(),
            principal: None,
            workspace: None,
            workload_class: workload_class.map(str::to_string),
            agent_id: agent_id.map(str::to_string),
            thread_id: None,
            task_id: None,
            model: None,
            operation: None,
            dropped: 0,
            seq: "1".to_string(),
            timestamp: 0,
        }
    }

    /// An undeclared dimension is absent from the wire, not `null`.
    ///
    /// `skip_serializing_if` is what keeps an undeclared span free: at the
    /// measured event rate every always-present key is paid for on every row.
    /// It also keeps "undeclared" distinguishable from "declared as nothing"
    /// for a consumer, which is the distinction the whole undeclared-is-its-own
    /// -state rule rests on.
    #[test]
    fn undeclared_activity_dimensions_are_absent_from_the_wire() {
        let encoded = serde_json::to_value(activity_started_with(None, None))
            .expect("serialize an undeclared activity start");
        let map = encoded
            .get("data")
            .and_then(|data| data.as_object())
            .expect("the tagged event carries a data object");

        for key in [
            "workload_class",
            "agent_id",
            "thread_id",
            "task_id",
            "model",
            "operation",
        ] {
            assert!(
                !map.contains_key(key),
                "`{key}` is undeclared and must not appear on the wire at all, \
                 not even as null"
            );
        }
        // The counter that must never be confused with absent still ships.
        assert!(map.contains_key("dropped"));
    }

    /// A declared dimension survives the round trip.
    #[test]
    fn declared_activity_dimensions_reach_the_wire() {
        let encoded = serde_json::to_value(activity_started_with(Some("Ambient"), Some("presto")))
            .expect("serialize a declared activity start");
        let map = encoded
            .get("data")
            .and_then(|data| data.as_object())
            .expect("the tagged event carries a data object");

        assert_eq!(
            map.get("workload_class").and_then(|v| v.as_str()),
            Some("Ambient")
        );
        assert_eq!(map.get("agent_id").and_then(|v| v.as_str()), Some("presto"));
        assert!(
            !map.contains_key("thread_id"),
            "undeclared siblings stay absent"
        );
    }
}

/// The only agent id whose `chat.engine.updated` reaches every scope: the
/// binary's relay of `subscribe_chat_engine_changes`.
pub const CHAT_ENGINE_UPDATED_AGENT_ID: &str = "magician-plane";

pub fn event_visible_to_scope(
    event: &RuntimeTransportEvent,
    principal: &str,
    workspace: &str,
) -> bool {
    match event {
        RuntimeTransportEvent::FeedItemCreated { item, .. } => {
            feed_item_in_scope(item, principal, workspace)
        },
        RuntimeTransportEvent::ChatMessageReceived {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::MessageProcessingStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::QueryAnalysisCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::StrategySelected {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExplorationProgress {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExecutionStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExecutionStepStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExecutionStepCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExecutionPaused {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExecutionResumed {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExecutionFailed {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExecutionCancelled {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExecutionInflightResent {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExecutionInflightDropped {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExecutionCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExecutionRestoreFailed {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::MessageCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExecutionStatusChanged {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ExecutionResponsibilityChanged {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ProcessingError {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::LLMAnalysisStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::LLMAnalysisCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::LLMAnalysisFailed {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ClarificationSessionSnapshot {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ClarificationConfidenceSnapshot {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::SlotGraphDiff {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::WorkflowResumed {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::WorkflowStageResumed {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::WorkflowResumeFailed {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ObservabilityAlert {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::PipelineStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::PipelineStepStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::PipelineStepCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::PipelineCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::PipelineFailed {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AtomicPlanOutlineStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AtomicPlanOutlineCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AtomicPlanExpansionStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AtomicPlanGenerated {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ToolMatchingTierStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ToolMatchingTierCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::SlotExtractionStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::SlotExtracted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::SlotEnrichmentStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::SlotEnrichmentCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::SlotConfidenceUpdated {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ClarifiedTaskReady {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::LLMRequestSent {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::LLMResponseReceived {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::InferenceAttempted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ThinkingModeActivated {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ThinkingModeCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticExecutionStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticIterationStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticIterationCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticPageUnderstanding {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticDecisionMade {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticActionExecuted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticClickFallbackUsed {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticExecutionCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticStepStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticStepCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticStepFailed {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticWaitingForConfirmation {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticWaitingForUser {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticMaxIterationsReached {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::DomChangeDetected {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgenticResumed {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::SubGoalRequested {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::SubGoalOutcome {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ShellOutputChunk {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::InteractivePtyChunk {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::ParameterInferenceAttempted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ParameterInferred {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ParameterInferenceFailed {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ParameterDiscoveryAttempted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ParameterDiscovered {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ParameterDiscoveryFailed {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ParameterResolutionProgress {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgentCycleStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgentCycleCompleted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::AgentTriggered {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::V3PlanningStarted {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::V3PlanningProgress {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::V3PlanningCompleted {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::V3PlanningFailed {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::AgentDefinitionChanged {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::FeedItemUpdated {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::FeedItemRemoved {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::ExecutionPanelDelta {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::TaskCreated {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::TaskUpdated {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::TaskDeleted {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::AgenticStepStuckWarning {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::HitlRequested {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::HitlResolved {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::CriticalRequestAlert {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::CriticalRequestRetired {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::VerificationRetrievalStatus {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        // Activity spans route per-workspace when they carry scope, so a
        // workspace user can watch their own background work. All three
        // variants are scoped as a set: scoping the start while leaving
        // the finish system-only would show work beginning and never
        // ending.
        | RuntimeTransportEvent::ActivityStarted {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ActivityFinished {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ActivityProgress {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        }
        | RuntimeTransportEvent::ActivityCost {
            principal: Some(event_principal),
            workspace: Some(event_workspace),
            ..
        } => event_principal == principal && event_workspace == workspace,
        RuntimeTransportEvent::ChatMessageReceived { .. }
        | RuntimeTransportEvent::MessageProcessingStarted { .. }
        | RuntimeTransportEvent::QueryAnalysisCompleted { .. }
        | RuntimeTransportEvent::StrategySelected { .. }
        | RuntimeTransportEvent::ExplorationProgress { .. }
        | RuntimeTransportEvent::ExecutionStarted { .. }
        | RuntimeTransportEvent::ExecutionStepStarted { .. }
        | RuntimeTransportEvent::ExecutionStepCompleted { .. }
        | RuntimeTransportEvent::ExecutionPaused { .. }
        | RuntimeTransportEvent::ExecutionResumed { .. }
        | RuntimeTransportEvent::ExecutionFailed { .. }
        | RuntimeTransportEvent::ExecutionCancelled { .. }
        | RuntimeTransportEvent::ExecutionInflightResent { .. }
        | RuntimeTransportEvent::ExecutionInflightDropped { .. }
        | RuntimeTransportEvent::ExecutionCompleted { .. }
        | RuntimeTransportEvent::ExecutionRestoreFailed { .. }
        | RuntimeTransportEvent::MessageCompleted { .. }
        | RuntimeTransportEvent::ExecutionStatusChanged { .. }
        | RuntimeTransportEvent::ExecutionResponsibilityChanged { .. }
        | RuntimeTransportEvent::ProcessingError { .. }
        | RuntimeTransportEvent::LLMAnalysisStarted { .. }
        | RuntimeTransportEvent::LLMAnalysisCompleted { .. }
        | RuntimeTransportEvent::LLMAnalysisFailed { .. }
        | RuntimeTransportEvent::ClarificationSessionSnapshot { .. }
        | RuntimeTransportEvent::ClarificationConfidenceSnapshot { .. }
        | RuntimeTransportEvent::SlotGraphDiff { .. }
        | RuntimeTransportEvent::WorkflowResumed { .. }
        | RuntimeTransportEvent::WorkflowStageResumed { .. }
        | RuntimeTransportEvent::WorkflowResumeFailed { .. }
        | RuntimeTransportEvent::ObservabilityAlert { .. }
        | RuntimeTransportEvent::PipelineStarted { .. }
        | RuntimeTransportEvent::PipelineStepStarted { .. }
        | RuntimeTransportEvent::PipelineStepCompleted { .. }
        | RuntimeTransportEvent::PipelineCompleted { .. }
        | RuntimeTransportEvent::PipelineFailed { .. }
        | RuntimeTransportEvent::ClarificationMetricsSnapshot { .. }
        | RuntimeTransportEvent::AtomicPlanOutlineStarted { .. }
        | RuntimeTransportEvent::AtomicPlanOutlineCompleted { .. }
        | RuntimeTransportEvent::AtomicPlanExpansionStarted { .. }
        | RuntimeTransportEvent::AtomicPlanGenerated { .. }
        | RuntimeTransportEvent::ToolMatchingTierStarted { .. }
        | RuntimeTransportEvent::ToolMatchingTierCompleted { .. }
        | RuntimeTransportEvent::SlotExtractionStarted { .. }
        | RuntimeTransportEvent::SlotExtracted { .. }
        | RuntimeTransportEvent::SlotEnrichmentStarted { .. }
        | RuntimeTransportEvent::SlotEnrichmentCompleted { .. }
        | RuntimeTransportEvent::SlotConfidenceUpdated { .. }
        | RuntimeTransportEvent::ClarifiedTaskReady { .. }
        | RuntimeTransportEvent::AgentCycleStarted { .. }
        | RuntimeTransportEvent::AgentCycleCompleted { .. }
        | RuntimeTransportEvent::AgentTriggered { .. }
        | RuntimeTransportEvent::LLMRequestSent { .. }
        | RuntimeTransportEvent::LLMResponseReceived { .. }
        | RuntimeTransportEvent::InferenceAttempted { .. }
        | RuntimeTransportEvent::ThinkingModeActivated { .. }
        | RuntimeTransportEvent::ThinkingModeCompleted { .. }
        | RuntimeTransportEvent::AgenticExecutionStarted { .. }
        | RuntimeTransportEvent::AgenticIterationStarted { .. }
        | RuntimeTransportEvent::AgenticIterationCompleted { .. }
        | RuntimeTransportEvent::AgenticPageUnderstanding { .. }
        | RuntimeTransportEvent::AgenticDecisionMade { .. }
        | RuntimeTransportEvent::AgenticActionExecuted { .. }
        | RuntimeTransportEvent::AgenticClickFallbackUsed { .. }
        | RuntimeTransportEvent::AgenticExecutionCompleted { .. }
        | RuntimeTransportEvent::AgenticStepStarted { .. }
        | RuntimeTransportEvent::AgenticStepCompleted { .. }
        | RuntimeTransportEvent::AgenticStepFailed { .. }
        | RuntimeTransportEvent::AgenticWaitingForConfirmation { .. }
        | RuntimeTransportEvent::AgenticWaitingForUser { .. }
        | RuntimeTransportEvent::AgenticMaxIterationsReached { .. }
        | RuntimeTransportEvent::DomChangeDetected { .. }
        | RuntimeTransportEvent::AgenticResumed { .. }
        | RuntimeTransportEvent::SubGoalRequested { .. }
        | RuntimeTransportEvent::SubGoalOutcome { .. }
        | RuntimeTransportEvent::ShellOutputChunk { .. }
        | RuntimeTransportEvent::ParameterInferenceAttempted { .. }
        | RuntimeTransportEvent::ParameterInferred { .. }
        | RuntimeTransportEvent::ParameterInferenceFailed { .. }
        | RuntimeTransportEvent::ParameterDiscoveryAttempted { .. }
        | RuntimeTransportEvent::ParameterDiscovered { .. }
        | RuntimeTransportEvent::ParameterDiscoveryFailed { .. }
        | RuntimeTransportEvent::ParameterResolutionProgress { .. }
        | RuntimeTransportEvent::AgenticStepStuckWarning { .. }
        | RuntimeTransportEvent::HitlRequested { .. }
        | RuntimeTransportEvent::HitlResolved { .. }
        | RuntimeTransportEvent::CriticalRequestAlert { .. }
        | RuntimeTransportEvent::CriticalRequestRetired { .. }
        | RuntimeTransportEvent::VerificationRetrievalStatus { .. }
        // Unscoped activity — a span opened from code with no principal in
        // context, or a progress line below the span floor. Fails closed
        // to the system bucket like every other scope-less event rather
        // than being shown to whoever is listening.
        | RuntimeTransportEvent::ActivityStarted { .. }
        | RuntimeTransportEvent::ActivityFinished { .. }
        | RuntimeTransportEvent::ActivityProgress { .. }
        | RuntimeTransportEvent::ActivityCost { .. } => {
            // Scope fields missing — only visible when the consumer query
            // explicitly opts into the system/system bucket. Pure system
            // events (cron tickers, capability bootstrap, etc.) emit with
            // `principal: \"system\", workspace: \"system\"`; everything
            // else is fail-closed to prevent cross-workspace leakage.
            principal == "system" && workspace == "system"
        },
        RuntimeTransportEvent::AgentEvent { event } => {
            agent_event_in_scope(event, principal, workspace)
        },
        // Thinking-map change notices carry a REQUIRED scope (not Option like
        // the chat/execution group above) — strict equality, no system bucket.
        RuntimeTransportEvent::DecisionAccountingGap {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::DecisionShadowAgreement {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::ThinkingMapUpdated {
            principal: event_principal,
            workspace: event_workspace,
            ..
        }
        | RuntimeTransportEvent::ThinkingMapInterpretProgress {
            principal: event_principal,
            workspace: event_workspace,
            ..
        } => event_principal == principal && event_workspace == workspace,
        // Heartbeat is transport-level (per-connection ping); the fan-out
        // path doesn't actually broadcast it, but if one ever leaks into
        // the broadcaster keep it visible — losing it would break the
        // connection-liveness channel for everyone.
        RuntimeTransportEvent::Heartbeat { .. } => true,
        // ProgressEvent carries a ProgressMessage with explicit
        // principal/workspace on the inner message. Single-rail
        // migration: producers emit these onto the bus; the router
        // (and other subscribers) demux by scope.
        RuntimeTransportEvent::ProgressEvent { message, .. } => {
            message.principal == principal && message.workspace == workspace
        },
        // No catch-all — exhaustiveness is the policy. When a new
        // `RuntimeTransportEvent` variant is added, rustc forces this
        // match to be updated with an explicit scope decision (else the
        // build fails). Without the exhaustive guard, a forgotten
        // variant would silently fall through to `_ => true` and leak
        // across workspaces.
    }
}

fn feed_item_in_scope(item: &FeedItem, principal: &str, workspace: &str) -> bool {
    item.principal == principal && item.workspace == workspace
}

fn agent_event_in_scope(
    event: &crate::magician_v2::realtime_events::AgentEventEnvelope,
    principal: &str,
    workspace: &str,
) -> bool {
    match (event.principal.as_deref(), event.workspace.as_deref()) {
        (Some(event_principal), Some(event_workspace)) => {
            event_principal == principal && event_workspace == workspace
        },
        _ => {
            // The chat engine is one process-wide setting, not any scope's
            // data: every open composer must hear that it changed, and the
            // payload names an engine and a model, nothing else. Only the
            // plane's relay may speak for every scope under this name.
            if event.event_type == RuntimeAgentEventType::ChatEngineUpdated.as_str()
                && event.agent_id == CHAT_ENGINE_UPDATED_AGENT_ID
            {
                return true;
            }
            // No envelope scope — try to extract scope from the payload
            // (some agent events embed `principal`/`workspace` in their
            // event-specific JSON). Fall back to the system/system bucket
            // for unscoped envelopes so cron/system-agent events surface
            // when explicitly queried.
            if user_request_value_in_scope(&event.payload, principal, workspace) {
                return true;
            }
            principal == "system" && workspace == "system"
        },
    }
}

fn user_request_value_in_scope(
    request: &serde_json::Value,
    principal: &str,
    workspace: &str,
) -> bool {
    request
        .get("principal")
        .and_then(|value| value.as_str())
        .zip(request.get("workspace").and_then(|value| value.as_str()))
        .map(|(event_principal, event_workspace)| {
            event_principal == principal && event_workspace == workspace
        })
        .unwrap_or(false)
}
