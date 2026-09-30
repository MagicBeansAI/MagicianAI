use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::realtime_events::RuntimeTransportEvent;

use super::models::CanonicalEvent;

/// Host-owned provenance on canonical facts projected from the stateless
/// run-loop journal. Projectors overwrite these keys after mapping the runtime
/// event; a transport payload can never choose either value.
pub const SOURCE_EVENT_REF_FIELD: &str = "source_event_ref";
pub const CANONICAL_UI_THREAD_ID_FIELD: &str = "ui_thread_id";

/// Attach the immutable source identity required by post-persistence routers.
///
/// Canonical runtime mappings are object payloads. Failing closed if a future
/// mapping stops being one is preferable to durably acknowledging an event
/// whose idempotency or same-installation routing identity was silently lost.
pub(crate) fn attach_projected_source_identity(
    payload: &mut Value,
    source_event_ref: &str,
    ui_thread_id: &str,
) -> Result<(), String> {
    let Some(object) = payload.as_object_mut() else {
        return Err("canonical projected runtime-event payload is not an object".to_string());
    };
    object.insert(
        SOURCE_EVENT_REF_FIELD.to_string(),
        Value::String(source_event_ref.to_string()),
    );
    object.insert(
        CANONICAL_UI_THREAD_ID_FIELD.to_string(),
        Value::String(ui_thread_id.to_string()),
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalEventScope {
    pub principal: String,
    pub workspace: String,
    pub task_id: String,
    pub execution_id: String,
    /// UI thread id for chat-surface routing. Carried alongside scope
    /// so canonical events can be enriched with thread context without
    /// requiring downstream consumers to acquire the task-store flock.
    pub ui_thread_id: String,
}

pub trait RuntimeCanonicalEventSink: Send + Sync {
    /// Enqueue a transport-originated event for canonical persistence.
    ///
    /// Implementations may apply synchronous bounded backpressure. Canonical
    /// writers therefore must not emit back into the sink while handling an
    /// event: recursive emission could wait on capacity owned by the current
    /// append and would also make the canonical ordering contract ambiguous.
    fn emit(&self, scope: CanonicalEventScope, event_type: ArtifactV2EventType, payload: Value);

    /// Enqueue a canonical event and return a receipt that resolves only after
    /// its durable append has completed.
    ///
    /// The ordinary live transport path intentionally uses [`Self::emit`] so it
    /// can apply bounded synchronous backpressure without awaiting filesystem
    /// I/O. A durable outbox projector has a stronger obligation: it must not
    /// advance its own cursor merely because another in-memory queue accepted
    /// the event. Sinks that cannot provide that acknowledgement fail closed.
    fn emit_with_receipt(
        &self,
        _scope: CanonicalEventScope,
        _event_type: ArtifactV2EventType,
        _payload: Value,
    ) -> Result<RuntimeCanonicalEventReceipt, String> {
        Err("canonical runtime-event sink does not support durable receipts".to_string())
    }

    /// Register the process-wide observer invoked after each durable append.
    ///
    /// Implementations that support this seam must include observer failure in
    /// [`RuntimeCanonicalEventReceipt`]. That makes a durable source replay the
    /// same stable `source_event_ref`; observers must therefore be idempotent on
    /// that reference. The default preserves compatibility for external and
    /// test sinks while failing registration explicitly.
    fn register_post_persistence_observer(
        &self,
        _observer: Arc<dyn RuntimeCanonicalEventObserver>,
    ) -> Result<(), String> {
        Err("canonical runtime-event sink does not support observers".to_string())
    }
}

/// An asynchronous consumer of an already-persisted canonical event.
///
/// The event is immutable and includes canonical scope plus host-owned
/// `source_event_ref` and `ui_thread_id` payload fields for stateless projected
/// facts. Returning an error does not roll back the append; it fails the
/// projector's durability receipt so the stable source event is offered again.
/// An observer must not synchronously emit back into this same canonical sink:
/// it runs inside a bounded per-task sink lane, and a recursive blocking emit
/// could wait for capacity that this observation must release.
#[async_trait::async_trait]
pub trait RuntimeCanonicalEventObserver: Send + Sync {
    async fn observe_persisted(&self, event: &CanonicalEvent) -> Result<(), String>;
}

/// Completion of one canonical runtime-event append admitted by a sink.
pub struct RuntimeCanonicalEventReceipt {
    completed: tokio::sync::oneshot::Receiver<Result<(), String>>,
}

impl RuntimeCanonicalEventReceipt {
    /// Build a receipt from a sink-owned completion channel. This is public so
    /// out-of-crate implementations of the public
    /// [`RuntimeCanonicalEventSink`] trait can honor its durable-ack contract.
    pub fn new(completed: tokio::sync::oneshot::Receiver<Result<(), String>>) -> Self {
        Self { completed }
    }

    pub async fn wait_persisted(self) -> Result<(), String> {
        self.completed.await.map_err(|_| {
            "canonical runtime-event sink stopped before acknowledging persistence".to_string()
        })?
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtifactV2EventType {
    MessageProcessingStarted,
    QueryAnalysisCompleted,
    StrategySelected,
    ExplorationProgress,
    AtomicPlanOutlineStarted,
    AtomicPlanOutlineCompleted,
    AtomicPlanExpansionStarted,
    AtomicPlanGenerated,
    ClarificationSessionSnapshot,
    ClarificationConfidenceSnapshot,
    SlotGraphDiff,
    WorkflowResumed,
    WorkflowStageResumed,
    WorkflowResumeFailed,
    /// STEP 3 — a single-execution-context pipeline stage began (carried on the
    /// root execution scope so the chat card / deep panel can group activity by
    /// stage). Payload: stage_index, stage_count, origin_agent_id, stage_label.
    WorkflowStageStarted,
    /// STEP 3 — a single-execution-context pipeline stage finished. Payload:
    /// stage_index, origin_agent_id, outcome.
    WorkflowStageCompleted,
    ProcessingError,
    LlmAnalysisStarted,
    LlmAnalysisCompleted,
    LlmAnalysisFailed,
    ToolMatchingTierStarted,
    ToolMatchingTierCompleted,
    SlotExtractionStarted,
    SlotExtracted,
    SlotEnrichmentStarted,
    SlotEnrichmentCompleted,
    SlotConfidenceUpdated,
    ClarifiedTaskReady,
    ParameterInferenceAttempted,
    ParameterInferred,
    ParameterInferenceFailed,
    ParameterDiscoveryAttempted,
    ParameterDiscovered,
    ParameterDiscoveryFailed,
    PlanCreated,
    PlanRevised,
    StepStarted,
    StepCompleted,
    StepFailed,
    StepBlocked,
    StepSkipped,
    StepDelegated,
    RecipeCancellationRequested,
    InputRequested,
    InputReceived,
    InputResolved,
    InputPromotedToTaskScope,
    LlmRequested,
    LlmFirstToken,
    LlmSucceeded,
    LlmFailed,
    LlmRetryScheduled,
    ToolStarted,
    ToolSucceeded,
    ToolFailed,
    AgenticExecutionStarted,
    AgenticIterationStarted,
    AgenticDecisionMade,
    StepStuckWarning,
    AgenticExecutionCompleted,
    AgenticResumed,
    WaitingForConfirmation,
    MaxIterationsReached,
    SubGoalRequested,
    SubGoalOutcome,
    ChildSpawned,
    ChildCompleted,
    ChildFailed,
    ChildOutputAvailable,
    DelegationLaunchAuthorized,
    DelegationLaunchDispatched,
    DelegationLaunchFailed,
    DelegationChildLinked,
    DelegationChildAttachFailed,
    DelegationResultsReady,
    DelegationUnmatchedChildDispatched,
    ExecutionStarted,
    ExecutionRuntimeHandoff,
    ExecutionRuntimeResumeHandoff,
    ExecutionDiscovered,
    ExecutionWaiting,
    ExecutionWaitingOnChildren,
    ExecutionWaitingForUser,
    ExecutionResumed,
    ExecutionOutcomeObserved,
    ExecutionCompleted,
    ExecutionFailed,
    ExecutionCancelled,
    ExecutionDeferred,
    ExecutionStatusChanged,
    ExecutionResponsibilityChanged,
    ExecutionProgress,
    ExecutionFinalizerStarted,
    ExecutionFinalizerCompleted,
    ExecutionFinalizerFailed,
    TaskAgentFinalizerStarted,
    TaskAgentFinalizerCompleted,
    TaskUserFinalizerStarted,
    TaskUserFinalizerCompleted,
    ParentResumeAuthorized,
    ParentResumeDispatched,
    OutputCreated,
    OutputAvailable,
    OutputCandidateProposed,
    SummaryUpdated,
    ArtifactCreated,
    ArtifactCreateFailed,
    ArtifactReferenced,
    ArtifactUsed,
    ImportResolved,
    MemoryEpisodeRecorded,
    AgentCycleStarted,
    AgentCycleCompleted,
    HitlRequested,
    HitlResolved,
    /// A verification-controller lifecycle transition. One canonical type for
    /// all eleven verification kinds — the payload's `kind` field carries the
    /// stable wire name (`verification.queued` … `verification.cancelled`),
    /// so consumers filter on the payload rather than the type table growing
    /// an entry per transition.
    Verification,
    /// Task Recipe replay lifecycle. The payload's `kind` identifies the
    /// transition while one canonical type keeps the execution journal compact.
    RecipeReplay,
}

impl ArtifactV2EventType {
    pub const ALL: &'static [Self] = &[
        Self::MessageProcessingStarted,
        Self::QueryAnalysisCompleted,
        Self::StrategySelected,
        Self::ExplorationProgress,
        Self::AtomicPlanOutlineStarted,
        Self::AtomicPlanOutlineCompleted,
        Self::AtomicPlanExpansionStarted,
        Self::AtomicPlanGenerated,
        Self::ClarificationSessionSnapshot,
        Self::ClarificationConfidenceSnapshot,
        Self::SlotGraphDiff,
        Self::WorkflowResumed,
        Self::WorkflowStageResumed,
        Self::WorkflowResumeFailed,
        Self::WorkflowStageStarted,
        Self::WorkflowStageCompleted,
        Self::ProcessingError,
        Self::LlmAnalysisStarted,
        Self::LlmAnalysisCompleted,
        Self::LlmAnalysisFailed,
        Self::ToolMatchingTierStarted,
        Self::ToolMatchingTierCompleted,
        Self::SlotExtractionStarted,
        Self::SlotExtracted,
        Self::SlotEnrichmentStarted,
        Self::SlotEnrichmentCompleted,
        Self::SlotConfidenceUpdated,
        Self::ClarifiedTaskReady,
        Self::ParameterInferenceAttempted,
        Self::ParameterInferred,
        Self::ParameterInferenceFailed,
        Self::ParameterDiscoveryAttempted,
        Self::ParameterDiscovered,
        Self::ParameterDiscoveryFailed,
        Self::PlanCreated,
        Self::PlanRevised,
        Self::StepStarted,
        Self::StepCompleted,
        Self::StepFailed,
        Self::StepBlocked,
        Self::StepSkipped,
        Self::StepDelegated,
        Self::RecipeCancellationRequested,
        Self::InputRequested,
        Self::InputReceived,
        Self::InputResolved,
        Self::InputPromotedToTaskScope,
        Self::LlmRequested,
        Self::LlmFirstToken,
        Self::LlmSucceeded,
        Self::LlmFailed,
        Self::LlmRetryScheduled,
        Self::ToolStarted,
        Self::ToolSucceeded,
        Self::ToolFailed,
        Self::AgenticExecutionStarted,
        Self::AgenticIterationStarted,
        Self::AgenticDecisionMade,
        Self::StepStuckWarning,
        Self::AgenticExecutionCompleted,
        Self::AgenticResumed,
        Self::WaitingForConfirmation,
        Self::MaxIterationsReached,
        Self::SubGoalRequested,
        Self::SubGoalOutcome,
        Self::ChildSpawned,
        Self::ChildCompleted,
        Self::ChildFailed,
        Self::ChildOutputAvailable,
        Self::DelegationLaunchAuthorized,
        Self::DelegationLaunchDispatched,
        Self::DelegationLaunchFailed,
        Self::DelegationChildLinked,
        Self::DelegationChildAttachFailed,
        Self::DelegationResultsReady,
        Self::DelegationUnmatchedChildDispatched,
        Self::ExecutionStarted,
        Self::ExecutionRuntimeHandoff,
        Self::ExecutionRuntimeResumeHandoff,
        Self::ExecutionDiscovered,
        Self::ExecutionWaiting,
        Self::ExecutionWaitingOnChildren,
        Self::ExecutionWaitingForUser,
        Self::ExecutionResumed,
        Self::ExecutionOutcomeObserved,
        Self::ExecutionCompleted,
        Self::ExecutionFailed,
        Self::ExecutionCancelled,
        Self::ExecutionDeferred,
        Self::ExecutionStatusChanged,
        Self::ExecutionResponsibilityChanged,
        Self::ExecutionProgress,
        Self::ExecutionFinalizerStarted,
        Self::ExecutionFinalizerCompleted,
        Self::ExecutionFinalizerFailed,
        Self::TaskAgentFinalizerStarted,
        Self::TaskAgentFinalizerCompleted,
        Self::TaskUserFinalizerStarted,
        Self::TaskUserFinalizerCompleted,
        Self::ParentResumeAuthorized,
        Self::ParentResumeDispatched,
        Self::OutputCreated,
        Self::OutputAvailable,
        Self::OutputCandidateProposed,
        Self::SummaryUpdated,
        Self::ArtifactCreated,
        Self::ArtifactCreateFailed,
        Self::ArtifactReferenced,
        Self::ArtifactUsed,
        Self::ImportResolved,
        Self::MemoryEpisodeRecorded,
        Self::AgentCycleStarted,
        Self::AgentCycleCompleted,
        Self::HitlRequested,
        Self::HitlResolved,
        Self::Verification,
        Self::RecipeReplay,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MessageProcessingStarted => "message.processing_started",
            Self::QueryAnalysisCompleted => "query_analysis.completed",
            Self::StrategySelected => "strategy.selected",
            Self::ExplorationProgress => "exploration.progress",
            Self::AtomicPlanOutlineStarted => "atomic_plan.outline_started",
            Self::AtomicPlanOutlineCompleted => "atomic_plan.outline_completed",
            Self::AtomicPlanExpansionStarted => "atomic_plan.expansion_started",
            Self::AtomicPlanGenerated => "atomic_plan.generated",
            Self::ClarificationSessionSnapshot => "clarification.session_snapshot",
            Self::ClarificationConfidenceSnapshot => "clarification.confidence_snapshot",
            Self::SlotGraphDiff => "slot_graph.diff",
            Self::WorkflowResumed => "workflow.resumed",
            Self::WorkflowStageResumed => "workflow.stage_resumed",
            Self::WorkflowResumeFailed => "workflow.resume_failed",
            Self::WorkflowStageStarted => "workflow.stage_started",
            Self::WorkflowStageCompleted => "workflow.stage_completed",
            Self::ProcessingError => "processing.error",
            Self::LlmAnalysisStarted => "llm_analysis.started",
            Self::LlmAnalysisCompleted => "llm_analysis.completed",
            Self::LlmAnalysisFailed => "llm_analysis.failed",
            Self::ToolMatchingTierStarted => "tool_matching_tier.started",
            Self::ToolMatchingTierCompleted => "tool_matching_tier.completed",
            Self::SlotExtractionStarted => "slot_extraction.started",
            Self::SlotExtracted => "slot.extracted",
            Self::SlotEnrichmentStarted => "slot_enrichment.started",
            Self::SlotEnrichmentCompleted => "slot_enrichment.completed",
            Self::SlotConfidenceUpdated => "slot.confidence_updated",
            Self::ClarifiedTaskReady => "clarified_task.ready",
            Self::ParameterInferenceAttempted => "parameter_inference.attempted",
            Self::ParameterInferred => "parameter_inference.inferred",
            Self::ParameterInferenceFailed => "parameter_inference.failed",
            Self::ParameterDiscoveryAttempted => "parameter_discovery.attempted",
            Self::ParameterDiscovered => "parameter_discovery.discovered",
            Self::ParameterDiscoveryFailed => "parameter_discovery.failed",
            Self::PlanCreated => "plan.created",
            Self::PlanRevised => "plan.revised",
            Self::StepStarted => "step.started",
            Self::StepCompleted => "step.completed",
            Self::StepFailed => "step.failed",
            Self::StepBlocked => "step.blocked",
            Self::StepSkipped => "step.skipped",
            Self::StepDelegated => "step.delegated",
            Self::RecipeCancellationRequested => "recipe.cancellation_requested",
            Self::InputRequested => "input.requested",
            Self::InputReceived => "input.received",
            Self::InputResolved => "input.resolved",
            Self::InputPromotedToTaskScope => "input.promoted_to_task_scope",
            Self::LlmRequested => "llm.requested",
            Self::LlmFirstToken => "llm.first_token",
            Self::LlmSucceeded => "llm.succeeded",
            Self::LlmFailed => "llm.failed",
            Self::LlmRetryScheduled => "llm.retry_scheduled",
            Self::ToolStarted => "tool.started",
            Self::ToolSucceeded => "tool.succeeded",
            Self::ToolFailed => "tool.failed",
            Self::AgenticExecutionStarted => "agentic.execution_started",
            Self::AgenticIterationStarted => "agentic.iteration_started",
            Self::AgenticDecisionMade => "agentic.decision_made",
            Self::StepStuckWarning => "agentic.step_stuck_warning",
            Self::AgenticExecutionCompleted => "agentic.execution_completed",
            Self::AgenticResumed => "agentic.resumed",
            Self::WaitingForConfirmation => "waiting_for_confirmation",
            Self::MaxIterationsReached => "max_iterations_reached",
            Self::SubGoalRequested => "sub_goal.requested",
            Self::SubGoalOutcome => "sub_goal.outcome",
            Self::ChildSpawned => "child.spawned",
            Self::ChildCompleted => "child.completed",
            Self::ChildFailed => "child.failed",
            Self::ChildOutputAvailable => "child.output_available",
            Self::DelegationLaunchAuthorized => "delegation.launch_authorized",
            Self::DelegationLaunchDispatched => "delegation.launch_dispatched",
            Self::DelegationLaunchFailed => "delegation.launch_failed",
            Self::DelegationChildLinked => "delegation.child_linked",
            Self::DelegationChildAttachFailed => "delegation.child_attach_failed",
            Self::DelegationResultsReady => "delegation.results_ready",
            Self::DelegationUnmatchedChildDispatched => "delegation.unmatched_child_dispatched",
            Self::ExecutionStarted => "execution.started",
            Self::ExecutionRuntimeHandoff => "execution.runtime_handoff",
            Self::ExecutionRuntimeResumeHandoff => "execution.runtime_resume_handoff",
            Self::ExecutionDiscovered => "execution.discovered",
            Self::ExecutionWaiting => "execution.waiting",
            Self::ExecutionWaitingOnChildren => "execution.waiting_on_children",
            Self::ExecutionWaitingForUser => "execution.waiting_for_user",
            Self::ExecutionResumed => "execution.resumed",
            Self::ExecutionOutcomeObserved => "execution.outcome_observed",
            Self::ExecutionCompleted => "execution.completed",
            Self::ExecutionFailed => "execution.failed",
            Self::ExecutionCancelled => "execution.cancelled",
            Self::ExecutionDeferred => "execution.deferred",
            Self::ExecutionStatusChanged => "execution.status_changed",
            Self::ExecutionResponsibilityChanged => "execution.responsibility_changed",
            Self::ExecutionProgress => "execution.progress",
            Self::ExecutionFinalizerStarted => "execution_finalizer.started",
            Self::ExecutionFinalizerCompleted => "execution_finalizer.completed",
            Self::ExecutionFinalizerFailed => "execution_finalizer.failed",
            Self::TaskAgentFinalizerStarted => "task_agent_finalizer.started",
            Self::TaskAgentFinalizerCompleted => "task_agent_finalizer.completed",
            Self::TaskUserFinalizerStarted => "task_user_finalizer.started",
            Self::TaskUserFinalizerCompleted => "task_user_finalizer.completed",
            Self::ParentResumeAuthorized => "parent.resume_authorized",
            Self::ParentResumeDispatched => "parent.resume_dispatched",
            Self::OutputCreated => "output.created",
            Self::OutputAvailable => "output.available",
            Self::OutputCandidateProposed => "output_candidate.proposed",
            Self::SummaryUpdated => "summary.updated",
            Self::ArtifactCreated => "artifact.created",
            Self::ArtifactCreateFailed => "artifact.create_failed",
            Self::ArtifactReferenced => "artifact.referenced",
            Self::ArtifactUsed => "artifact.used",
            Self::ImportResolved => "import.resolved",
            Self::MemoryEpisodeRecorded => "memory.episode_recorded",
            Self::AgentCycleStarted => "agent.cycle.started",
            Self::AgentCycleCompleted => "agent.cycle.completed",
            Self::HitlRequested => "hitl.requested",
            Self::HitlResolved => "hitl.resolved",
            Self::Verification => "verification.lifecycle",
            Self::RecipeReplay => "recipe.replay",
        }
    }
}

impl std::fmt::Display for ArtifactV2EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Bridges verification lifecycle events onto the canonical runtime sink.
///
/// The verification module deliberately knows nothing about the runtime bus —
/// its controller takes a `VerificationEventSink` — so the seam constructs one
/// of these per driver. Principal and workspace are fixed at construction;
/// task and execution identity come from each event, so an adapter cannot
/// relabel another gate's evidence.
pub struct CanonicalVerificationEventSink {
    sink: std::sync::Arc<dyn RuntimeCanonicalEventSink>,
    principal: String,
    workspace: String,
}

impl CanonicalVerificationEventSink {
    pub fn new(
        sink: std::sync::Arc<dyn RuntimeCanonicalEventSink>,
        principal: String,
        workspace: String,
    ) -> Self {
        Self {
            sink,
            principal,
            workspace,
        }
    }
}

impl crate::magician_v2::execution::verification::VerificationEventSink
    for CanonicalVerificationEventSink
{
    fn emit(&self, event: &crate::magician_v2::execution::verification::VerificationEvent) {
        // Projection only: a serialization failure must never touch the pass.
        let Ok(mut payload) = serde_json::to_value(event) else {
            return;
        };
        if let Some(object) = payload.as_object_mut() {
            // The serde form of `kind` is the bare variant name; consumers
            // filter on the stable wire names, so those win.
            object.insert(
                "kind".to_string(),
                Value::String(event.kind.as_str().to_string()),
            );
        }
        self.sink.emit(
            CanonicalEventScope {
                principal: self.principal.clone(),
                workspace: self.workspace.clone(),
                task_id: event.root_task_id.clone(),
                execution_id: event.root_execution_id.clone(),
                // A gate does not know its chat thread; consumers join thread
                // context through the task projection instead.
                ui_thread_id: String::new(),
            },
            ArtifactV2EventType::Verification,
            payload,
        );
    }
}

#[derive(Debug, Clone)]
pub struct MappedRuntimeEvent {
    pub execution_id: String,
    pub event_type: ArtifactV2EventType,
    pub payload: Value,
    pub execution_status_hint: Option<String>,
    pub task_status_hint: Option<&'static str>,
    pub current_step_id: Option<String>,
    pub active_child_execution_ids: Option<Vec<String>>,
}

/// Whether `event` is a canonical runtime fact **of `scope`**, and its mapping
/// when it is.
///
/// # One rule, three former copies
///
/// This was written out three times before 2026-08-28: twice inside
/// `executor.rs::ActionExecutors::emit_event` — once as `should_emit_runtime_fact`
/// in the broadcaster branch and again inline in the canonical-sink branch — and
/// once as the first three checks of
/// `execution::agentic::run_loop::phases::outbox::routing_for`. They agreed, and
/// nothing kept them agreeing.
///
/// That stopped being merely untidy when the loop's final switch was thrown.
/// `routing_for`'s answer is written into `JournalBody::Event` and is what the
/// projector replays an event by, and **a record replays under the rule that was
/// in force when it was written** — so a divergence between the copies would
/// bake into the journal permanently, and correcting the rule afterwards would
/// not reach records already on disk.
///
/// # Why `routing_for` still is not just this
///
/// It calls this and then asks a fourth question this cannot: whether the
/// scope's execution is the one whose journal the record is being filed in. That
/// is a property of the RECORD, not of the event, and it produces
/// `RecordedEventRouting::Unrecorded` — an answer a live emit has no use for,
/// because a live emit has no record. Folding that branch in here would have
/// required inventing a journal id for `emit_event`, which has none; the
/// duplication that could honestly be removed is the part above, and only that
/// part was.
pub fn canonical_runtime_fact_of(
    scope: &CanonicalEventScope,
    event: &RuntimeTransportEvent,
) -> Option<MappedRuntimeEvent> {
    let mapped = map_v2_realtime_event(event)?;
    // Another run's event carried by this run's transports — a child's id, most
    // often. It is a runtime fact of THAT run, and this one must not persist it
    // under its own scope.
    (mapped.execution_id == scope.execution_id).then_some(mapped)
}

pub fn map_v2_realtime_event(event: &RuntimeTransportEvent) -> Option<MappedRuntimeEvent> {
    match event {
        RuntimeTransportEvent::MessageProcessingStarted {
            execution_id,
            turn_id,
            correlation_id,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::MessageProcessingStarted,
            payload: json!({
                "turn_id": turn_id,
                "correlation_id": correlation_id,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::QueryAnalysisCompleted {
            execution_id,
            correlation_id,
            complexity_score,
            intent,
            categories,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::QueryAnalysisCompleted,
            payload: json!({
                "correlation_id": correlation_id,
                "complexity_score": complexity_score,
                "intent": intent,
                "categories": categories,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::StrategySelected {
            execution_id,
            correlation_id,
            strategy,
            confidence,
            reason,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::StrategySelected,
            payload: json!({
                "correlation_id": correlation_id,
                "strategy": strategy,
                "confidence": confidence,
                "reason": reason,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ExplorationProgress {
            execution_id,
            correlation_id,
            nodes_explored,
            current_depth,
            best_score,
            current_task,
            progress_percent,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ExplorationProgress,
            payload: json!({
                "correlation_id": correlation_id,
                "nodes_explored": nodes_explored,
                "current_depth": current_depth,
                "best_score": best_score,
                "current_task": current_task,
                "progress_percent": progress_percent,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AtomicPlanOutlineStarted {
            execution_id,
            correlation_id,
            total_atomic_tools,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::AtomicPlanOutlineStarted,
            payload: json!({
                "correlation_id": correlation_id,
                "total_atomic_tools": total_atomic_tools,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AtomicPlanOutlineCompleted {
            execution_id,
            correlation_id,
            goals_count,
            confidence,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::AtomicPlanOutlineCompleted,
            payload: json!({
                "correlation_id": correlation_id,
                "goals_count": goals_count,
                "confidence": confidence,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AtomicPlanExpansionStarted {
            execution_id,
            correlation_id,
            goals_from_outline,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::AtomicPlanExpansionStarted,
            payload: json!({
                "correlation_id": correlation_id,
                "goals_from_outline": goals_from_outline,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AtomicPlanGenerated {
            execution_id,
            correlation_id,
            turn_id,
            validation_status,
            attempt_number,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::AtomicPlanGenerated,
            payload: json!({
                "correlation_id": correlation_id,
                "turn_id": turn_id,
                "validation_status": validation_status,
                "attempt_number": attempt_number,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        // `ClarificationQueued` / `ClarificationResponseReceived` mapping
        // removed in H7.2 along with the typed variants. Persistence via
        // canonical `HitlRequested` / `HitlResolved` with `source:
        // "clarification"` is a known follow-up.
        RuntimeTransportEvent::ClarificationSessionSnapshot {
            execution_id,
            state,
            total_questions,
            waiting_on_user,
            queued,
            answered,
            cancelled,
            pending_question_ids,
            active_batch,
            last_question_asked_at,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ClarificationSessionSnapshot,
            payload: json!({
                "state": state,
                "total_questions": total_questions,
                "waiting_on_user": waiting_on_user,
                "queued": queued,
                "answered": answered,
                "cancelled": cancelled,
                "pending_question_ids": pending_question_ids,
                "active_batch": active_batch,
                "last_question_asked_at": last_question_asked_at,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("waiting_for_user".to_string()),
            task_status_hint: Some("paused"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ClarificationConfidenceSnapshot {
            execution_id,
            question_id,
            trigger,
            overall_confidence,
            unresolved_count,
            slot_deltas,
            question_created_at,
            answered_at,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ClarificationConfidenceSnapshot,
            payload: json!({
                "question_id": question_id,
                "trigger": trigger,
                "overall_confidence": overall_confidence,
                "unresolved_count": unresolved_count,
                "slot_deltas": slot_deltas,
                "question_created_at": question_created_at,
                "answered_at": answered_at,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::SlotGraphDiff {
            execution_id,
            source,
            inserted,
            updated,
            removed,
            total_slots,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::SlotGraphDiff,
            payload: json!({
                "source": source,
                "inserted": inserted,
                "updated": updated,
                "removed": removed,
                "total_slots": total_slots,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::WorkflowResumed {
            execution_id,
            question_id,
            resume_mode,
            answered_count,
            pending_count,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::WorkflowResumed,
            payload: json!({
                "question_id": question_id,
                "resume_mode": resume_mode,
                "answered_count": answered_count,
                "pending_count": pending_count,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::WorkflowStageResumed {
            execution_id,
            stage_name,
            stage_context,
            attempt,
            reused_checkpoint,
            checkpoint_hash,
            reused_stages,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::WorkflowStageResumed,
            payload: json!({
                "stage_name": stage_name,
                "stage_context": stage_context,
                "attempt": attempt,
                "reused_checkpoint": reused_checkpoint,
                "checkpoint_hash": checkpoint_hash,
                "reused_stages": reused_stages,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::WorkflowResumeFailed {
            execution_id,
            question_id,
            error,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::WorkflowResumeFailed,
            payload: json!({
                "question_id": question_id,
                "error": error,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("failed".to_string()),
            task_status_hint: Some("failed"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ProcessingError {
            execution_id,
            correlation_id,
            error_message,
            error_type,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ProcessingError,
            payload: json!({
                "correlation_id": correlation_id,
                "error_message": error_message,
                "error_type": error_type,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("failed".to_string()),
            task_status_hint: Some("failed"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::LLMAnalysisStarted {
            execution_id,
            correlation_id,
            provider,
            stage,
            query_length,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::LlmAnalysisStarted,
            payload: json!({
                "correlation_id": correlation_id,
                "provider": provider,
                "stage": stage,
                "query_length": query_length,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::LLMAnalysisCompleted {
            execution_id,
            correlation_id,
            provider,
            stage,
            response_length,
            duration_ms,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::LlmAnalysisCompleted,
            payload: json!({
                "correlation_id": correlation_id,
                "provider": provider,
                "stage": stage,
                "response_length": response_length,
                "duration_ms": duration_ms,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::LLMAnalysisFailed {
            execution_id,
            correlation_id,
            provider,
            stage,
            error_type,
            error_message,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::LlmAnalysisFailed,
            payload: json!({
                "correlation_id": correlation_id,
                "provider": provider,
                "stage": stage,
                "error_type": error_type,
                "error_message": error_message,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("failed".to_string()),
            task_status_hint: Some("failed"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ToolMatchingTierStarted {
            execution_id,
            correlation_id,
            tier_number,
            tier_name,
            description,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ToolMatchingTierStarted,
            payload: json!({
                "correlation_id": correlation_id,
                "tier_number": tier_number,
                "tier_name": tier_name,
                "description": description,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ToolMatchingTierCompleted {
            execution_id,
            correlation_id,
            tier_number,
            tier_name,
            candidates_count,
            duration_ms,
            top_candidates,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ToolMatchingTierCompleted,
            payload: json!({
                "correlation_id": correlation_id,
                "tier_number": tier_number,
                "tier_name": tier_name,
                "candidates_count": candidates_count,
                "duration_ms": duration_ms,
                "top_candidates": top_candidates,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::SlotExtractionStarted {
            execution_id,
            correlation_id,
            message_length,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::SlotExtractionStarted,
            payload: json!({
                "correlation_id": correlation_id,
                "message_length": message_length,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::SlotExtracted {
            execution_id,
            correlation_id,
            slot_id,
            slot_type,
            confidence,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::SlotExtracted,
            payload: json!({
                "correlation_id": correlation_id,
                "slot_id": slot_id,
                "slot_type": slot_type,
                "confidence": confidence,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::SlotEnrichmentStarted {
            execution_id,
            correlation_id,
            total_slots,
            enricher_count,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::SlotEnrichmentStarted,
            payload: json!({
                "correlation_id": correlation_id,
                "total_slots": total_slots,
                "enricher_count": enricher_count,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::SlotEnrichmentCompleted {
            execution_id,
            correlation_id,
            total_slots,
            slots_changed,
            invocations,
            errors_count,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::SlotEnrichmentCompleted,
            payload: json!({
                "correlation_id": correlation_id,
                "total_slots": total_slots,
                "slots_changed": slots_changed,
                "invocations": invocations,
                "errors_count": errors_count,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::SlotConfidenceUpdated {
            execution_id,
            correlation_id,
            slot_id,
            old_confidence,
            new_confidence,
            source,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::SlotConfidenceUpdated,
            payload: json!({
                "correlation_id": correlation_id,
                "slot_id": slot_id,
                "old_confidence": old_confidence,
                "new_confidence": new_confidence,
                "source": source,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ClarifiedTaskReady {
            execution_id,
            correlation_id,
            clarified_task,
            objectives_count,
            constraints_count,
            confidence,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ClarifiedTaskReady,
            payload: json!({
                "correlation_id": correlation_id,
                "clarified_task": clarified_task,
                "objectives_count": objectives_count,
                "constraints_count": constraints_count,
                "confidence": confidence,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ParameterInferenceAttempted {
            execution_id,
            parameter_name,
            priority,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ParameterInferenceAttempted,
            payload: json!({
                "parameter_name": parameter_name,
                "priority": priority,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ParameterInferred {
            execution_id,
            parameter_name,
            inferred_value,
            confidence,
            method,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ParameterInferred,
            payload: json!({
                "parameter_name": parameter_name,
                "inferred_value": inferred_value,
                "confidence": confidence,
                "method": method,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ParameterInferenceFailed {
            execution_id,
            parameter_name,
            confidence,
            reason,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ParameterInferenceFailed,
            payload: json!({
                "parameter_name": parameter_name,
                "confidence": confidence,
                "reason": reason,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ParameterDiscoveryAttempted {
            execution_id,
            parameter_name,
            discovery_method,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ParameterDiscoveryAttempted,
            payload: json!({
                "parameter_name": parameter_name,
                "discovery_method": discovery_method,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ParameterDiscovered {
            execution_id,
            parameter_name,
            discovered_value,
            confidence,
            discovery_method,
            external_actions_performed,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ParameterDiscovered,
            payload: json!({
                "parameter_name": parameter_name,
                "discovered_value": discovered_value,
                "confidence": confidence,
                "discovery_method": discovery_method,
                "external_actions_performed": external_actions_performed,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ParameterDiscoveryFailed {
            execution_id,
            parameter_name,
            reason,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ParameterDiscoveryFailed,
            payload: json!({
                "parameter_name": parameter_name,
                "reason": reason,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("planning".to_string()),
            task_status_hint: Some("planning"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::SubGoalRequested {
            execution_id,
            plan_id,
            parent_step_id,
            sub_goal,
            budget_iterations,
            depth,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::SubGoalRequested,
            payload: json!({
                "plan_id": plan_id,
                "parent_step_id": parent_step_id,
                "sub_goal": sub_goal,
                "budget_iterations": budget_iterations,
                "depth": depth,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("waiting_for_children".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::SubGoalOutcome {
            execution_id,
            plan_id,
            parent_step_id,
            sub_goal,
            outcome,
            iterations_used,
            duration_ms,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::SubGoalOutcome,
            payload: json!({
                "plan_id": plan_id,
                "parent_step_id": parent_step_id,
                "sub_goal": sub_goal,
                "outcome": outcome,
                "iterations_used": iterations_used,
                "duration_ms": duration_ms,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::LLMRequestSent {
            execution_id,
            plan_id,
            step_id,
            step_index,
            capability,
            request_summary: _,
            input_tokens_estimate,
            budget_remaining,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::LlmRequested,
            payload: json!({
                "plan_id": plan_id,
                "step_id": step_id,
                "step_index": step_index,
                "capability": capability,
                "input_tokens_estimate": input_tokens_estimate,
                "budget_remaining": budget_remaining,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::LLMResponseReceived {
            execution_id,
            correlation,
            plan_id,
            step_id,
            step_index,
            capability,
            success,
            decision_summary,
            cost,
            latency_ms,
            error,
            provider,
            model,
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            ttft_ms,
            task_id,
            agent_id,
            delegated_agent_id,
            chat_session_id,
            operation,
            profile,
            attempt,
            response_kind,
            started_at_ms,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: if *success {
                ArtifactV2EventType::LlmSucceeded
            } else {
                ArtifactV2EventType::LlmFailed
            },
            payload: json!({
                "schema_version": correlation.as_ref().map(|value| value.schema_version),
                "trace_id": correlation.as_ref().map(|value| value.trace_id.as_str()),
                "llm_call_id": correlation.as_ref().map(|value| value.llm_call_id.as_str()),
                "provider_attempt_id": correlation.as_ref().and_then(|value| value.provider_attempt_id.as_deref()),
                "dispatch_job_id": correlation.as_ref().and_then(|value| value.dispatch_job_id.as_deref()),
                "parent_call_id": correlation.as_ref().and_then(|value| value.parent_call_id.as_deref()),
                "parent_relation": correlation.as_ref().and_then(|value| value.parent_relation.as_deref()),
                "retry_group_id": correlation.as_ref().and_then(|value| value.retry_group_id.as_deref()),
                "route_decision_id": correlation.as_ref().and_then(|value| value.route_decision_id.as_deref()),
                "scope_resolution": correlation.as_ref().map(|value| value.scope_resolution.as_str()),
                "correlated_task_id": correlation.as_ref().and_then(|value| value.task_id.as_deref()),
                "root_execution_id": correlation.as_ref().and_then(|value| value.root_execution_id.as_deref()),
                "correlated_execution_id": correlation.as_ref().and_then(|value| value.execution_id.as_deref()),
                "correlated_plan_id": correlation.as_ref().and_then(|value| value.plan_id.as_deref()),
                "correlated_step_id": correlation.as_ref().and_then(|value| value.step_id.as_deref()),
                "iteration_id": correlation.as_ref().and_then(|value| value.iteration_id.as_deref()),
                "prompt_projection_mode": correlation.as_ref().and_then(|value| value.prompt_projection_mode.as_deref()),
                "correlated_chat_session_id": correlation.as_ref().and_then(|value| value.chat_session_id.as_deref()),
                "chat_turn_id": correlation.as_ref().and_then(|value| value.chat_turn_id.as_deref()),
                "user_message_id": correlation.as_ref().and_then(|value| value.user_message_id.as_deref()),
                "workload_class": correlation.as_ref().map(|value| value.workload_class.as_str()),
                "call_role": correlation.as_ref().map(|value| value.call_role.as_str()),
                "provider_attempt_count": correlation.as_ref().map(|value| value.provider_attempt_count),
                "response_reused": correlation.as_ref().map(|value| value.response_reused),
                "plan_id": plan_id,
                "step_id": step_id,
                "step_index": step_index,
                "capability": capability,
                "decision_summary": decision_summary,
                "usage_availability": correlation.as_ref().and_then(|c| c.usage_availability),
                "cost": correlation.as_ref().and_then(|c| c.usage_availability).map_or(true, |u| u.cost).then_some(cost),
                "latency_ms": latency_ms,
                "error": error,
                "provider": provider,
                "model": model,
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "reasoning_tokens": reasoning_tokens,
                "cache_read_tokens": correlation.as_ref().and_then(|c| c.usage_availability).map_or(true, |u| u.cache_read).then_some(cache_read_tokens),
                "cache_creation_tokens": correlation.as_ref().and_then(|c| c.usage_availability).map_or(true, |u| u.cache_write).then_some(cache_creation_tokens),
                "ttft_ms": ttft_ms,
                "task_id": task_id,
                "agent_id": agent_id,
                "delegated_agent_id": delegated_agent_id,
                "chat_session_id": chat_session_id,
                "operation": operation,
                "profile": profile,
                "attempt": attempt,
                "response_kind": response_kind,
                "started_at_ms": started_at_ms,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AgenticIterationStarted {
            execution_id,
            plan_id,
            step_id,
            iteration,
            environment_type,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::AgenticIterationStarted,
            payload: json!({
                "plan_id": plan_id,
                "step_id": step_id,
                "iteration": iteration,
                "environment_type": environment_type,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AgenticStepStuckWarning {
            execution_id,
            plan_id,
            step_id,
            iteration,
            consecutive_count,
            recent_actions,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::StepStuckWarning,
            payload: json!({
                "plan_id": plan_id,
                "step_id": step_id,
                "iteration": iteration,
                "consecutive_count": consecutive_count,
                "recent_actions": recent_actions,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: None,
            task_status_hint: None,
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AgenticExecutionStarted {
            execution_id,
            plan_id,
            step_id,
            goal,
            success_criteria,
            max_iterations,
            hint_action,
            agent_id,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::AgenticExecutionStarted,
            payload: json!({
                "plan_id": plan_id,
                "step_id": step_id,
                "goal": goal,
                "success_criteria": success_criteria,
                "max_iterations": max_iterations,
                "hint_action": hint_action,
                "agent_id": agent_id,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AgenticDecisionMade {
            execution_id,
            plan_id,
            step_id,
            iteration,
            decision_type,
            action_summary,
            reasoning: _,
            confidence,
            thinking: _,
            evidence: _,
            tool_name,
            action_type,
            element_id,
            candidates_count,
            raw_decision: _,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::AgenticDecisionMade,
            payload: json!({
                "plan_id": plan_id,
                "step_id": step_id,
                "iteration": iteration,
                "decision_type": decision_type,
                "action_summary": action_summary,
                "confidence": confidence,
                "tool_name": tool_name,
                "action_type": action_type,
                "element_id": element_id,
                "candidates_count": candidates_count,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AgenticActionExecuted {
            execution_id,
            plan_id,
            step_id,
            iteration,
            action_type,
            target,
            success,
            latency_ms,
            error,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: if *success {
                ArtifactV2EventType::ToolSucceeded
            } else {
                ArtifactV2EventType::ToolFailed
            },
            payload: json!({
                "plan_id": plan_id,
                "step_id": step_id,
                "iteration": iteration,
                "action_type": action_type,
                "target": target,
                "latency_ms": latency_ms,
                "error": error,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AgenticWaitingForUser {
            execution_id,
            plan_id,
            step_id,
            iteration,
            pause_state_id,
            correlation_id,
            is_retry,
            retry_count,
            agent_id,
            goal_id,
            cycle_id,
            escalation_trigger,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::InputRequested,
            // HITL payload (question / input_type / hint / options /
            // input_schema / previous_answer / retry_reason) moved to the
            // canonical `hitl.requested { source: "agentic" }` row in
            // H7.4 lifecycle slim; this persistence record now only
            // captures lifecycle / scope identifiers. Consumers needing
            // the human-response payload should read the canonical row.
            payload: {
                let mut payload = json!({
                    "plan_id": plan_id,
                    "step_id": step_id,
                    "iteration": iteration,
                    "pause_state_id": pause_state_id,
                    "is_retry": is_retry,
                    "retry_count": retry_count,
                    "agent_id": agent_id,
                    "goal_id": goal_id,
                    "cycle_id": cycle_id,
                    "escalation_trigger": escalation_trigger,
                    "timestamp_ms": timestamp,
                });
                // diff_approval pauses carry the sibling ccp-<uuid> id here so
                // the attention page can pair THIS input.requested half against
                // the single ccp hitl.resolved. Non-diff-approval pauses leave
                // it None (their id space is `pause_state_id`).
                if let Some(correlation_id) = correlation_id {
                    payload["correlation_id"] = json!(correlation_id);
                }
                payload
            },
            execution_status_hint: Some("waiting_for_user".to_string()),
            task_status_hint: Some("paused"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AgenticWaitingForConfirmation {
            execution_id,
            plan_id,
            step_id,
            iteration,
            pause_state_id,
            agent_id,
            goal_id,
            cycle_id,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::WaitingForConfirmation,
            // HITL payload (action_summary / reason / action_type)
            // moved to the canonical `hitl.requested` row in H7.4.
            payload: json!({
                "plan_id": plan_id,
                "step_id": step_id,
                "iteration": iteration,
                "pause_state_id": pause_state_id,
                "agent_id": agent_id,
                "goal_id": goal_id,
                "cycle_id": cycle_id,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("waiting_for_confirmation".to_string()),
            task_status_hint: Some("paused"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AgenticMaxIterationsReached {
            execution_id,
            plan_id,
            step_id,
            iterations_used,
            pause_state_id,
            agent_id,
            goal_id,
            cycle_id,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::MaxIterationsReached,
            payload: json!({
                "plan_id": plan_id,
                "step_id": step_id,
                "iterations_used": iterations_used,
                "pause_state_id": pause_state_id,
                "agent_id": agent_id,
                "goal_id": goal_id,
                "cycle_id": cycle_id,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("max_iterations_reached".to_string()),
            task_status_hint: Some("paused"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AgenticExecutionCompleted {
            execution_id,
            plan_id,
            step_id,
            outcome,
            iterations_used,
            artifacts,
            duration_ms,
            summary,
            timestamp,
            refinement_pass_index,
            refinement_pending,
            ..
        } => {
            // tactical pattern T1 dedup at the persistence layer. The refinement
            // wrapper emits TWO AgenticExecutionCompleted events for one
            // logical run: a pass-0 (intermediate, `refinement_pending=true`)
            // and a pass-N (final, authoritative). The realtime bus keeps
            // both because the UI uses the pass-0 event to flip the task
            // card into "refining" state. The persistent task event log,
            // however, treats `agentic.execution_completed` as a terminal
            // marker — downstream analytics, replay tooling, and the
            // event-counted "did the execution finish?" projection would
            // double-count refined runs. Drop the intermediate event from
            // the persistent log so each logical execution has exactly
            // one terminal record. The pass-N event (where
            // `refinement_pending=false`) carries the canonical outcome,
            // total iterations (sum across passes per `merge_refinement_outcomes`),
            // and final artifact set.
            if *refinement_pending {
                return None;
            }
            let _ = refinement_pass_index;
            Some(MappedRuntimeEvent {
                execution_id: execution_id.clone(),
                event_type: ArtifactV2EventType::AgenticExecutionCompleted,
                payload: json!({
                    "plan_id": plan_id,
                    "step_id": step_id,
                    "outcome": outcome,
                    "iterations_used": iterations_used,
                    "artifacts": artifacts,
                    "duration_ms": duration_ms,
                    "summary": summary,
                    "timestamp_ms": timestamp,
                    "refinement_pass_index": refinement_pass_index,
                }),
                execution_status_hint: None,
                task_status_hint: None,
                current_step_id: None,
                active_child_execution_ids: None,
            })
        },
        RuntimeTransportEvent::AgenticResumed {
            execution_id,
            pause_state_id,
            plan_id,
            step_id,
            resumed_from_iteration,
            input_type,
            user_responded,
            agent_id,
            goal_id,
            cycle_id,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::AgenticResumed,
            payload: json!({
                "plan_id": plan_id,
                "step_id": step_id,
                "pause_state_id": pause_state_id,
                "resumed_from_iteration": resumed_from_iteration,
                "input_type": input_type,
                "user_responded": user_responded,
                "agent_id": agent_id,
                "goal_id": goal_id,
                "cycle_id": cycle_id,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ExecutionStatusChanged {
            execution_id,
            task_id,
            root_execution_id,
            previous_status,
            new_status,
            reason,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ExecutionStatusChanged,
            payload: json!({
                "task_id": task_id,
                "root_execution_id": root_execution_id,
                "previous_status": previous_status,
                "new_status": new_status,
                "reason": reason,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: normalize_waiting_state_status(new_status).map(str::to_string),
            // Terminal statuses suppressed: the 2-step reducer owns the terminal
            // task write (ordered after synthesis). See `task_status_hint_non_terminal`.
            task_status_hint: normalize_waiting_state_status(new_status)
                .and_then(task_status_hint_non_terminal),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::ExecutionResponsibilityChanged {
            execution_id,
            parent_execution_id,
            task_id,
            root_execution_id,
            waiting_state,
            active_owner_agent_id,
            owner_stack,
            active_delegation_group,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::ExecutionResponsibilityChanged,
            payload: json!({
                "parent_execution_id": parent_execution_id,
                "task_id": task_id,
                "root_execution_id": root_execution_id,
                "waiting_state": waiting_state,
                "active_owner_agent_id": active_owner_agent_id,
                "owner_stack": owner_stack,
                "active_delegation_group": active_delegation_group,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: normalize_waiting_state_status(waiting_state)
                .map(str::to_string),
            // Terminal statuses suppressed: the 2-step reducer owns the terminal
            // task write (ordered after synthesis). See `task_status_hint_non_terminal`.
            task_status_hint: normalize_waiting_state_status(waiting_state)
                .and_then(task_status_hint_non_terminal),
            current_step_id: None,
            active_child_execution_ids: Some(active_delegation_group.clone()),
        }),
        // Canonical HITL envelopes — persisted under the synthetic
        // `hitl.requested` / `hitl.resolved` event types so events.jsonl
        // replay (and V3 attention projection) sees pending/resolved
        // human-in-the-loop pauses. `correlation_id` carries the original
        // pause/approval/request id; `source` carries the originating
        // surface (`agentic`, `user_request`, `approval`, `clarification`).
        RuntimeTransportEvent::HitlRequested {
            correlation_id,
            source,
            input_type,
            prompt,
            hint,
            input_schema,
            task_id,
            execution_id: Some(execution_id),
            agent_id,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::HitlRequested,
            payload: json!({
                "correlation_id": correlation_id,
                "source": source,
                "input_type": input_type,
                "prompt": prompt,
                "hint": hint,
                "input_schema": input_schema,
                "task_id": task_id,
                "agent_id": agent_id,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("waiting_for_user".to_string()),
            task_status_hint: Some("paused"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::HitlResolved {
            correlation_id,
            source,
            outcome,
            decision,
            task_id,
            execution_id: Some(execution_id),
            agent_id,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::HitlResolved,
            payload: json!({
                "correlation_id": correlation_id,
                "source": source,
                "outcome": outcome,
                "decision": decision,
                "task_id": task_id,
                "agent_id": agent_id,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AgentCycleStarted {
            execution_id: Some(execution_id),
            agent_id,
            goal_id,
            cycle_id,
            goal,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::AgentCycleStarted,
            payload: json!({
                "agent_id": agent_id,
                "goal_id": goal_id,
                "cycle_id": cycle_id,
                "goal": goal,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AgentCycleCompleted {
            execution_id: Some(execution_id),
            agent_id,
            goal_id,
            cycle_id,
            outcome,
            iterations_used,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::AgentCycleCompleted,
            payload: json!({
                "agent_id": agent_id,
                "goal_id": goal_id,
                "cycle_id": cycle_id,
                "outcome": outcome,
                "iterations_used": iterations_used,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: Some("running".to_string()),
            task_status_hint: Some("running"),
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AgenticStepCompleted {
            execution_id,
            plan_id,
            step_id,
            iteration,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::StepCompleted,
            payload: json!({
                "plan_id": plan_id,
                "step_id": step_id,
                "iteration": iteration,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: None,
            task_status_hint: None,
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        RuntimeTransportEvent::AgenticStepFailed {
            execution_id,
            plan_id,
            step_id,
            iteration,
            timestamp,
            ..
        } => Some(MappedRuntimeEvent {
            execution_id: execution_id.clone(),
            event_type: ArtifactV2EventType::StepFailed,
            payload: json!({
                "plan_id": plan_id,
                "step_id": step_id,
                "iteration": iteration,
                "timestamp_ms": timestamp,
            }),
            execution_status_hint: None,
            task_status_hint: None,
            current_step_id: None,
            active_child_execution_ids: None,
        }),
        _ => None,
    }
}

pub fn task_status_hint(execution_status: &str) -> Option<&'static str> {
    match execution_status {
        "planning" => Some("planning"),
        "ready" => Some("ready"),
        "running" => Some("running"),
        "waiting_for_user" | "waiting_for_confirmation" | "max_iterations_reached" => {
            Some("paused")
        },
        "waiting_for_children" => Some("running"),
        "completed" => Some("completed"),
        "failed" => Some("failed"),
        "cancelled" => Some("cancelled"),
        _ => None,
    }
}

/// Like [`task_status_hint`] but NEVER returns a terminal task status
/// (`completed`/`failed`/`cancelled`).
///
/// The realtime-event bridge (`V3RuntimeEventBridge`) feeds `task_status_hint`
/// into `reduce_runtime_signal`, which writes `task.state.status` directly. That
/// path runs on a separate async task and races AHEAD of the authoritative
/// 2-step terminal reducer (`reduce_execution_terminal_status_only` → synthesis →
/// `reduce_execution_terminal`). If it stamps a terminal status, a chat task is
/// marked `completed` 5-70s BEFORE its deliverable outputs register — so fan-outs
/// tearing down on `task.status_changed: completed` miss the later
/// `output.available` events. Step 1 (`reduce_execution_terminal_status_only`)
/// already writes the terminal task status UNCONDITIONALLY for every terminal
/// outcome (via `persist_execution_outcome`), so suppressing the hint here strands
/// nothing — it just lets the authoritative path own the terminal write, ordered
/// after synthesis. Non-terminal hints (`planning`/`ready`/`running`/`paused`)
/// flow through unchanged so live progress + active-root tracking are unaffected.
fn task_status_hint_non_terminal(execution_status: &str) -> Option<&'static str> {
    match task_status_hint(execution_status) {
        Some("completed") | Some("failed") | Some("cancelled") => None,
        other => other,
    }
}

fn normalize_waiting_state_status(raw: &str) -> Option<&'static str> {
    match raw {
        "Planning" | "planning" => Some("planning"),
        "PlanningComplete" | "ready" => Some("ready"),
        "Runnable" | "Executing" | "Running" | "running" => Some("running"),
        "WaitingChildren" | "WaitingForChildren" | "waiting_for_children" => {
            Some("waiting_for_children")
        },
        "WaitingUser" | "WaitingForUser" | "waiting_for_user" => Some("waiting_for_user"),
        "WaitingForConfirmation" | "waiting_for_confirmation" => Some("waiting_for_confirmation"),
        "MaxIterationsReached" | "max_iterations_reached" => Some("max_iterations_reached"),
        "Paused" | "paused" => Some("paused"),
        "Completed" | "completed" => Some("completed"),
        "Failed" | "failed" => Some("failed"),
        "Cancelled" | "cancelled" => Some("cancelled"),
        _ => None,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn projected_source_identity_is_host_owned_and_complete() {
        let mut payload = serde_json::json!({
            "source_event_ref": "transport-chosen",
            "ui_thread_id": "app:wrong",
            "value": 7,
        });
        attach_projected_source_identity(
            &mut payload,
            "evt_loop_v1_authoritative",
            "app:installation-1",
        )
        .expect("object mapping accepts host provenance");

        assert_eq!(
            payload[SOURCE_EVENT_REF_FIELD],
            serde_json::json!("evt_loop_v1_authoritative")
        );
        assert_eq!(
            payload[CANONICAL_UI_THREAD_ID_FIELD],
            serde_json::json!("app:installation-1")
        );
        assert_eq!(payload["value"], serde_json::json!(7));

        let mut non_object = serde_json::json!(["mapped-event"]);
        assert!(attach_projected_source_identity(
            &mut non_object,
            "evt_loop_v1_authoritative",
            "app:installation-1",
        )
        .is_err());
    }

    #[test]
    fn step_stuck_warning_event_type_has_expected_tag() {
        assert_eq!(
            ArtifactV2EventType::StepStuckWarning.as_str(),
            "agentic.step_stuck_warning"
        );
        assert!(ArtifactV2EventType::ALL.contains(&ArtifactV2EventType::StepStuckWarning));
    }

    #[test]
    fn step_stuck_warning_maps_to_artifact_event() {
        use crate::magician_v2::realtime_events::RuntimeTransportEvent;
        let evt = RuntimeTransportEvent::AgenticStepStuckWarning {
            execution_id: "exec-1".into(),
            principal: Some("anonymous".into()),
            workspace: Some("default".into()),
            plan_id: "plan-1".into(),
            step_id: "step-1".into(),
            iteration: 5,
            is_preflight: false,
            consecutive_count: 3,
            recent_actions: vec!["click(x)".into(), "click(y)".into(), "scroll".into()],
            timestamp: 1_700_000_000_000,
        };
        let mapped = map_v2_realtime_event(&evt).expect("should map");
        assert_eq!(mapped.event_type, ArtifactV2EventType::StepStuckWarning);
        assert_eq!(mapped.payload["iteration"], 5);
        assert_eq!(mapped.payload["consecutive_count"], 3);
        assert_eq!(
            mapped.payload["recent_actions"].as_array().unwrap().len(),
            3
        );
    }

    #[test]
    fn completed_execution_status_change_suppresses_terminal_task_status_hint() {
        use crate::magician_v2::realtime_events::RuntimeTransportEvent;
        let make = |new_status: &str| RuntimeTransportEvent::ExecutionStatusChanged {
            execution_id: "exec-1".into(),
            principal: Some("anonymous".into()),
            workspace: Some("default".into()),
            task_id: Some("task-1".into()),
            root_execution_id: Some("exec-1".into()),
            previous_status: "Running".into(),
            new_status: new_status.into(),
            reason: None,
            timestamp: 1_700_000_000_000,
        };

        // Terminal: the EXECUTION status still flips live, but the terminal TASK
        // status hint is suppressed so the realtime-signal path can't mark the task
        // completed before the 2-step reducer registers the deliverable outputs.
        let completed = map_v2_realtime_event(&make("Completed")).expect("should map");
        assert_eq!(
            completed.execution_status_hint.as_deref(),
            Some("completed")
        );
        assert_eq!(
            completed.task_status_hint, None,
            "terminal task status must NOT be stamped from the realtime-signal path"
        );

        // Non-terminal: hint flows through unchanged (live progress preserved).
        let running = map_v2_realtime_event(&make("Running")).expect("should map");
        assert_eq!(running.execution_status_hint.as_deref(), Some("running"));
        assert_eq!(running.task_status_hint, Some("running"));
    }

    #[test]
    fn task_status_hint_non_terminal_drops_only_terminal_statuses() {
        assert_eq!(task_status_hint_non_terminal("completed"), None);
        assert_eq!(task_status_hint_non_terminal("failed"), None);
        assert_eq!(task_status_hint_non_terminal("cancelled"), None);
        assert_eq!(task_status_hint_non_terminal("running"), Some("running"));
        assert_eq!(task_status_hint_non_terminal("planning"), Some("planning"));
        // `task_status_hint` takes an EXECUTION status and maps it; "paused" is an
        // output value, not an input. `waiting_for_user` is a real non-terminal
        // execution status that maps to the "paused" task status — and must pass
        // through unchanged (only completed/failed/cancelled are dropped).
        assert_eq!(
            task_status_hint_non_terminal("waiting_for_user"),
            Some("paused")
        );
    }
}
