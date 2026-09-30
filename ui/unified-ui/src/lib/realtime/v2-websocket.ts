// V2 WebSocket client for real-time message processing events
import { get, writable } from 'svelte/store';
import type { FeedItem, FeedItemPatch } from '$lib/feed/types';
import type { ExecutionPanelState } from '$lib/types/executionPanel';
import { KNOWN_EVENT_TYPES } from '$lib/realtime/event-taxonomy';
import {
    handleAgentEvent,
    handleAgentEnvelopeEvent,
    reconcileAgentDefinitionChange
} from '$lib/stores/agentStore';
import {
    handleApprovalEnvelopeEvent,
    handleCanonicalHitlApprovalEvent
} from '$lib/stores/approvalStore';
import { handleAgentUpdateEnvelopeEvent } from '$lib/stores/agentUpdateStore';
import { handleAgenticStreamEvent } from '$lib/stores/agenticStreamStore';
import {
    handleMuijEvent,
    requestMuijSnapshots,
    clearAgentMuij,
    handleMuijSnapshot,
    handleMuijSnapshotError,
    onAgentCycleStarted,
    onDisconnect as muijOnDisconnect
} from '$lib/stores/muijStore';
import { chatStore } from '$lib/stores/chatStore';
import { scopeIdentityStore, scopedWebSocketProtocols, scopedMagicianWebSocketUrl } from '$lib/stores/scopeIdentityStore';
import { handleMediaPreferencesUpdatedEnvelope } from '$lib/media/preferences';
import { handleMediaConfigUpdatedEnvelope } from '$lib/media/audioSettings';
import { mediaProvidersStore } from '$lib/media/providers';
import { handleUiPreferencesUpdatedEnvelope } from '$lib/shared/stores/themeStore';
import { handleChatEngineUpdatedEnvelope } from '$lib/stores/chatHarnessPreferenceStore';

export const MAGICIAN_REALTIME_WEBSOCKET_PROTOCOL = 'magician-events-v2';

// V2 Event Types matching backend
export interface V2BaseEvent {
    execution_id?: string;
    correlation_id?: string;
    timestamp: number;
}

export interface MessageProcessingStartedEvent extends V2BaseEvent {
    execution_id: string;
    turn_id: string;
    correlation_id: string;
}

export interface QueryAnalysisCompletedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    complexity_score: number;
    intent: string;
    categories: string[];
}

export interface StrategySelectedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    strategy: string;
    confidence: number;
    reason: string;
}

export interface ExplorationProgressEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    nodes_explored: number;
    current_depth: number;
    best_score: number;
    current_task: string;
    progress_percent: number;
}

// NOTE: ToolMatchingEvent removed - superseded by ToolMatchingTierStarted/Completed

export interface ExplorationSummary {
    strategy_used: string;
    nodes_explored: number;
    max_depth_reached: number;
    best_tool: string;
    best_confidence: number;
    total_execution_time_ms: number;
}

export interface MessageCompletedEvent extends V2BaseEvent {
    execution_id: string;
    turn_id: string;
    correlation_id: string;
    response: string;
    exploration_summary?: ExplorationSummary;
}

// NOTE: ProcessingCancelledEvent removed - ExecutionCancelled covers cancellation

export interface ProcessingErrorEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    error_message: string;
    error_type: string;
}

export interface LLMAnalysisStartedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    provider: string; // "openai", "anthropic", "ollama"
    query_length: number;
}

export interface LLMAnalysisCompletedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    provider: string;
    response_length: number;
    duration_ms: number;
}

export interface LLMAnalysisFailedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    provider: string;
    error_type: string; // "api_key_missing", "timeout", "invalid_response", "network_error"
    error_message: string;
}

export interface HeartbeatEvent {
    timestamp: number;
}

export interface TierCandidate {
    tool_name: string;
    score: number;
    category: string;
}

export interface ToolMatchingTierStartedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    tier_number: number; // 0-4
    tier_name: string; // "Category Pre-Filter", "Rule-Based", "Semantic", "Candidate Selection", "LLM Evaluation"
    description: string;
}

export interface ToolMatchingTierCompletedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    tier_number: number;
    tier_name: string;
    candidates_count: number;
    duration_ms: number;
    top_candidates: TierCandidate[];
}

// NOTE: CategoryFuzzyMatchingEvent removed - superseded by ExplorationProgress

export interface AtomicPlanOutlineStartedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    total_atomic_tools: number;
    query?: string;
}

export interface AtomicPlanOutlineCompletedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    goals_count: number;
    confidence: number;
}

export interface AtomicPlanExpansionStartedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    goals_from_outline: number;
}

export interface PlanGraph {
    steps: any[];
    edges: any[];
    unresolved_inputs: any[];
    confidence: number;
}

export interface AtomicPlanGeneratedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    turn_id: string;
    plan_graph: PlanGraph;
    validation_status: string;
    attempt_number: number;
    steps_count?: number;
    confidence?: number;
    provenance?: string;
}

export interface ClarificationSessionSnapshotEvent extends V2BaseEvent {
    execution_id: string;
    state: string;
    total_questions: number;
    waiting_on_user: number;
    queued: number;
    answered: number;
    cancelled: number;
    pending_question_ids: string[];
    active_batch?: {
        batch_id: string;
        total: number;
        answered: number;
    } | null;
    last_question_asked_at?: number | null;
}

export interface ClarificationConfidenceSlotDelta {
    slot_id: string;
    previous?: number | null;
    updated: number;
}

export interface ClarificationConfidenceSnapshotEvent extends V2BaseEvent {
    execution_id: string;
    question_id: string;
    trigger: string;
    overall_confidence: number;
    unresolved_count: number;
    slot_deltas: ClarificationConfidenceSlotDelta[];
    question_created_at: number;
    answered_at: number;
}

export interface ClarificationMetricsSnapshotEvent extends V2BaseEvent {
    total_sessions_started: number;
    total_sessions_completed: number;
    active_sessions: number;
    avg_session_duration_ms?: number | null;
    avg_questions_per_session?: number | null;
    guardrail_timeouts: number;
    guardrail_question_caps: number;
    guardrail_round_caps: number;
}

export interface WorkflowResumedEvent extends V2BaseEvent {
    execution_id: string;
    question_id?: string | null;
    resume_mode: string;
    answered_count: number;
    pending_count: number;
}

export interface ObservabilityAlertEvent extends V2BaseEvent {
    execution_id: string;
    principal?: string | null;
    workspace?: string | null;
    alert_type: string;
    details: Record<string, unknown>;
}

// ============================================================
// LLM Observability Events (for agentic execution tracing)
// ============================================================
// NOTE: ObservationCaptured, ObservationFailed events have been removed.
// Agentic execution uses AgenticPageUnderstanding for observation visibility.

export interface LLMRequestSentEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id?: string;
    step_index?: number;
    capability: string;
    request_summary: string;
    input_tokens_estimate?: number;
    budget_remaining: number;
}

export interface LLMResponseReceivedEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id?: string;
    step_index?: number;
    capability: string;
    success: boolean;
    decision_summary: string;
    cost: number;
    latency_ms: number;
    error?: string;
}

// NOTE: ActionDispatched, ActionResultEvent events have been removed.
// Agentic execution uses AgenticActionExecuted for action visibility.

export interface InferenceAttemptedEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id?: string;
    parameter: string;
    inferred_value?: unknown;
    confidence: number;
    reason: string;
    accepted: boolean;
}

// NOTE: ValidationResult, ExecutabilityCheck, PageStageDetected events have been removed.
// Agentic execution uses AgenticDecisionMade for decision visibility and
// AgenticPageUnderstanding for page stage detection.

// Parameter Inference Events (Progressive Elicitation)
export interface ParameterInferenceAttemptedEvent extends V2BaseEvent {
    execution_id: string;
    parameter_name: string;
    priority: string;
}

export interface ParameterInferredEvent extends V2BaseEvent {
    execution_id: string;
    parameter_name: string;
    inferred_value: unknown;
    confidence: number;
    method: string; // "LLMBased", "RuleBased", "Historical", "Default", "AutoFill"
}

export interface ParameterInferenceFailedEvent extends V2BaseEvent {
    execution_id: string;
    parameter_name: string;
    confidence: number;
    reason: string;
}

// Execution Events (Plan Execution Lifecycle)
export interface ExecutionStartedEvent extends V2BaseEvent {
    execution_id: string;
    principal?: string | null;
    workspace?: string | null;
    plan_id: string;
    steps_total: number;
}

export interface ExecutionStepStartedEvent extends V2BaseEvent {
    execution_id: string;
    principal?: string | null;
    workspace?: string | null;
    plan_id: string;
    step_index: number;
    step_id: string;
    steps_total: number;
    providing_agent_id?: string;
}

export interface ExecutionStepCompletedEvent extends V2BaseEvent {
    execution_id: string;
    principal?: string | null;
    workspace?: string | null;
    plan_id: string;
    step_index: number;
    step_id: string;
    success: boolean;
    providing_agent_id?: string;
}

export interface ExecutionPausedEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_index: number;
    step_id: string;
    reason: string;
}

export interface ExecutionResumedEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_index: number;
    step_id?: string;
    mode: string;
}

export interface ExecutionFailedEvent extends V2BaseEvent {
    execution_id: string;
    principal?: string | null;
    workspace?: string | null;
    plan_id: string;
    step_index: number;
    step_id: string;
    error: string;
}

export interface ExecutionCompletedEvent extends V2BaseEvent {
    execution_id: string;
    principal?: string | null;
    workspace?: string | null;
    plan_id: string;
    steps_total: number;
    success: boolean;
}

export interface ExecutionRestoreFailedEvent extends V2BaseEvent {
    execution_id: string;
    principal?: string | null;
    workspace?: string | null;
    reason: string;
    note?: string;
}

// Execution status change event for real-time UI updates
export interface ExecutionStatusChangedEvent extends V2BaseEvent {
    execution_id: string;
    task_id?: string;
    root_execution_id?: string;
    previous_status: string;
    new_status: string;
    reason?: string;
}

export interface ExecutionResponsibilityChangedEvent extends V2BaseEvent {
    execution_id: string;
    parent_execution_id?: string;
    task_id?: string;
    root_execution_id?: string;
    waiting_state: string;
    active_owner_agent_id: string;
    owner_stack: string[];
    active_delegation_group: string[];
}

export interface ExecutionInflightResentEvent extends V2BaseEvent {
    execution_id: string;
    principal?: string | null;
    workspace?: string | null;
    plan_id: string;
    step_index: number;
    step_id: string;
    request_id: string;
    attempt_count: number;
}

export interface ExecutionInflightDroppedEvent extends V2BaseEvent {
    execution_id: string;
    principal?: string | null;
    workspace?: string | null;
    plan_id: string;
    step_index: number;
    step_id: string;
    request_id: string;
}

// Slot Extraction Events
export interface SlotExtractionStartedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    message_length: number;
}

export interface SlotExtractedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    slot_id: string;
    slot_type: string;
    confidence: number;
    slot_name?: string;
}

export interface SlotEnrichmentStartedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    enricher_count: number;
    total_slots: number;
    slot_name?: string;
}

export interface SlotEnrichmentCompletedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    total_slots: number;
    slots_changed: number;
    invocations: number;
    errors_count: number;
    enriched_count?: number;
    avg_confidence?: number;
}

export interface ClarifiedTaskReadyEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    slot_count?: number;
    avg_confidence?: number;
}

// Additional Slot Events
export interface SlotConfidenceUpdatedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    slot_id: string;
    old_confidence: number;
    new_confidence: number;
    source: string;
    slot_name?: string;
    enricher?: string;
}

// Clarification Events
export interface ClarificationQueuedEvent extends V2BaseEvent {
    execution_id: string;
    question_id: string;
    blocker_type: string;
    channel: string;
    stage: string;
    urgency: number;
}

export interface ClarificationResponseReceivedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    question_id?: string;
    response_text?: string;
    extracted_slots?: number;
}

// Workflow Resume Events
export interface WorkflowStageResumedEvent extends V2BaseEvent {
    execution_id: string;
    stage_name: string;
    stage_context: string;
    attempt: number;
    reused_checkpoint: boolean;
    checkpoint_hash?: string | null;
    reused_stages: string[];
}

export interface WorkflowResumeFailedEvent extends V2BaseEvent {
    execution_id: string;
    question_id?: string | null;
    error: string;
}

// Execution Cancelled Event
export interface ExecutionCancelledEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_index: number;
}

// Slot Graph Diff Event
export interface SlotGraphDiffEvent extends V2BaseEvent {
    execution_id: string;
    source: string;
    inserted: number;
    updated: number;
    removed: number;
    total_slots: number;
}

// Parameter Discovery Events
export interface ParameterDiscoveryAttemptedEvent extends V2BaseEvent {
    execution_id: string;
    parameter_name: string;
    discovery_method: string; // "WebSearch", "FilesystemSearch", "APIQuery", etc.
}

export interface ParameterDiscoveredEvent extends V2BaseEvent {
    execution_id: string;
    parameter_name: string;
    discovered_value: unknown;
    confidence: number;
    discovery_method: string;
    external_actions_performed: boolean;
}

export interface ParameterDiscoveryFailedEvent extends V2BaseEvent {
    execution_id: string;
    parameter_name: string;
    reason: string;
}

// ============================================================
// Agentic Execution Events (observe-decide-execute loop)
// ============================================================

export interface AgenticExecutionStartedEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id: string;
    goal: string;
    success_criteria: string;
    max_iterations: number;
    hint_action?: string;
}

export interface AgenticIterationStartedEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id: string;
    iteration: number;
    environment_type: string; // "browser", "filesystem", "http", "shell"
}

export interface AgenticIterationCompletedEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id: string;
    iteration: number;
    duration_ms: number;
    /** "action_executed" | "decision_rejected" | "loop_continue" | "terminal" | "paused" | "cancelled" */
    outcome: string;
}

export interface AgenticPageUnderstandingEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id: string;
    observation_id: string;
    iteration: number;
    page_stage: string;
    element_count: number;
    appears_loading: boolean;
    url?: string;
    confidence: number;
    has_screenshot: boolean;
}

export interface AgenticDecisionMadeEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id: string;
    iteration: number;
    decision_type: string; // "execute_action", "goal_achieved", "goal_failed", "needs_recovery"
    action_summary?: string;
    reasoning: string;
    confidence: number;
    thinking?: string;
    evidence?: string;
    tool_name?: string;
    action_type?: string;
    element_id?: number;
    candidates_count?: number;
    raw_decision?: any;
}

export interface AgenticActionExecutedEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id: string;
    iteration: number;
    action_type: string;
    target: string;
    success: boolean;
    latency_ms: number;
    error?: string;
}

export interface AgenticExecutionCompletedEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id: string;
    outcome: string; // "goal_achieved", "goal_failed", "max_iterations", "loop_detected", "error", "waiting_for_user", "waiting_for_confirmation", "budget_exhausted", "cannot_proceed"
    iterations_used: number;
    artifacts: string[];
    duration_ms: number;
    summary: string;
    // Loop detection details (only present when outcome = "loop_detected")
    loop_detection_type?: string; // "state_loop", "action_cycle", "no_progress"
    loop_repeated_action?: string; // The repeated action signature
    loop_recommendation?: string; // Human-readable recommendation for breaking the loop
    loop_cycle_pattern?: string[]; // For action cycles: the pattern of actions
    loop_similarity?: number; // Similarity score (for state_loop and no_progress)
    // Budget exhaustion details (only present when outcome = "budget_exhausted")
    budget_dimension?: string; // "time", "cost", "iterations", "llm_calls", "actions"
    budget_details?: string; // JSON object with used/limit
    // Cannot proceed details (only present when outcome = "cannot_proceed")
    cannot_proceed_reason?: string; // Reason why the agent cannot proceed
    // tactical pattern T1 multi-pass refinement. `refinement_pass_index = 0` (or
    // absent) is the original execution; positive values indicate a
    // refinement pass. `refinement_pending = true` on a pass-0 event
    // means the runtime will commission a refinement next — UI should
    // show "intermediate / refining" rather than "completed". When a
    // pass-N (N>0) event arrives later for the same execution_id, it
    // is authoritative and replaces the pass-0 state.
    refinement_pass_index?: number;
    refinement_pending?: boolean;
    // Structured yield payload — present when the execution terminated
    // via Decision::Yield (the unified terminal outcome-report tool).
    // Carries summary/completed/open/blockers/next_step_hint plus a
    // disposition string ('completed' / 'partial_success' / 'failed' /
    // 'retry_transient'). UI surfaces use this to render a structured
    // partial-progress card without re-parsing the partial_findings.md
    // artifact. Absent for legacy GoalReached / CannotProceed /
    // NeedUserInput terminations.
    yield_payload?: YieldPayloadSummary;
}

/** Structured yield payload as serialised by the Rust event taxonomy. */
export interface YieldPayloadSummary {
    summary: string;
    completed?: string[];
    open?: string[];
    blockers?: YieldBlockerSummary[];
    next_step_hint?: string;
    /** 'completed' | 'partial_success' | 'failed' | 'retry_transient' */
    disposition: string;
}

export interface YieldBlockerSummary {
    /** 'auth' | 'data_missing' | 'permission' | 'transient' | 'external' | 'other' */
    kind: string;
    description: string;
}

/**
 * Lifecycle marker for the agentic "waiting for user" pause. Post-H7.4
 * slim: HITL payload (question / input_type / hint / options /
 * input_schema / previous_answer / retry_reason) moved to canonical
 * `HitlRequested { source: "agentic" }`. Subscribe to that for the
 * human-response shape; this event is now only the lifecycle marker.
 */
export interface AgenticWaitingForUserEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id: string;
    iteration: number;
    pause_state_id?: string;
    // Re-asking scenario fields (lifecycle — they describe pause-state
    // evolution, not the question itself).
    is_retry?: boolean;
    retry_count?: number;
    // Agent routing fields (TRUE_AGENTS Phase 0)
    agent_id?: string;
    goal_id?: string;
    cycle_id?: string;
    // Escalation trigger (lifecycle cause).
    escalation_trigger?: string;
}

export interface AgenticResumedEvent extends V2BaseEvent {
    execution_id: string;
    pause_state_id?: string;
    plan_id: string;
    step_id: string;
    resumed_from_iteration: number;
    input_type: string;
    user_responded: boolean;
    // Agent routing fields (TRUE_AGENTS Phase 0)
    agent_id?: string;
    goal_id?: string;
    cycle_id?: string;
}

/**
 * Lifecycle marker for the agentic "waiting for confirmation" pause.
 * Post-H7.4 slim: HITL payload (action_summary / reason / action_type)
 * moved to canonical `HitlRequested { source: "agentic",
 * input_schema.action_type }`. Subscribe to that for the
 * human-response shape; this event is now only the lifecycle marker.
 */
export interface AgenticWaitingForConfirmationEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id: string;
    iteration: number;
    pause_state_id?: string;
    // Agent routing fields (TRUE_AGENTS Phase 0)
    agent_id?: string;
    goal_id?: string;
    cycle_id?: string;
}

export interface AgenticMaxIterationsReachedEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id: string;
    iterations_used: number;
    pause_state_id?: string;
    // Agent routing fields (TRUE_AGENTS Phase 0)
    agent_id?: string;
    goal_id?: string;
    cycle_id?: string;
}

// ============================================================
// Agent Lifecycle Events (TRUE_AGENTS Phase 0)
// ============================================================

export interface AgentCycleStartedEvent extends V2BaseEvent {
    principal?: string | null;
    workspace?: string | null;
    agent_id: string;
    goal_id: string;
    cycle_id: string;
    execution_id?: string;
    goal: string;
}

export interface AgentCycleCompletedEvent extends V2BaseEvent {
    principal?: string | null;
    workspace?: string | null;
    agent_id: string;
    goal_id: string;
    cycle_id: string;
    execution_id?: string;
    outcome: string;
    iterations_used: number;
}

export interface AgentTriggeredEvent extends V2BaseEvent {
    principal?: string | null;
    workspace?: string | null;
    agent_id: string;
    goal_id: string;
    trigger: string;
}

// ============================================================
// ActionBus Interaction Frames (GAUI-γ)
// ============================================================

export interface UiInteractionAckEvent extends V2BaseEvent {
    agent_id: string;
    component_id: string;
    cycle_id: string;
    status: string; // "accepted" | "queued"
}

export interface UiInteractionErrorEvent extends V2BaseEvent {
    agent_id: string;
    component_id: string;
    error: string;
    code: string; // "rate_limited" | "invalid_agent" | "agent_not_found" | "trigger_failed"
}

// Chat Mode Events
export interface ChatMessageReceivedEvent extends V2BaseEvent {
    session_id: string;
    principal?: string | null;
    workspace?: string | null;
    message: {
        id: string;
        session_id: string;
        direction: string;
        content: unknown;
        created_at: number;
    };
}

// Live Thinking Map change notice — a map's revision advanced server-side
// (owner ops / interpret / consolidation decision / ambient auto-map). This is
// a poll ACCELERATOR: it carries no map payload; consumers re-fetch the
// authoritative map via `GET /thinking-maps/{id}` on receipt.
export interface ThinkingMapUpdatedEvent extends V2BaseEvent {
    map_id: string;
    principal: string;
    workspace: string;
    revision: number;
}

// One stage of an owner-triggered map interpretation (`/interpret` narrates
// its pipeline: preparing → loading_context → facilitating → parsing →
// shaping, then a terminal idle). `utterance_id` is the run id the caller
// minted — the only thing that separates this page's run from an ambient
// auto-map narrated against the same board. `stage` stays a plain string on
// the wire: unknown stages must be tolerated (keep the current line), so the
// vocabulary can grow server-first.
export interface ThinkingMapInterpretProgressEvent extends V2BaseEvent {
    map_id: string;
    principal: string;
    workspace: string;
    utterance_id: string;
    stage: string;
    detail?: string;
    node_count?: number;
}

export interface V3PlanningStartedEvent extends V2BaseEvent {
    principal: string;
    workspace: string;
    task_id: string;
    task_title: string;
    plan_id: string;
    ui_thread_id: string;
}

export interface V3PlanningProgressEvent extends V2BaseEvent {
    principal: string;
    workspace: string;
    task_id: string;
    task_title: string;
    plan_id: string;
    ui_thread_id: string;
    phase: string;
    detail?: string;
}

export interface V3PlanningClarificationNeededEvent extends V2BaseEvent {
    principal: string;
    workspace: string;
    task_id: string;
    task_title: string;
    plan_id: string;
    ui_thread_id: string;
    questions: Array<Record<string, unknown>>;
}

export interface V3PlanningClarificationResolvedEvent extends V2BaseEvent {
    principal: string;
    workspace: string;
    task_id: string;
    task_title: string;
    plan_id: string;
    ui_thread_id: string;
    question_id: string;
    response_text: string;
}

export interface V3PlanningCompletedEvent extends V2BaseEvent {
    principal: string;
    workspace: string;
    task_id: string;
    task_title: string;
    plan_id: string;
    ui_thread_id: string;
}

export interface V3PlanningFailedEvent extends V2BaseEvent {
    principal: string;
    workspace: string;
    task_id: string;
    task_title: string;
    plan_id: string;
    ui_thread_id: string;
    error: string;
}

export interface SubGoalRequestedEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    parent_step_id: string;
    sub_goal: string;
    budget_iterations: number;
    depth: number;
}

export interface SubGoalOutcomeEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    parent_step_id: string;
    sub_goal: string;
    outcome: string;
    iterations_used: number;
    duration_ms: number;
}

export interface UserRequestPendingEvent extends V2BaseEvent {
    request: Record<string, unknown>;
}

export interface UserRequestResolvedEvent extends V2BaseEvent {
    request_id: string;
    decision: string;
    channel: string;
    principal?: string | null;
    workspace?: string | null;
    task_id?: string | null;
    request_type?: string | null;
    owner_agent_id?: string | null;
}

export interface PipelineStartedEvent extends V2BaseEvent {
    workflow_id: string;
    chain_id: string;
    max_iterations: number;
    principal?: string | null;
    workspace?: string | null;
}

export interface PipelineStepStartedEvent extends V2BaseEvent {
    workflow_id: string;
    step_id: string;
    agent_id: string;
    principal?: string | null;
    workspace?: string | null;
}

export interface PipelineStepCompletedEvent extends V2BaseEvent {
    workflow_id: string;
    step_id: string;
    agent_id: string;
    outcome_kind: string;
    principal?: string | null;
    workspace?: string | null;
}

export interface PipelineCompletedEvent extends V2BaseEvent {
    workflow_id: string;
    chain_id: string;
    steps_executed: number;
    principal?: string | null;
    workspace?: string | null;
}

export interface PipelineFailedEvent extends V2BaseEvent {
    workflow_id: string;
    chain_id: string;
    reason: string;
    steps_executed: number;
    principal?: string | null;
    workspace?: string | null;
}

export interface ParameterResolutionProgressEvent extends V2BaseEvent {
    execution_id: string;
    total_parameters: number;
    resolved_count: number;
    inferred_count: number;
    discovered_count: number;
    deferred_count: number;
    remaining_count: number;
}

export interface AgenticClickFallbackUsedEvent extends V2BaseEvent {
    execution_id: string;
    plan_id: string;
    step_id: string;
    iteration: number;
    original_selector: string;
    original_error: string;
    coordinates: [number, number];
    fallback_success: boolean;
    fallback_error?: string | null;
    latency_ms: number;
}

export interface DomChangeDetectedEvent extends V2BaseEvent {
    execution_id: string;
    correlation_id: string;
    total_changes: number;
    nodes_added: number;
    nodes_removed: number;
    signals: string[];
    initial_url?: string | null;
    final_url?: string | null;
    action_type?: string | null;
}

export interface AgentDefinitionChangedEvent extends V2BaseEvent {
    principal: string;
    workspace: string;
    agent_id: string;
}

export interface TaskCreatedEvent extends V2BaseEvent {
	principal: string;
	workspace: string;
	task_id: string;
	execution_id?: string;
	ui_thread_id?: string;
	title: string;
	created_at: number;
	updated_at: number;
}

export interface TaskUpdatedEvent extends V2BaseEvent {
	principal: string;
	workspace: string;
	task_id: string;
	execution_id?: string;
	ui_thread_id?: string;
	title: string;
	updated_at: number;
}

export interface TaskDeletedEvent extends V2BaseEvent {
	principal: string;
	workspace: string;
	task_id: string;
	execution_id?: string;
	ui_thread_id?: string;
	title: string;
	deleted_at: number;
}

export interface FeedItemCreatedEvent extends V2BaseEvent {
    item: FeedItem;
}

export interface FeedItemUpdatedEvent extends V2BaseEvent {
    principal: string;
    workspace: string;
    id: string;
    task_id?: string;
    ui_thread_id?: string;
    execution_id?: string;
    patch: FeedItemPatch;
}

export interface FeedItemRemovedEvent extends V2BaseEvent {
    principal: string;
    workspace: string;
    id: string;
    task_id?: string;
    ui_thread_id?: string;
    execution_id?: string;
}

export interface ExecutionPanelDeltaEvent extends V2BaseEvent {
    principal: string;
    workspace: string;
    task_id?: string;
    execution_id?: string;
    state: ExecutionPanelState;
}

export interface ShellOutputChunkEvent extends V2BaseEvent {
    execution_id: string;
    step_id: string;
    step_index: number;
    command: string;
    stream: string;
    data: string;
    sequence: number;
    is_final: boolean;
    exit_code?: number | null;
}

/**
 * Live PTY chunk from an `interactive_process` session. Emitted by the
 * backend reader thread whenever it pulls bytes off the child's master.
 *
 * `bytes_b64` is base64-encoded raw bytes — decode before passing to
 * xterm.js `term.write(...)`. The session is scope-isolated; subscribers
 * filter by `session_id` (the agent gets it back from `op=start`).
 *
 * See docs/plans/2026-05-13-developer-mode-workbench.md Phase 1.
 */
export interface InteractivePtyChunkEvent extends V2BaseEvent {
    session_id: string;
    principal: string;
    workspace: string;
    ui_thread_id?: string | null;
    program?: string | null;
    offset_start?: number;
    offset_end?: number;
    bytes_b64: string;
    timestamp_ms: number;
}

// GAUI-α: Agent UI event envelope for MUIJ delta delivery
export interface AgentEventEnvelope {
    event_type: string;   // "agent.ui.delta"
    agent_id: string;
    principal?: string | null;
    workspace?: string | null;
    payload: unknown;
    timestamp: number;
}

export type V2WebSocketEvent =
    | { event_type: 'MessageProcessingStarted'; data: MessageProcessingStartedEvent }
    | { event_type: 'QueryAnalysisCompleted'; data: QueryAnalysisCompletedEvent }
    | { event_type: 'StrategySelected'; data: StrategySelectedEvent }
    | { event_type: 'ExplorationProgress'; data: ExplorationProgressEvent }
    // NOTE: ToolMatching removed - superseded by ToolMatchingTierStarted/Completed
    | { event_type: 'MessageCompleted'; data: MessageCompletedEvent }
    // Execution status change for real-time UI updates
    | { event_type: 'ExecutionStatusChanged'; data: ExecutionStatusChangedEvent }
    | { event_type: 'ExecutionResponsibilityChanged'; data: ExecutionResponsibilityChangedEvent }
    // NOTE: ProcessingCancelled removed - ExecutionCancelled covers cancellation
    | { event_type: 'ProcessingError'; data: ProcessingErrorEvent }
    | { event_type: 'LLMAnalysisStarted'; data: LLMAnalysisStartedEvent }
    | { event_type: 'LLMAnalysisCompleted'; data: LLMAnalysisCompletedEvent }
    | { event_type: 'LLMAnalysisFailed'; data: LLMAnalysisFailedEvent }
    | { event_type: 'ToolMatchingTierStarted'; data: ToolMatchingTierStartedEvent }
    | { event_type: 'ToolMatchingTierCompleted'; data: ToolMatchingTierCompletedEvent }
    // NOTE: CategoryFuzzyMatching removed - superseded by ExplorationProgress
    | { event_type: 'AtomicPlanOutlineStarted'; data: AtomicPlanOutlineStartedEvent }
    | { event_type: 'AtomicPlanOutlineCompleted'; data: AtomicPlanOutlineCompletedEvent }
    | { event_type: 'AtomicPlanExpansionStarted'; data: AtomicPlanExpansionStartedEvent }
    | { event_type: 'AtomicPlanGenerated'; data: AtomicPlanGeneratedEvent }
    | { event_type: 'ClarificationSessionSnapshot'; data: ClarificationSessionSnapshotEvent }
    | { event_type: 'ClarificationConfidenceSnapshot'; data: ClarificationConfidenceSnapshotEvent }
    | { event_type: 'ClarificationMetricsSnapshot'; data: ClarificationMetricsSnapshotEvent }
    | { event_type: 'WorkflowResumed'; data: WorkflowResumedEvent }
    | { event_type: 'ObservabilityAlert'; data: ObservabilityAlertEvent }
    | { event_type: 'Heartbeat'; data: HeartbeatEvent }
    | { event_type: 'PipelineStarted'; data: PipelineStartedEvent }
    | { event_type: 'PipelineStepStarted'; data: PipelineStepStartedEvent }
    | { event_type: 'PipelineStepCompleted'; data: PipelineStepCompletedEvent }
    | { event_type: 'PipelineCompleted'; data: PipelineCompletedEvent }
    | { event_type: 'PipelineFailed'; data: PipelineFailedEvent }
    | { event_type: 'ExecutionStarted'; data: ExecutionStartedEvent }
    | { event_type: 'ExecutionStepStarted'; data: ExecutionStepStartedEvent }
    | { event_type: 'ExecutionStepCompleted'; data: ExecutionStepCompletedEvent }
    | { event_type: 'ExecutionPaused'; data: ExecutionPausedEvent }
    | { event_type: 'ExecutionResumed'; data: ExecutionResumedEvent }
    | { event_type: 'ExecutionFailed'; data: ExecutionFailedEvent }
    | { event_type: 'ExecutionCompleted'; data: ExecutionCompletedEvent }
    | { event_type: 'ExecutionRestoreFailed'; data: ExecutionRestoreFailedEvent }
    | { event_type: 'ExecutionInflightResent'; data: ExecutionInflightResentEvent }
    | { event_type: 'ExecutionInflightDropped'; data: ExecutionInflightDroppedEvent }
    // Execution Observability Events
    // NOTE: ObservationCaptured, ObservationFailed removed - use AgenticPageUnderstanding
    // NOTE: ActionDispatched, ActionResultEvent removed - use AgenticActionExecuted
    // NOTE: ValidationResult, ExecutabilityCheck, PageStageDetected removed - use AgenticDecisionMade
    | { event_type: 'LLMRequestSent'; data: LLMRequestSentEvent }
    | { event_type: 'LLMResponseReceived'; data: LLMResponseReceivedEvent }
    | { event_type: 'InferenceAttempted'; data: InferenceAttemptedEvent }
    // Parameter Inference Events
    | { event_type: 'ParameterInferenceAttempted'; data: ParameterInferenceAttemptedEvent }
    | { event_type: 'ParameterInferred'; data: ParameterInferredEvent }
    | { event_type: 'ParameterInferenceFailed'; data: ParameterInferenceFailedEvent }
    | { event_type: 'ParameterResolutionProgress'; data: ParameterResolutionProgressEvent }
    | { event_type: 'SlotExtractionStarted'; data: SlotExtractionStartedEvent }
    | { event_type: 'SlotExtracted'; data: SlotExtractedEvent }
    | { event_type: 'SlotEnrichmentStarted'; data: SlotEnrichmentStartedEvent }
    | { event_type: 'SlotEnrichmentCompleted'; data: SlotEnrichmentCompletedEvent }
    | { event_type: 'ClarifiedTaskReady'; data: ClarifiedTaskReadyEvent }
    | { event_type: 'SlotConfidenceUpdated'; data: SlotConfidenceUpdatedEvent }
    | { event_type: 'ClarificationQueued'; data: ClarificationQueuedEvent }
    | { event_type: 'ClarificationResponseReceived'; data: ClarificationResponseReceivedEvent }
    | { event_type: 'WorkflowStageResumed'; data: WorkflowStageResumedEvent }
    | { event_type: 'WorkflowResumeFailed'; data: WorkflowResumeFailedEvent }
    // Execution Cancelled
    | { event_type: 'ExecutionCancelled'; data: ExecutionCancelledEvent }
    // Slot Graph Diff
    | { event_type: 'SlotGraphDiff'; data: SlotGraphDiffEvent }
    // Parameter Discovery Events
    | { event_type: 'ParameterDiscoveryAttempted'; data: ParameterDiscoveryAttemptedEvent }
    | { event_type: 'ParameterDiscovered'; data: ParameterDiscoveredEvent }
    | { event_type: 'ParameterDiscoveryFailed'; data: ParameterDiscoveryFailedEvent }
    // Agentic Execution Events
    | { event_type: 'AgenticExecutionStarted'; data: AgenticExecutionStartedEvent }
    | { event_type: 'AgenticIterationStarted'; data: AgenticIterationStartedEvent }
    | { event_type: 'AgenticIterationCompleted'; data: AgenticIterationCompletedEvent }
    | { event_type: 'AgenticPageUnderstanding'; data: AgenticPageUnderstandingEvent }
    | { event_type: 'AgenticDecisionMade'; data: AgenticDecisionMadeEvent }
    | { event_type: 'AgenticActionExecuted'; data: AgenticActionExecutedEvent }
    | { event_type: 'AgenticClickFallbackUsed'; data: AgenticClickFallbackUsedEvent }
    | { event_type: 'AgenticExecutionCompleted'; data: AgenticExecutionCompletedEvent }
    | { event_type: 'AgenticWaitingForUser'; data: AgenticWaitingForUserEvent }
    | { event_type: 'AgenticWaitingForConfirmation'; data: AgenticWaitingForConfirmationEvent }
    | { event_type: 'AgenticResumed'; data: AgenticResumedEvent }
    | { event_type: 'AgenticMaxIterationsReached'; data: AgenticMaxIterationsReachedEvent }
    // Agent Lifecycle Events (TRUE_AGENTS Phase 0)
    | { event_type: 'AgentCycleStarted'; data: AgentCycleStartedEvent }
    | { event_type: 'AgentCycleCompleted'; data: AgentCycleCompletedEvent }
    | { event_type: 'AgentTriggered'; data: AgentTriggeredEvent }
    | { event_type: 'UiInteractionAck'; data: UiInteractionAckEvent }
    | { event_type: 'UiInteractionError'; data: UiInteractionErrorEvent }
    // GAUI-α: Agent UI events (MUIJ deltas)
    | { event_type: 'AgentEvent'; data: { event: AgentEventEnvelope } }
    // Chat Mode Events
    | { event_type: 'ChatMessageReceived'; data: ChatMessageReceivedEvent }
    | { event_type: 'ThinkingMapUpdated'; data: ThinkingMapUpdatedEvent }
    | { event_type: 'ThinkingMapInterpretProgress'; data: ThinkingMapInterpretProgressEvent }
    | { event_type: 'V3PlanningStarted'; data: V3PlanningStartedEvent }
    | { event_type: 'V3PlanningProgress'; data: V3PlanningProgressEvent }
    | { event_type: 'V3PlanningClarificationNeeded'; data: V3PlanningClarificationNeededEvent }
    | { event_type: 'V3PlanningClarificationResolved'; data: V3PlanningClarificationResolvedEvent }
    | { event_type: 'V3PlanningCompleted'; data: V3PlanningCompletedEvent }
    | { event_type: 'V3PlanningFailed'; data: V3PlanningFailedEvent }
    | { event_type: 'SubGoalRequested'; data: SubGoalRequestedEvent }
    | { event_type: 'SubGoalOutcome'; data: SubGoalOutcomeEvent }
    | { event_type: 'UserRequestPending'; data: UserRequestPendingEvent }
    | { event_type: 'UserRequestResolved'; data: UserRequestResolvedEvent }
    | { event_type: 'DomChangeDetected'; data: DomChangeDetectedEvent }
    | { event_type: 'AgentDefinitionChanged'; data: AgentDefinitionChangedEvent }
    | { event_type: 'TaskCreated'; data: TaskCreatedEvent }
    | { event_type: 'TaskUpdated'; data: TaskUpdatedEvent }
    | { event_type: 'TaskDeleted'; data: TaskDeletedEvent }
    | { event_type: 'FeedItemCreated'; data: FeedItemCreatedEvent }
    | { event_type: 'FeedItemUpdated'; data: FeedItemUpdatedEvent }
    | { event_type: 'FeedItemRemoved'; data: FeedItemRemovedEvent }
    | { event_type: 'ExecutionPanelDelta'; data: ExecutionPanelDeltaEvent }
    // Shell Output Streaming
    | { event_type: 'ShellOutputChunk'; data: ShellOutputChunkEvent }
    // Developer Mode — live PTY bytes for the xterm.js pane
    | { event_type: 'InteractivePtyChunk'; data: InteractivePtyChunkEvent };

export interface V2EventStore {
    subscribe: (callback: (events: V2WebSocketEvent[]) => void) => () => void;
    connect: (executionId?: string, agentId?: string) => void;
    disconnect: () => void;
    isConnected: boolean;
    clear: () => void;
    send: (message: Record<string, unknown>) => boolean;
}

// Connection status for health indicator
export type ConnectionStatus = 'connected' | 'connecting' | 'disconnected';

export interface ConnectionStatusStore {
    subscribe: (callback: (status: ConnectionStatus) => void) => () => void;
}

const GENERIC_CYCLE_STARTED_EVENT_TYPES = new Set(['agent.cycle.started']);
const GENERIC_CYCLE_COMPLETED_EVENT_TYPES = new Set([
    'agent.cycle.completed',
    'agent.cycle.failed',
    'agent.cycle.paused'
]);

// R623: Known top-level event_type values from V2WebSocketEvent union.
// DEV-mode warning fires for event types not in this set.
const KNOWN_V2_EVENT_TYPES: ReadonlySet<string> = new Set([
    // Every event the shared taxonomy declares is known by construction.
    // This list used to be maintained by hand beside the taxonomy, and the two
    // drifted: the whole Activity family plus HitlRequested/HitlResolved,
    // ProgressEvent and ThinkingMode* were declared, routed and rendered, yet
    // still warned as a protocol mismatch on every message. Sourcing them here
    // means a new taxonomy row can never reappear as a false mismatch.
    ...KNOWN_EVENT_TYPES,
    // Below: event types with no taxonomy row of their own — client-side and
    // V3 planning events. These still need listing by hand.
    'MessageProcessingStarted', 'QueryAnalysisCompleted', 'StrategySelected',
    'ExplorationProgress', 'MessageCompleted', 'ExecutionStatusChanged',
    'ExecutionResponsibilityChanged',
    'ProcessingError', 'LLMAnalysisStarted', 'LLMAnalysisCompleted',
    'LLMAnalysisFailed', 'ToolMatchingTierStarted', 'ToolMatchingTierCompleted',
    'AtomicPlanOutlineStarted', 'AtomicPlanOutlineCompleted',
    'AtomicPlanExpansionStarted', 'AtomicPlanGenerated',
    'ClarificationSessionSnapshot', 'ClarificationConfidenceSnapshot',
    'ClarificationMetricsSnapshot', 'WorkflowResumed', 'ObservabilityAlert',
    'Heartbeat', 'PipelineStarted', 'PipelineStepStarted', 'PipelineStepCompleted',
    'PipelineCompleted',
    'PipelineFailed', 'ExecutionStarted', 'ExecutionStepStarted',
    'ExecutionStepCompleted', 'ExecutionPaused', 'ExecutionResumed',
    'ExecutionFailed', 'ExecutionCompleted', 'ExecutionRestoreFailed',
    'ExecutionInflightResent', 'ExecutionInflightDropped',
    'LLMRequestSent', 'LLMResponseReceived', 'InferenceAttempted',
    'ParameterInferenceAttempted', 'ParameterInferred', 'ParameterInferenceFailed',
    'ParameterResolutionProgress',
    'SlotExtractionStarted', 'SlotExtracted', 'SlotEnrichmentStarted',
    'SlotEnrichmentCompleted', 'ClarifiedTaskReady', 'SlotConfidenceUpdated',
    'ClarificationQueued', 'ClarificationResponseReceived',
    'WorkflowStageResumed', 'WorkflowResumeFailed', 'ExecutionCancelled',
    'SlotGraphDiff', 'ParameterDiscoveryAttempted', 'ParameterDiscovered',
    'ParameterDiscoveryFailed', 'AgenticExecutionStarted',
    'AgenticIterationStarted', 'AgenticIterationCompleted',
    'AgenticStepStarted', 'AgenticStepCompleted', 'AgenticStepFailed',
    'AgenticStepStuckWarning', 'AgenticPageUnderstanding',
    'AgenticDecisionMade', 'AgenticActionExecuted', 'AgenticClickFallbackUsed',
    'AgenticExecutionCompleted', 'AgenticWaitingForUser',
    'AgenticWaitingForConfirmation', 'AgenticResumed',
    'AgenticMaxIterationsReached', 'AgentCycleStarted',
    'AgentCycleCompleted', 'AgentTriggered', 'UiInteractionAck',
    'UiInteractionError', 'AgentEvent', 'ChatMessageReceived', 'ThinkingMapUpdated',
    'ThinkingMapInterpretProgress',
    'V3PlanningStarted', 'V3PlanningProgress',
    'V3PlanningClarificationNeeded', 'V3PlanningClarificationResolved',
    'V3PlanningCompleted', 'V3PlanningFailed', 'SubGoalRequested', 'SubGoalOutcome',
    'UserRequestPending', 'UserRequestResolved', 'DomChangeDetected',
    'AgentDefinitionChanged', 'TaskCreated', 'TaskUpdated', 'TaskDeleted',
    'FeedItemCreated', 'FeedItemUpdated', 'FeedItemRemoved', 'ExecutionPanelDelta',
    'ShellOutputChunk',
    'InteractivePtyChunk',
]);

function asRecord(value: unknown): Record<string, unknown> | null {
    return typeof value === 'object' && value !== null
        ? (value as Record<string, unknown>)
        : null;
}

function isSnapshotDocument(value: unknown): boolean {
    const rec = asRecord(value);
    // R685: Also check muij_version to match muijStore's isValidSnapshotDocument.
    // Without this, a frame like { layout: [] } (no version) passes here but fails
    // in handleMuijSnapshot, prematurely clearing delta tracking.
    return !!rec && Array.isArray(rec.layout) && typeof rec.muij_version === 'string';
}

function extractAgentEnvelope(data: unknown): AgentEventEnvelope | null {
    const container = asRecord(data);
    if (!container) return null;

    const maybeEnvelope = asRecord(container.event);
    if (!maybeEnvelope) return null;

    const eventType = maybeEnvelope.event_type;
    const agentId = maybeEnvelope.agent_id;
    const timestamp = maybeEnvelope.timestamp;
    if (typeof eventType !== 'string' || eventType.length === 0) return null;
    if (typeof agentId !== 'string' || agentId.length === 0) return null;

    return {
        event_type: eventType,
        agent_id: agentId,
        principal: typeof maybeEnvelope.principal === 'string' ? maybeEnvelope.principal : undefined,
        workspace: typeof maybeEnvelope.workspace === 'string' ? maybeEnvelope.workspace : undefined,
        payload: maybeEnvelope.payload ?? {},
        timestamp: typeof timestamp === 'number' && Number.isFinite(timestamp) ? timestamp : Date.now()
    };
}

const EVENT_SEQUENCE_BY_REF = new WeakMap<object, number>();
let nextEventSequence = 1;

function assignEventSequence(event: V2WebSocketEvent): V2WebSocketEvent {
    EVENT_SEQUENCE_BY_REF.set(event as object, nextEventSequence++);
    // R705: Wrap to prevent exceeding MAX_SAFE_INTEGER (theoretical, ~9e15 events)
    if (nextEventSequence > Number.MAX_SAFE_INTEGER - 1) nextEventSequence = 1;
    return event;
}

export function getV2EventSequence(event: V2WebSocketEvent): number {
    return EVENT_SEQUENCE_BY_REF.get(event as object) ?? 0;
}

class V2WebSocketManager implements V2EventStore {
    private events = writable<V2WebSocketEvent[]>([]);
    private ws: WebSocket | null = null;
    private executionId: string | null = null;
    private agentId: string | null = null;
    public isConnected = false;
    private scopeKey = '';

    // Connection status store for reactive UI updates (health indicator)
    private _connectionStatus = writable<ConnectionStatus>('disconnected');
    public connectionStatus: ConnectionStatusStore = {
        subscribe: this._connectionStatus.subscribe
    };

    // Reconnection state
    private reconnectAttempts = 0;
    private maxReconnectAttempts = 10;
    private reconnectTimeout: ReturnType<typeof setTimeout> | null = null;
    private shouldReconnect = true;
    private pingInterval: ReturnType<typeof setInterval> | null = null;

    constructor() {
        this.scopeKey = this.currentScopeKey();
        if (typeof window === 'undefined') {
            return;
        }
        scopeIdentityStore.subscribe((scope) => {
            const nextScopeKey = `${scope.principal.trim()}:${scope.workspace.trim()}`;
            if (nextScopeKey === this.scopeKey) {
                return;
            }
            this.scopeKey = nextScopeKey;
            if (!this.hasActiveScopeBoundConnection()) {
                return;
            }
            this.clear();
            this.reconnect();
        });
    }

    subscribe(callback: (events: V2WebSocketEvent[]) => void) {
        return this.events.subscribe(callback);
    }

    private currentScopeKey(): string {
        const scope = get(scopeIdentityStore);
        return `${scope.principal.trim()}:${scope.workspace.trim()}`;
    }

    private hasActiveScopeBoundConnection(): boolean {
        return this.shouldReconnect && (this.ws !== null || this.reconnectTimeout !== null || this.isConnected);
    }

    connect(executionId?: string, agentId?: string) {
        const nextExecutionId = executionId || null;
        const nextAgentId = agentId || null;
        const sameTarget = this.executionId === nextExecutionId && this.agentId === nextAgentId;
        const socketReadyState = this.ws?.readyState;
        const socketIsActive = socketReadyState === WebSocket.OPEN || socketReadyState === WebSocket.CONNECTING;

        // Idempotent reconnect guard: if the requested target is already active
        // (or reconnect has already been scheduled), do not tear down the socket.
        if (sameTarget && this.shouldReconnect && (socketIsActive || this.reconnectTimeout !== null)) {
            return;
        }

        // Close existing connection if any
        this.disconnect();

        this.executionId = nextExecutionId;
        this.agentId = nextAgentId;
        this.shouldReconnect = true;
        this.reconnectAttempts = 0;

        this.establishConnection();
    }

    /**
     * Connect globally without an execution filter.
     * Receives all WebSocket events for real-time task list updates.
     */
    connectGlobal() {
        this.connect();
    }

    private establishConnection() {
        if (typeof window !== 'undefined' && (window as any).__MAGICIAN_MISSING__) {
            return;
        }

        // LEAK GUARD: this is the ONLY place a socket is created, so it must
        // never orphan a previous one. The onerror→scheduleReconnect path can
        // arrive here while the old socket is still CONNECTING/OPEN (onerror
        // without onclose — see R559): without this teardown the old socket
        // survives with its handlers attached, its eventual onclose schedules
        // ANOTHER reconnect, and connections multiply against a flapping
        // backend (observed as hundreds of server-side ws connections).
        if (this.ws) {
            this.ws.onopen = null;
            this.ws.onmessage = null;
            this.ws.onerror = null;
            this.ws.onclose = null;
            try {
                if (this.ws.readyState !== WebSocket.CLOSED) {
                    this.ws.close(4002, 'Superseded by new connection');
                }
            } catch (error) {
                console.warn('[V2WS] stale socket close failed', error);
            }
            this.ws = null;
        }

        // Determine protocol (ws or wss based on current page protocol)
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const host = window.location.hostname;
        const port = window.location.port || (protocol === 'wss:' ? '443' : '80');

        // V2 WebSocket endpoint with optional execution_id/agent_id for reconnect support.
        // execution_id re-emits pending WaitingForUser events for execution-scoped pauses.
        // agent_id re-emits pending pauses for agent-scoped execution (TRUE_AGENTS Phase 0).
        const params = new URLSearchParams();
        if (this.executionId) params.set('execution_id', this.executionId);
        if (this.agentId) params.set('agent_id', this.agentId);
        params.set('supports_structured_presentation', 'true');
        const queryString = params.toString() ? `?${params.toString()}` : '';
        // In dev mode, connect directly to the backend (port 3002) because
        // SvelteKit's dev server intercepts WebSocket upgrades before Vite's
        // proxy can forward them.  In production, use the same origin.
        const backendPort = import.meta.env.DEV ? '3002' : port;
        const wsUrl = `${protocol}//${host}:${backendPort}/api/magician/v2/realtime/ws${queryString}`;

        // R587: Guard connection lifecycle logs for production
        if (import.meta.env.DEV) console.log(`🔌 V2 WebSocket connecting to: ${wsUrl} (execution: ${this.executionId || 'global'}, agent: ${this.agentId || 'none'})`);
        this._connectionStatus.set('connecting');

        try {
            // The bearer travels as an auth-only protocol because browsers cannot
            // set Authorization on a WebSocket upgrade. A stable application
            // protocol must be offered as well: the server selects this value and
            // never echoes the bearer back in Sec-WebSocket-Protocol.
            this.ws = new WebSocket(
                scopedMagicianWebSocketUrl(`/api/magician/v2/realtime/ws${queryString}`, `${protocol}//${host}:${backendPort}`),
                scopedWebSocketProtocols([MAGICIAN_REALTIME_WEBSOCKET_PROTOCOL])
            );

            this.ws.onopen = () => {
                if (import.meta.env.DEV) console.log('✅ V2 WebSocket connection established');
                this.isConnected = true;
                this._connectionStatus.set('connected');
                this.reconnectAttempts = 0;

                // Start ping interval to keep connection alive
                this.startPingInterval();

                // Request MUIJ snapshots for tracked agents on reconnect
                requestMuijSnapshots(this);
            };

            this.ws.onmessage = (event) => {
                try {
                    const message = JSON.parse(event.data);
                    // R721: Guard production console noise
                    if (import.meta.env.DEV) console.log('📡 V2 WebSocket message received:', message);

                    // Handle MUIJ snapshot responses (direct server→client, not broadcast)
                    if (message.type === 'agent.ui.snapshot') {
                        const agentId = typeof message.agent_id === 'string' ? message.agent_id : '';
                        if (agentId && isSnapshotDocument(message.document)) {
                            handleMuijSnapshot(agentId, message.document);
                        } else {
                            if (agentId) {
                                handleMuijSnapshotError(agentId);
                            }
                            console.warn('[MUIJ] invalid snapshot payload:', message);
                        }
                        return;
                    }
                    if (message.type === 'agent.ui.snapshot_error') {
                        if (typeof message.agent_id === 'string') {
                            handleMuijSnapshotError(message.agent_id);
                        }
                        console.warn('[MUIJ] snapshot error:', message.agent_id, message.error);
                        return;
                    }
                    if (message.type === 'ui.interaction.ack') {
                        if (
                            typeof message.agent_id !== 'string'
                            || typeof message.component_id !== 'string'
                            || typeof message.cycle_id !== 'string'
                            || typeof message.status !== 'string'
                        ) {
                            console.warn('[V2WS] invalid ui.interaction.ack payload:', message);
                            return;
                        }
                        this.handleEvent({
                            event_type: 'UiInteractionAck',
                            data: {
                                agent_id: message.agent_id,
                                component_id: message.component_id,
                                cycle_id: message.cycle_id,
                                status: message.status,
                                timestamp: Date.now()
                            }
                        });
                        return;
                    }
                    if (message.type === 'ui.interaction.error') {
                        if (
                            typeof message.agent_id !== 'string'
                            || typeof message.component_id !== 'string'
                            || typeof message.error !== 'string'
                            || typeof message.code !== 'string'
                        ) {
                            console.warn('[V2WS] invalid ui.interaction.error payload:', message);
                            return;
                        }
                        this.handleEvent({
                            event_type: 'UiInteractionError',
                            data: {
                                agent_id: message.agent_id,
                                component_id: message.component_id,
                                error: message.error,
                                code: message.code,
                                timestamp: Date.now()
                            }
                        });
                        return;
                    }

                    // R700: Validate message shape before routing — reject non-conforming frames
                    // that would enter the events store as invalid entries.
                    if (typeof message.event_type !== 'string' || message.data == null) {
                        if (import.meta.env.DEV) {
                            console.warn('[V2WS] dropped frame without event_type/data:', message);
                        }
                        return;
                    }

                    // Handle the event
                    this.handleEvent(message);
                } catch (error) {
                    console.error('Failed to parse V2 WebSocket message:', error);
                }
            };

            this.ws.onerror = (error) => {
                console.error('❌ V2 WebSocket error:', error);
                this.isConnected = false;
                this._connectionStatus.set('disconnected');
                // R559: Some network failures fire onerror without onclose.
                // Schedule reconnect here as a safety net; onclose's guard
                // (reconnectTimeout !== null) prevents double-scheduling.
                if (this.shouldReconnect && this.reconnectAttempts < this.maxReconnectAttempts && !this.reconnectTimeout) {
                    this.scheduleReconnect();
                }
            };

            this.ws.onclose = (event) => {
                if (import.meta.env.DEV) console.log(`🔌 V2 WebSocket closed: ${event.code} - ${event.reason}`);
                this.isConnected = false;
                this._connectionStatus.set('disconnected');
                this.stopPingInterval();

                // Attempt to reconnect if we should
                if (this.shouldReconnect && this.reconnectAttempts < this.maxReconnectAttempts) {
                    this.scheduleReconnect();
                }
            };
        } catch (error) {
            console.error('Failed to create V2 WebSocket:', error);
            this._connectionStatus.set('disconnected');
            this.scheduleReconnect();
        }
    }

    private scheduleReconnect() {
        if (this.reconnectTimeout) {
            clearTimeout(this.reconnectTimeout);
        }

        this.reconnectAttempts++;

        // Exponential backoff: 1s, 2s, 4s, 8s, 16s, 32s (max)
        const delay = Math.min(1000 * Math.pow(2, this.reconnectAttempts - 1), 32000);

        if (import.meta.env.DEV) console.log(`🔄 V2 reconnecting in ${delay}ms (attempt ${this.reconnectAttempts}/${this.maxReconnectAttempts})`);

        // Show connecting status while waiting to reconnect
        this._connectionStatus.set('connecting');

        this.reconnectTimeout = setTimeout(() => {
            // R524: Clear handle so manual reconnect is not blocked by stale non-null ref.
            this.reconnectTimeout = null;
            // R525: Re-check shouldReconnect — disconnect() may have been called during delay.
            if (!this.shouldReconnect) return;
            this.establishConnection();
        }, delay);
    }

    private reconnect() {
        if (!this.shouldReconnect) return;
        if (this.reconnectAttempts >= this.maxReconnectAttempts) return;

        this.stopPingInterval();
        this.isConnected = false;
        this._connectionStatus.set('disconnected');

        if (this.ws) {
            // Prevent duplicate scheduleReconnect calls from onclose when we force-close here.
            this.ws.onopen = null;
            this.ws.onmessage = null;
            this.ws.onerror = null;
            this.ws.onclose = null;

            try {
                if (this.ws.readyState === WebSocket.OPEN || this.ws.readyState === WebSocket.CONNECTING) {
                    this.ws.close(4001, 'Client reconnect');
                }
            } catch (error) {
                console.warn('[V2WS] reconnect close failed', error);
            }

            this.ws = null;
        }

        this.scheduleReconnect();
    }

    private startPingInterval() {
        // R339: Check connection health every 30s.
        // Browser WebSocket API doesn't expose a ping() method;
        // the backend sends protocol-level pings and the browser auto-pongs.
        // This interval detects zombie sockets (readyState stuck on OPEN
        // despite network loss) and triggers reconnect.
        this.pingInterval = setInterval(() => {
            if (this.ws && this.ws.readyState !== WebSocket.OPEN) {
                console.warn('[V2WS] Heartbeat detected non-OPEN socket, triggering reconnect');
                this.reconnect();
                return;
            }
            // Lightweight app-level ping. The browser already auto-pongs the
            // server's protocol-level pings, so this is defense-in-depth: it
            // sends inbound client traffic the server can use to refresh the
            // session's last_heartbeat and avoid tearing down an otherwise-live
            // socket on the CLIENT_TIMEOUT edge. NOTE: the primary fix is
            // server-side — websocket_handler.rs must refresh last_heartbeat on
            // any inbound Text (or a dedicated {type:'ping'}) and relax
            // CLIENT_TIMEOUT to ~45-60s. See cross_file_requests.
            this.send({ type: 'ping', ts: Date.now() });
        }, 30000);
    }

    private stopPingInterval() {
        if (this.pingInterval) {
            clearInterval(this.pingInterval);
            this.pingInterval = null;
        }
    }

    disconnect() {
        if (import.meta.env.DEV) console.log('🔌 Disconnecting V2 WebSocket');

        this.shouldReconnect = false;

        if (this.reconnectTimeout) {
            clearTimeout(this.reconnectTimeout);
            this.reconnectTimeout = null;
        }

        this.stopPingInterval();

        if (this.ws) {
            // R638: Null handlers BEFORE close() to prevent ghost messages.
            // WebSocket.close() is async — buffered messages can still fire
            // onmessage after disconnect() returns, processing against reset state.
            // Compare with reconnect() which already does this correctly.
            this.ws.onopen = null;
            this.ws.onmessage = null;
            this.ws.onerror = null;
            this.ws.onclose = null;
            // R630: Only call close() on non-CLOSED sockets to avoid InvalidStateError
            if (this.ws.readyState !== WebSocket.CLOSED) {
                this.ws.close(1000, 'Client disconnecting');
            }
            this.ws = null;
        }

        this.isConnected = false;
        this._connectionStatus.set('disconnected');
        this.executionId = null;
        // R336: Reset agentId to prevent stale ID on subsequent connect()
        this.agentId = null;
        this.reconnectAttempts = 0;
        // R322: Clear MUIJ tracking state to prevent stale entries across navigations
        muijOnDisconnect();
    }

    private handleEvent(event: V2WebSocketEvent) {
        const eventWithSequence = assignEventSequence(event);

        // Add event to store
        this.events.update(events => {
            // Keep only last 100 events to prevent memory issues
            const newEvents = [...events, eventWithSequence];
            return newEvents.slice(-100);
        });

        // R721: Guard production console noise
        if (import.meta.env.DEV) console.log(`📡 V2 Event: ${eventWithSequence.event_type}`, eventWithSequence.data);

        // Route ChatMessageReceived to chatStore
        // The store's handleChatMessageReceived calls convertMessage internally,
        // so we pass the raw message without inline normalization.
        if (eventWithSequence.event_type === 'ChatMessageReceived') {
            const data = eventWithSequence.data as ChatMessageReceivedEvent;
            if (data.session_id && data.message) {
                chatStore.handleChatMessageReceived(
                    data.session_id,
                    data.message as unknown as Record<string, unknown>
                );

                // When an escalation_resolved message arrives, also mark any
                // matching unresolved escalation messages for that execution as
                // resolved (disables buttons in the UI).
                const msgContent = data.message.content as Record<string, unknown> | undefined;
                if (msgContent && (msgContent as { type?: string }).type === 'escalation_resolved') {
                    const executionId = (msgContent as { execution_id?: string }).execution_id;
                    const requestId = (msgContent as { request_id?: string }).request_id;
                    const pauseStateId = (msgContent as { pause_state_id?: string }).pause_state_id;
                    const summary = (msgContent as { summary?: string }).summary;
                    if (executionId || requestId || pauseStateId) {
                        chatStore.markEscalationResolved(executionId || '', requestId, pauseStateId, summary);
                    }
                }
            }
        }

        if (eventWithSequence.event_type === 'UserRequestResolved') {
            const data = eventWithSequence.data as UserRequestResolvedEvent;
            if (data.request_id) {
                chatStore.markEscalationResolved('', data.request_id);
            }
        }

        // Phase H5.3 — route canonical HITL typed events into the
        // approval store (filtered by `source: "approval"` inside the
        // handler). Pairs with `handleApprovalEnvelopeEvent` below
        // which still consumes the legacy `approval.*` AGUI envelope
        // events; dedup happens via `correlation_id == approval_id`
        // in the approvalMap.
        //
        // The `V2WebSocketEvent` discriminated union doesn't model
        // canonical HITL variants yet (they live on the v3 SSE
        // stream as the primary path), so we read `event_type` and
        // `data` through unknown casts rather than discriminate on
        // the union. When the v2 WS type definitions catch up,
        // collapse to a typed check.
        const canonicalEventType = (eventWithSequence as { event_type: string }).event_type;
        if (
            canonicalEventType === 'HitlRequested'
            || canonicalEventType === 'HitlResolved'
        ) {
            const data = (eventWithSequence as { data?: unknown }).data;
            if (data && typeof data === 'object') {
                handleCanonicalHitlApprovalEvent(
                    canonicalEventType,
                    data as Record<string, unknown>
                );
            }
        }

        // Route agent lifecycle events to agentStore
        if (
            eventWithSequence.event_type === 'AgentCycleStarted' ||
            eventWithSequence.event_type === 'AgentCycleCompleted' ||
            eventWithSequence.event_type === 'AgentTriggered'
        ) {
            handleAgentEvent(eventWithSequence);
        }

        if (eventWithSequence.event_type === 'AgentDefinitionChanged') {
            const data = eventWithSequence.data as AgentDefinitionChangedEvent;
            if (typeof data.agent_id === 'string' && data.agent_id.trim().length > 0) {
                void reconcileAgentDefinitionChange(data.agent_id);
            }
        }

        // New cycle starting — re-enable delta acceptance for this agent
        // (must run before MUIJ delta routing so same-event deltas are accepted)
        // R631: Track whether typed path already handled cycle start/complete
        // to prevent duplicate onAgentCycleStarted / clearAgentMuij from envelope path.
        let typedCycleStartHandled = false;
        let typedCycleCompleteHandled = false;
        if (eventWithSequence.event_type === 'AgentCycleStarted') {
            const data = eventWithSequence.data as AgentCycleStartedEvent;
            onAgentCycleStarted(data.agent_id, data.cycle_id);
            typedCycleStartHandled = true;
        }

        // R627: Clear MUIJ state on typed AgentCycleCompleted path.
        // Previously only the AgentEvent envelope path called clearAgentMuij.
        // If the typed event fires without/before the envelope, stale components persisted.
        // clearAgentMuij is idempotent (R122), so double-calls from both paths are safe.
        if (eventWithSequence.event_type === 'AgentCycleCompleted') {
            const data = eventWithSequence.data as AgentCycleCompletedEvent;
            clearAgentMuij(data.agent_id, data.cycle_id);
            typedCycleCompleteHandled = true;
        }

        // Route MUIJ delta events
        if (eventWithSequence.event_type === 'AgentEvent') {
            const envelope = extractAgentEnvelope(eventWithSequence.data);
            if (!envelope) {
                console.warn('Invalid AgentEvent envelope received; dropping event', eventWithSequence.data);
                return;
            }

            // Route generic agent events (cycle + approval + lifecycle) into store.
            handleAgentEnvelopeEvent(envelope);
            handleApprovalEnvelopeEvent(envelope);
            // Route operator-feed agent.update events to the feed store.
            handleAgentUpdateEnvelopeEvent(envelope);
            // Route first-class agentic stream events (plan.* / tool.call.* /
            // reasoning.*) into the agentic stream store. No-op for envelope
            // types this store doesn't recognize.
            handleAgenticStreamEvent(envelope);
            handleMediaPreferencesUpdatedEnvelope(envelope);
            handleMediaConfigUpdatedEnvelope(envelope);
            if (envelope.event_type === 'media.config.updated') {
                void mediaProvidersStore.refresh();
            }
            handleUiPreferencesUpdatedEnvelope(envelope);
            handleChatEngineUpdatedEnvelope(envelope);

            // Generic cycle-start events should re-enable MUIJ delta acceptance.
            // R631: Skip when typed AgentCycleStarted already handled this —
            // prevents double muijMap.update() and spurious re-render.
            if (GENERIC_CYCLE_STARTED_EVENT_TYPES.has(envelope.event_type) && !typedCycleStartHandled) {
                const payload = asRecord(envelope.payload);
                const cycleId = payload && typeof payload.cycle_id === 'string'
                    ? payload.cycle_id
                    : undefined;
                onAgentCycleStarted(envelope.agent_id, cycleId);
            }

            // Route MUIJ deltas (`agent.ui.delta`) through the same envelope.
            handleMuijEvent(envelope);

            // Generic cycle terminal events clear cycle-scoped MUIJ state.
            // R627/R631: Skip when typed AgentCycleCompleted already handled this.
            if (GENERIC_CYCLE_COMPLETED_EVENT_TYPES.has(envelope.event_type) && !typedCycleCompleteHandled) {
                const payload = asRecord(envelope.payload);
                const cycleId = payload && typeof payload.cycle_id === 'string'
                    ? payload.cycle_id
                    : undefined;
                clearAgentMuij(envelope.agent_id, cycleId);
            }
        }

        // R318: Removed duplicate clearAgentMuij for AgentCycleCompleted —
        // already handled by GENERIC_CYCLE_COMPLETED_EVENT_TYPES in the
        // AgentEvent envelope routing above (lines 1096-1101).

        // R623: Warn about unrecognized event types in DEV to surface protocol mismatches
        if (import.meta.env.DEV && !KNOWN_V2_EVENT_TYPES.has(eventWithSequence.event_type)) {
            console.warn(`[V2WS] unrecognized event_type "${eventWithSequence.event_type}" — stored but not routed`, eventWithSequence.data);
        }
    }

    // Clear all events
    clear() {
        this.events.set([]);
    }

    // Get connection state for debugging
    getConnectionState(): string {
        if (!this.ws) return 'CLOSED';

        switch (this.ws.readyState) {
            case WebSocket.CONNECTING: return 'CONNECTING';
            case WebSocket.OPEN: return 'OPEN';
            case WebSocket.CLOSING: return 'CLOSING';
            case WebSocket.CLOSED: return 'CLOSED';
            default: return 'UNKNOWN';
        }
    }

    // Send a JSON message to the WebSocket server.
    // R620: Wrap in try-catch to handle TOCTOU race where socket transitions from
    // OPEN to CLOSING between readyState check and send() call, which can throw
    // DOMException: InvalidStateError in some browsers.
    send(message: Record<string, unknown>): boolean {
        if (this.ws && this.ws.readyState === WebSocket.OPEN) {
            try {
                this.ws.send(JSON.stringify(message));
                return true;
            } catch {
                console.debug('[v2-ws] send() failed (socket state changed) — triggering reconnect');
                this.reconnect();
                return false;
            }
        }
        return false;
    }
}

// Export singleton instance
export const v2Events = new V2WebSocketManager();

// Dev-only: vite HMR re-evaluates this module on edit, constructing a NEW
// singleton — close the old instance's socket or it leaks one per hot reload.
if (import.meta.hot) {
    import.meta.hot.dispose(() => v2Events.disconnect());
}

// Helper function to get events for a specific execution.
export function getV2EventsForExecution(events: V2WebSocketEvent[], executionId: string): V2WebSocketEvent[] {
    return events.filter(event =>
        'execution_id' in event.data && event.data.execution_id === executionId
    );
}

// Helper function to get latest event by type
export function getLatestV2EventByType<T extends V2WebSocketEvent['event_type']>(
    events: V2WebSocketEvent[],
    type: T
): Extract<V2WebSocketEvent, { event_type: T }> | null {
    const filtered = events.filter((event): event is Extract<V2WebSocketEvent, { event_type: T }> =>
        event.event_type === type
    );
    return filtered.length > 0 ? filtered[filtered.length - 1] : null;
}

// Helper to check if processing is active based on events
export function isProcessingActive(events: V2WebSocketEvent[], executionId: string): boolean {
    const executionEvents = getV2EventsForExecution(events, executionId);

    // Check if we have a started event but no completed/cancelled/error event
    const lastStarted = getLatestV2EventByType(executionEvents, 'MessageProcessingStarted');
    const lastCompleted = getLatestV2EventByType(executionEvents, 'MessageCompleted');
    // NOTE: Use ExecutionCancelled instead of ProcessingCancelled
    const lastCancelled = getLatestV2EventByType(executionEvents, 'ExecutionCancelled');
    const lastError = getLatestV2EventByType(executionEvents, 'ProcessingError');

    if (!lastStarted) return false;

    const startTime = lastStarted.data.timestamp;
    const endTime = Math.max(
        lastCompleted?.data.timestamp || 0,
        lastCancelled?.data.timestamp || 0,
        lastError?.data.timestamp || 0
    );

    return startTime > endTime;
}
