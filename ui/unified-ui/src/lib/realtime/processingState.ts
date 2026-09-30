// Processing state management for V2 message processing
import { writable, derived, get } from 'svelte/store';
import type { V2WebSocketEvent } from '$lib/realtime/v2-websocket';
import { v2Events, isProcessingActive, getLatestV2EventByType } from '$lib/realtime/v2-websocket';
import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

import { timedFetch } from '$lib/shared/fetch';
import { coordinateExecutionControl } from '$lib/magician/execution/controlClient';
// Performance: Limit log array size to prevent memory issues
const MAX_LOG_ENTRIES = 500;

export interface LogEntry {
    timestamp: number;
    level: 'info' | 'warn' | 'error';
    message: string;
    provider?: string;
    details?: Record<string, any>;
}

export interface ProcessingState {
    isProcessing: boolean;
    executionId: string | null;
    correlationId: string | null;
    stage: string;
    progress: number; // 0-100
    currentTask: string;
    canCancel: boolean;
    error: string | null;
    logs: LogEntry[];
    // Cache for execution state
    executionStepsTotalCache?: number;  // Cached from ExecutionStarted for progress calculation
}

const defaultState: ProcessingState = {
    isProcessing: false,
    executionId: null,
    correlationId: null,
    stage: 'idle',
    progress: 0,
    currentTask: '',
    canCancel: false,
    error: null,
    logs: []
};

/**
 * Helper function to add a log entry while maintaining the maximum size limit.
 * Keeps only the last MAX_LOG_ENTRIES entries.
 */
function addLogEntry(existingLogs: LogEntry[], newLog: LogEntry): LogEntry[] {
    const updatedLogs = [...existingLogs, newLog];
    // If we exceed the limit, keep only the last MAX_LOG_ENTRIES entries
    if (updatedLogs.length > MAX_LOG_ENTRIES) {
        return updatedLogs.slice(updatedLogs.length - MAX_LOG_ENTRIES);
    }
    return updatedLogs;
}

// Main processing state store
function createProcessingStore() {
    const { subscribe, set, update } = writable<ProcessingState>(defaultState);

    return {
        subscribe,

        // Start processing a message
        startProcessing: (executionId: string) => {
            update(state => ({
                ...state,
                isProcessing: true,
                executionId,
                stage: 'starting',
                progress: 0,
                currentTask: 'Initializing...',
                canCancel: true,
                error: null
            }));
        },

        // Update processing state from V2 events
        updateFromEvent: (event: V2WebSocketEvent) => {
            update(state => {
                switch (event.event_type) {
                    case 'MessageProcessingStarted':
                        return {
                            ...state,
                            isProcessing: true,
                            executionId: event.data.execution_id,
                            correlationId: event.data.correlation_id,
                            stage: 'started',
                            progress: 5,
                            currentTask: 'Message processing started...',
                            canCancel: true,
                            error: null
                        };

                    case 'QueryAnalysisCompleted':
                        return {
                            ...state,
                            stage: 'analysis',
                            progress: 20,
                            currentTask: `Analyzed query - ${event.data.intent} (complexity: ${event.data.complexity_score.toFixed(2)})`
                        };

                    case 'StrategySelected':
                        return {
                            ...state,
                            stage: 'strategy',
                            progress: 30,
                            currentTask: `Strategy selected: ${event.data.strategy} (confidence: ${(event.data.confidence * 100).toFixed(0)}%)`
                        };

                    case 'ToolMatchingTierStarted':
                        // Map tier numbers to progress (30-70%)
                        const tierStartProgress = 30 + (event.data.tier_number * 10);
                        return {
                            ...state,
                            stage: 'tool_matching',
                            progress: tierStartProgress,
                            currentTask: `${event.data.tier_name}: ${event.data.description}`
                        };

                    case 'ToolMatchingTierCompleted':
                        // Map tier numbers to progress (35-75%)
                        const tierCompleteProgress = 35 + (event.data.tier_number * 10);
                        const topTool = event.data.top_candidates && event.data.top_candidates.length > 0
                            ? event.data.top_candidates[0].tool_name
                            : 'N/A';
                        return {
                            ...state,
                            stage: 'tool_matching',
                            progress: tierCompleteProgress,
                            currentTask: `${event.data.tier_name} complete: ${event.data.candidates_count} tools, top: ${topTool}`,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Tool matching tier ${event.data.tier_number} (${event.data.tier_name}) completed: ${event.data.candidates_count} candidates found`,
                                details: {
                                    tier_name: event.data.tier_name,
                                    candidates_count: event.data.candidates_count,
                                    top_tool: topTool
                                }
                            })
                        };

                    case 'ExplorationProgress':
                        return {
                            ...state,
                            stage: 'exploration',
                            progress: 40 + (event.data.progress_percent * 0.4), // 40-80%
                            currentTask: event.data.current_task || `Exploring (${event.data.nodes_explored} nodes)`,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Exploration progress: ${event.data.progress_percent.toFixed(0)}% (${event.data.nodes_explored} nodes explored)`,
                                details: {
                                    nodes_explored: event.data.nodes_explored,
                                    progress_percent: event.data.progress_percent,
                                    current_task: event.data.current_task
                                }
                            })
                        };

                    // NOTE: ToolMatching handler removed - superseded by ToolMatchingTierStarted/Completed

                    case 'MessageCompleted':
                        return {
                            ...state,
                            isProcessing: false,
                            stage: 'completed',
                            progress: 100,
                            currentTask: 'Processing complete',
                            canCancel: false,
                            correlationId: null
                        };

                    case 'ExecutionStatusChanged':
                        // Log status change for debugging.
                        console.log(`🔄 Execution status changed: ${event.data.previous_status} → ${event.data.new_status}`, event.data);
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Execution status: ${event.data.previous_status} → ${event.data.new_status}${event.data.reason ? ` (${event.data.reason})` : ''}`,
                                details: {
                                    previous_status: event.data.previous_status,
                                    new_status: event.data.new_status,
                                    reason: event.data.reason
                                }
                            })
                        };

                    // NOTE: ProcessingCancelled handler removed - ExecutionCancelled covers cancellation

                    case 'ProcessingError':
                        return {
                            ...state,
                            isProcessing: false, // Stop processing
                            stage: 'error',
                            progress: 0, // Reset progress to 0
                            currentTask: `Error: ${event.data.error_message}`, // Show error in task
                            canCancel: false, // Cannot cancel after error
                            correlationId: null,
                            error: event.data.error_message, // Store full error
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'error',
                                message: event.data.error_message,
                                details: { error_type: event.data.error_type }
                            })
                        };

                    case 'LLMAnalysisStarted':
                        return {
                            ...state,
                            stage: 'llm_analysis',
                            progress: 10,
                            currentTask: `Starting LLM analysis (${event.data.provider})...`,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `LLM analysis started with ${event.data.provider} (query: ${event.data.query_length} chars)`,
                                provider: event.data.provider,
                                details: { query_length: event.data.query_length }
                            })
                        };

                    case 'LLMAnalysisCompleted':
                        return {
                            ...state,
                            stage: 'llm_analysis_complete',
                            progress: 15,
                            currentTask: `LLM analysis completed in ${event.data.duration_ms}ms`,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `LLM analysis completed successfully (${event.data.duration_ms}ms, response: ${event.data.response_length} chars)`,
                                provider: event.data.provider,
                                details: {
                                    duration_ms: event.data.duration_ms,
                                    response_length: event.data.response_length
                                }
                            })
                        };

                    case 'LLMAnalysisFailed':
                        return {
                            ...state,
                            isProcessing: false,
                            stage: 'error',
                            progress: 0,
                            currentTask: `LLM analysis failed: ${event.data.error_type}`,
                            canCancel: false,
                            correlationId: null,
                            error: event.data.error_message,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'error',
                                message: `LLM analysis failed (${event.data.provider}): ${event.data.error_message}`,
                                provider: event.data.provider,
                                details: {
                                    error_type: event.data.error_type,
                                    error_message: event.data.error_message
                                }
                            })
                        };

                    case 'AtomicPlanOutlineStarted':
                        return {
                            ...state,
                            stage: 'plan_outline',
                            progress: 25,
                            currentTask: 'Generating plan outline...',
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: 'Started generating atomic plan outline',
                                details: { query: event.data.query }
                            })
                        };

                    case 'AtomicPlanOutlineCompleted':
                        return {
                            ...state,
                            stage: 'plan_outline_complete',
                            progress: 35,
                            currentTask: `Plan outline completed: ${event.data.goals_count} goals`,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Plan outline completed with ${event.data.goals_count} goals (confidence: ${(event.data.confidence * 100).toFixed(0)}%)`,
                                details: {
                                    goals_count: event.data.goals_count,
                                    confidence: event.data.confidence
                                }
                            })
                        };

                    case 'AtomicPlanExpansionStarted':
                        return {
                            ...state,
                            stage: 'plan_expansion',
                            progress: 45,
                            currentTask: 'Expanding plan to detailed steps...',
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: 'Started expanding plan into detailed steps',
                                details: event.data
                            })
                        };

                    case 'AtomicPlanGenerated':
                        return {
                            ...state,
                            stage: 'plan_complete',
                            progress: 60,
                            currentTask: `Plan generated: ${event.data.steps_count} steps`,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Atomic plan generated with ${event.data.steps_count} steps (confidence: ${((event.data.confidence ?? 0) * 100).toFixed(0)}%)`,
                                details: {
                                    steps_count: event.data.steps_count,
                                    confidence: event.data.confidence,
                                    provenance: event.data.provenance
                                }
                            })
                        };

                    case 'SlotExtracted':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Extracted slot: ${event.data.slot_name ?? event.data.slot_id} (type: ${event.data.slot_type})`,
                                details: {
                                    slot_id: event.data.slot_id,
                                    slot_name: event.data.slot_name,
                                    slot_type: event.data.slot_type,
                                    confidence: event.data.confidence
                                }
                            })
                        };

                    case 'SlotEnrichmentStarted':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Started enriching ${event.data.total_slots} slot(s) with ${event.data.enricher_count} enricher(s)`,
                                details: {
                                    total_slots: event.data.total_slots,
                                    enricher_count: event.data.enricher_count,
                                    slot_name: event.data.slot_name
                                }
                            })
                        };

                    case 'SlotExtractionStarted':
                        return {
                            ...state,
                            stage: 'slot_extraction',
                            progress: 15,
                            currentTask: 'Extracting parameters from query...',
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: 'Started slot extraction process',
                                details: event.data
                            })
                        };

                    case 'SlotEnrichmentCompleted':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Slot enrichment completed (${event.data.slots_changed} slot(s) changed)`,
                                details: {
                                    total_slots: event.data.total_slots,
                                    slots_changed: event.data.slots_changed,
                                    invocations: event.data.invocations,
                                    errors_count: event.data.errors_count
                                }
                            })
                        };

                    case 'SlotConfidenceUpdated':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Slot confidence updated: ${event.data.slot_name ?? event.data.slot_id} → ${(event.data.new_confidence * 100).toFixed(0)}%`,
                                details: {
                                    slot_id: event.data.slot_id,
                                    slot_name: event.data.slot_name,
                                    old_confidence: event.data.old_confidence,
                                    new_confidence: event.data.new_confidence,
                                    source: event.data.source,
                                    enricher: event.data.enricher
                                }
                            })
                        };

                    case 'ClarifiedTaskReady':
                        return {
                            ...state,
                            stage: 'clarification_complete',
                            progress: 25,
                            currentTask: 'Clarified task ready for planning',
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Task clarification complete (${event.data.slot_count || 0} parameters extracted)`,
                                details: {
                                    slot_count: event.data.slot_count,
                                    avg_confidence: event.data.avg_confidence
                                }
                            })
                        };

                    // `ClarificationQueued` / `ClarificationResponseReceived`
                    // cases retired in H7.2 — canonical `HitlRequested` /
                    // `HitlResolved { source: "clarification" }` is the
                    // wire shape. processingState reads the canonical
                    // envelopes via the upstream `HitlRequested` /
                    // `HitlResolved` handlers (no dedicated processing-
                    // state row needed; pendingHitlStore + Plan Inspector
                    // surface the data).

                    case 'ClarificationSessionSnapshot': {
                        const waiting = event.data.waiting_on_user ?? 0;
                        const queued = event.data.queued ?? 0;
                        const answered = event.data.answered ?? 0;
                        const statusMessage =
                            waiting === 0 && queued === 0
                                ? 'Clarification queue is clear'
                                : `Clarification session updated (${waiting} waiting, ${queued} queued, ${answered} answered)`;

                        return {
                            ...state,
                            stage: waiting === 0 && queued === 0 ? 'clarification_complete' : 'clarification_needed',
                            currentTask: statusMessage,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: statusMessage,
                                details: {
                                    state: event.data.state,
                                    total_questions: event.data.total_questions,
                                    waiting_on_user: waiting,
                                    queued,
                                    answered,
                                    cancelled: event.data.cancelled,
                                    active_batch: event.data.active_batch
                                }
                            })
                        };
                    }

                    case 'ClarificationConfidenceSnapshot': {
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.answered_at ?? event.data.timestamp,
                                level: 'info',
                                message: `Confidence updated to ${(event.data.overall_confidence * 100).toFixed(1)}% (trigger: ${event.data.trigger})`,
                                details: {
                                    question_id: event.data.question_id,
                                    unresolved_count: event.data.unresolved_count,
                                    slot_deltas: event.data.slot_deltas
                                }
                            })
                        };
                    }

                    case 'ObservabilityAlert': {
                        const asNumber = (value: unknown): number | undefined =>
                            typeof value === 'number' ? value : undefined;
                        const details = event.data.details ?? {};
                        const guardrailMessage = (() => {
                            switch (event.data.alert_type) {
                                case 'clarification_round_cap': {
                                    const round = asNumber(details.round_count);
                                    const limit = asNumber(details.max_rounds);
                                    return `Clarification round limit reached${
                                        round !== undefined && limit !== undefined
                                            ? ` (${round}/${limit})`
                                            : ''
                                    }. Finishing current questions before resuming.`;
                                }
                                case 'clarification_question_cap': {
                                    const asked = asNumber(details.total_questions_asked);
                                    const cap = asNumber(details.question_cap);
                                    return `Clarification question cap reached${
                                        asked !== undefined && cap !== undefined
                                            ? ` (${asked}/${cap})`
                                            : ''
                                    }. Resuming planning with best available answers.`;
                                }
                                case 'clarification_timeout': {
                                    const pendingIds = Array.isArray(details.pending_question_ids)
                                        ? details.pending_question_ids
                                        : [];
                                    const pendingCount = pendingIds.length;
                                    return pendingCount > 0
                                        ? `Clarification session timed out with ${pendingCount} unanswered question${pendingCount === 1 ? '' : 's'}.`
                                        : 'Clarification session timed out. Continuing with collected answers.';
                                }
                                default:
                                    return null;
                            }
                        })();

                        return {
                            ...state,
                            stage: guardrailMessage ? 'clarification_guardrail' : state.stage,
                            currentTask: guardrailMessage ?? state.currentTask,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'warn',
                                message:
                                    guardrailMessage ??
                                    `Observability alert: ${event.data.alert_type}`,
                                details
                            })
                        };
                    }

                    case 'ClarificationMetricsSnapshot':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: 'Clarification metrics updated',
                                details: {
                                    total_sessions_started: event.data.total_sessions_started,
                                    total_sessions_completed: event.data.total_sessions_completed,
                                    active_sessions: event.data.active_sessions,
                                    avg_session_duration_ms: event.data.avg_session_duration_ms,
                                    avg_questions_per_session: event.data.avg_questions_per_session,
                                    guardrail_timeouts: event.data.guardrail_timeouts,
                                    guardrail_question_caps: event.data.guardrail_question_caps,
                                    guardrail_round_caps: event.data.guardrail_round_caps,
                                }
                            })
                        };

                    case 'WorkflowResumed':
                        return {
                            ...state,
                            stage: 'resumed',
                            currentTask: `Workflow resumed (${event.data.resume_mode} mode)`,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Workflow resumed with ${event.data.resume_mode} strategy`,
                                details: {
                                    resume_mode: event.data.resume_mode,
                                    answered_count: event.data.answered_count,
                                    pending_count: event.data.pending_count
                                }
                            })
                        };

                    case 'WorkflowStageResumed':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Resumed stage: ${event.data.stage_name}`,
                                details: {
                                    stage_name: event.data.stage_name,
                                    stage_context: event.data.stage_context,
                                    attempt: event.data.attempt,
                                    reused_checkpoint: event.data.reused_checkpoint,
                                    checkpoint_hash: event.data.checkpoint_hash,
                                    reused_stages: event.data.reused_stages
                                }
                            })
                        };

                    case 'WorkflowResumeFailed':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'error',
                                message: `Workflow resume failed: ${event.data.error}`,
                                details: {
                                    question_id: event.data.question_id,
                                    error: event.data.error
                                }
                            })
                        };

                    // Execution Events (Plan Execution Lifecycle)
                    case 'ExecutionStarted':
                        return {
                            ...state,
                            stage: 'execution',
                            progress: 60,
                            currentTask: `Executing plan (${event.data.steps_total} steps)...`,
                            executionStepsTotalCache: event.data.steps_total,  // Cache for later use
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Execution started for plan ${event.data.plan_id} with ${event.data.steps_total} steps`,
                                details: {
                                    plan_id: event.data.plan_id,
                                    steps_total: event.data.steps_total
                                }
                            })
                        };

                    case 'ExecutionStepStarted':
                        // Progress at START of step: 60% base + (step_index / total * 30%) = 60-90% range
                        const cachedStepsTotal = state.executionStepsTotalCache ?? event.data.steps_total ?? 1;
                        const stepStartProgress = 60 + (event.data.step_index / cachedStepsTotal) * 30;
                        return {
                            ...state,
                            stage: 'execution',
                            progress: Math.min(stepStartProgress, 90),
                            currentTask: `Executing step ${event.data.step_index + 1}/${cachedStepsTotal}...`,
                            executionStepsTotalCache: cachedStepsTotal,  // Update cache if available
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Step ${event.data.step_index + 1}/${cachedStepsTotal} started (${event.data.step_id})`,
                                details: {
                                    step_index: event.data.step_index,
                                    step_id: event.data.step_id,
                                    steps_total: cachedStepsTotal
                                }
                            })
                        };

                    case 'ExecutionStepCompleted':
                        // Progress at END of step: 60% base + ((step_index + 1) / total * 30%) = 60-90% range
                        // Use cached steps_total since backend doesn't provide it in this event
                        const stepsTotal = state.executionStepsTotalCache ?? 1;
                        const stepCompleteProgress = 60 + ((event.data.step_index + 1) / stepsTotal) * 30;
                        const stepSuccess = event.data.success;
                        return {
                            ...state,
                            stage: 'execution',
                            progress: Math.min(stepCompleteProgress, 90),
                            currentTask: stepSuccess
                                ? `Step ${event.data.step_index + 1}/${stepsTotal} completed`
                                : `Step ${event.data.step_index + 1}/${stepsTotal} failed`,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: stepSuccess ? 'info' : 'warn',
                                message: `Step ${event.data.step_index + 1}/${stepsTotal} ${stepSuccess ? 'completed successfully' : 'failed'} (${event.data.step_id})`,
                                details: {
                                    step_index: event.data.step_index,
                                    step_id: event.data.step_id,
                                    success: stepSuccess
                                }
                            })
                        };

                    case 'ExecutionPaused':
                        return {
                            ...state,
                            stage: 'paused',
                            currentTask: `Execution paused: ${event.data.reason}`,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'warn',
                                message: `Execution paused at step ${event.data.step_index + 1}: ${event.data.reason}`,
                                details: {
                                    step_index: event.data.step_index,
                                    step_id: event.data.step_id,
                                    reason: event.data.reason
                                }
                            })
                        };

                    case 'ExecutionResumed':
                        return {
                            ...state,
                            stage: 'execution',
                            currentTask: `Execution resumed (${event.data.mode} mode)`,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Execution resumed from step ${event.data.step_index + 1} (${event.data.mode} mode)`,
                                details: {
                                    step_index: event.data.step_index,
                                    step_id: event.data.step_id || `unknown-step-${event.data.step_index}`,  // Guard against undefined
                                    mode: event.data.mode
                                }
                            })
                        };

                    case 'ExecutionFailed':
                        return {
                            ...state,
                            isProcessing: false,
                            stage: 'error',
                            progress: 0,  // Reset progress on failure (was stale at last step value)
                            currentTask: `Execution failed: ${event.data.error}`,
                            canCancel: false,
                            error: event.data.error,
                            executionStepsTotalCache: undefined,  // FIX CRITICAL 5: Clear cache on failure
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'error',
                                message: `Execution failed at step ${event.data.step_index + 1}: ${event.data.error}`,
                                details: {
                                    step_index: event.data.step_index,
                                    step_id: event.data.step_id,
                                    error: event.data.error
                                }
                            })
                        };

                    case 'ExecutionCompleted':
                        return {
                            ...state,
                            isProcessing: false,
                            stage: event.data.success ? 'completed' : 'error',
                            progress: event.data.success ? 100 : 0,  // Only 100% on actual success
                            currentTask: event.data.success
                                ? `Execution completed successfully (${event.data.steps_total} steps)`
                                : `Execution completed with errors (${event.data.steps_total} steps)`,
                            canCancel: false,
                            executionStepsTotalCache: undefined,  // FIX CRITICAL 5: Clear cache on completion
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: event.data.success ? 'info' : 'warn',
                                message: `Execution ${event.data.success ? 'completed successfully' : 'completed with errors'} (${event.data.steps_total} steps total)`,
                                details: {
                                    plan_id: event.data.plan_id,
                                    steps_total: event.data.steps_total,
                                    success: event.data.success
                                }
                            })
                        };

                    case 'ExecutionRestoreFailed':
                        return {
                            ...state,
                            isProcessing: false,
                            stage: 'error',
                            canCancel: false,
                            error: `Restore failed: ${event.data.reason}${event.data.note ? ` (${event.data.note})` : ''}`,
                            executionStepsTotalCache: undefined,  // FIX CRITICAL 5: Clear cache on restore failure
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'error',
                                message: `Execution restore failed: ${event.data.reason}${event.data.note ? ` (${event.data.note})` : ''}`,
                                details: {
                                    reason: event.data.reason,
                                    note: event.data.note
                                }
                            })
                        };

                    case 'ExecutionInflightResent':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Resending inflight request at step ${event.data.step_index + 1} (attempt ${event.data.attempt_count})`,
                                details: {
                                    step_index: event.data.step_index,
                                    step_id: event.data.step_id,
                                    request_id: event.data.request_id,
                                    attempt_count: event.data.attempt_count
                                }
                            })
                        };

                    case 'ExecutionInflightDropped':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'warn',
                                message: `Dropped inflight request at step ${event.data.step_index + 1} (no retry)`,
                                details: {
                                    step_index: event.data.step_index,
                                    step_id: event.data.step_id,
                                    request_id: event.data.request_id
                                }
                            })
                        };

                    case 'ExecutionCancelled':
                        return {
                            ...state,
                            isProcessing: false,
                            stage: 'cancelled',
                            progress: 0,
                            currentTask: 'Execution cancelled by user',
                            canCancel: false,
                            executionStepsTotalCache: undefined,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Execution cancelled at step ${event.data.step_index + 1}`,
                                details: {
                                    step_index: event.data.step_index,
                                    plan_id: event.data.plan_id
                                }
                            })
                        };

                    case 'SlotGraphDiff':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Slot graph updated: ${event.data.inserted} added, ${event.data.updated} updated, ${event.data.removed} removed (${event.data.total_slots} total)`,
                                details: {
                                    source: event.data.source,
                                    inserted: event.data.inserted,
                                    updated: event.data.updated,
                                    removed: event.data.removed,
                                    total_slots: event.data.total_slots
                                }
                            })
                        };

                    // Parameter Discovery Events
                    case 'ParameterDiscoveryAttempted':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Discovering parameter: ${event.data.parameter_name} via ${event.data.discovery_method}`,
                                details: {
                                    parameter_name: event.data.parameter_name,
                                    discovery_method: event.data.discovery_method
                                }
                            })
                        };

                    case 'ParameterDiscovered':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Parameter discovered: ${event.data.parameter_name} (${(event.data.confidence * 100).toFixed(0)}% confidence)`,
                                details: {
                                    parameter_name: event.data.parameter_name,
                                    discovery_method: event.data.discovery_method,
                                    confidence: event.data.confidence,
                                    external_actions: event.data.external_actions_performed
                                }
                            })
                        };

                    case 'ParameterDiscoveryFailed':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'warn',
                                message: `Parameter discovery failed: ${event.data.parameter_name} - ${event.data.reason}`,
                                details: {
                                    parameter_name: event.data.parameter_name,
                                    reason: event.data.reason
                                }
                            })
                        };

                    // Agentic Execution Events
                    case 'AgenticExecutionStarted':
                        return {
                            ...state,
                            stage: 'agentic_execution',
                            currentTask: `Agentic execution: ${event.data.goal}`,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Agentic execution started: ${event.data.goal} (max ${event.data.max_iterations} iterations)`,
                                details: {
                                    goal: event.data.goal,
                                    success_criteria: event.data.success_criteria,
                                    max_iterations: event.data.max_iterations,
                                    hint_action: event.data.hint_action
                                }
                            })
                        };

                    case 'AgenticIterationStarted':
                        return {
                            ...state,
                            currentTask: `Agentic iteration ${event.data.iteration}: observing ${event.data.environment_type}`,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Iteration ${event.data.iteration}: observing ${event.data.environment_type}`,
                                details: {
                                    iteration: event.data.iteration,
                                    environment_type: event.data.environment_type
                                }
                            })
                        };

                    case 'AgenticPageUnderstanding':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: `Page analyzed: ${event.data.page_stage} (${event.data.element_count} elements, ${(event.data.confidence * 100).toFixed(0)}% confidence)`,
                                details: {
                                    observation_id: event.data.observation_id,
                                    page_stage: event.data.page_stage,
                                    element_count: event.data.element_count,
                                    appears_loading: event.data.appears_loading,
                                    url: event.data.url,
                                    has_screenshot: event.data.has_screenshot
                                }
                            })
                        };

                    case 'AgenticDecisionMade': {
                        // tactical pattern T4: when the decision is delegate_to_agent
                        // with multiple targets, surface the fan-out in
                        // the log line so operators reading the timeline
                        // see decomposition explicitly. Generic decisions
                        // pass through with the legacy message.
                        let fanoutSummary: string | null = null;
                        if (
                            event.data.decision_type === 'delegate_to_agent' &&
                            event.data.raw_decision &&
                            typeof event.data.raw_decision === 'object'
                        ) {
                            const raw = event.data.raw_decision as Record<string, unknown>;
                            const targets = Array.isArray(raw.delegation_targets)
                                ? (raw.delegation_targets as unknown[])
                                : [];
                            const targetIds = targets
                                .map((t) => {
                                    if (t && typeof t === 'object') {
                                        const obj = t as Record<string, unknown>;
                                        return typeof obj.target_agent_id === 'string'
                                            ? obj.target_agent_id
                                            : null;
                                    }
                                    return null;
                                })
                                .filter((id): id is string => !!id);
                            if (targetIds.length >= 2) {
                                fanoutSummary = `Delegating to ${targetIds.length} agents in parallel: ${targetIds.join(', ')}`;
                            } else if (targetIds.length === 1) {
                                fanoutSummary = `Delegating to ${targetIds[0]}`;
                            }
                        }
                        const baseMessage = fanoutSummary
                            ? fanoutSummary
                            : `Decision made: ${event.data.decision_type}${event.data.action_summary ? ` - ${event.data.action_summary}` : ''}`;
                        return {
                            ...state,
                            currentTask: fanoutSummary
                                ? fanoutSummary
                                : event.data.action_summary
                                    ? `Decision: ${event.data.action_summary}`
                                    : `Decision: ${event.data.decision_type}`,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: 'info',
                                message: baseMessage,
                                details: {
                                    iteration: event.data.iteration,
                                    decision_type: event.data.decision_type,
                                    action_summary: event.data.action_summary,
                                    reasoning: event.data.reasoning,
                                    confidence: event.data.confidence,
                                    fanout_target_count:
                                        event.data.decision_type === 'delegate_to_agent'
                                            ? Array.isArray(
                                                  (event.data.raw_decision as Record<string, unknown> | undefined)
                                                      ?.delegation_targets
                                              )
                                                ? ((event.data.raw_decision as Record<string, unknown>)
                                                      .delegation_targets as unknown[]).length
                                                : undefined
                                            : undefined
                                }
                            })
                        };
                    }

                    case 'AgenticActionExecuted':
                        return {
                            ...state,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: event.data.success ? 'info' : 'warn',
                                message: `Action ${event.data.success ? 'succeeded' : 'failed'}: ${event.data.action_type} on ${event.data.target} (${event.data.latency_ms}ms)`,
                                details: {
                                    iteration: event.data.iteration,
                                    action_type: event.data.action_type,
                                    target: event.data.target,
                                    success: event.data.success,
                                    latency_ms: event.data.latency_ms,
                                    error: event.data.error
                                }
                            })
                        };

                    case 'AgenticExecutionCompleted': {
                        // tactical pattern T1: the executor emits an intermediate
                        // pass-0 completion when refinement is pending,
                        // then a pass-N completion when refinement
                        // resolves. The intermediate event must NOT
                        // park the UI in `completed` — the run isn't
                        // done. Show it as `refining` (a transient
                        // running-like state) until the authoritative
                        // pass-N event arrives.
                        const isIntermediateRefinement =
                            event.data.refinement_pending === true &&
                            (event.data.refinement_pass_index ?? 0) === 0;
                        const isFullSuccess =
                            event.data.outcome === 'success' ||
                            event.data.outcome === 'goal_achieved';
                        const isPartialSuccess =
                            event.data.outcome === 'goal_achieved_partial' ||
                            event.data.outcome === 'partial_progress';
                        const nextStage = isIntermediateRefinement
                            ? 'refining'
                            : isFullSuccess
                                ? 'completed'
                                : isPartialSuccess
                                    ? 'partial'
                                    : 'error';
                        const logMessage = isIntermediateRefinement
                            ? `Agentic execution finished pass 0 (partial); refinement pass commissioned.`
                            : `Agentic execution ${event.data.outcome}: ${event.data.summary} (${event.data.iterations_used} iterations, ${event.data.duration_ms}ms)`;
                        return {
                            ...state,
                            stage: nextStage,
                            currentTask: event.data.summary,
                            logs: addLogEntry(state.logs, {
                                timestamp: event.data.timestamp,
                                level: isFullSuccess ? 'info' : 'warn',
                                message: logMessage,
                                details: {
                                    outcome: event.data.outcome,
                                    iterations_used: event.data.iterations_used,
                                    artifacts: event.data.artifacts,
                                    duration_ms: event.data.duration_ms,
                                    refinement_pass_index: event.data.refinement_pass_index ?? 0,
                                    refinement_pending: event.data.refinement_pending ?? false
                                }
                            })
                        };
                    }

                    default:
                        return state;
                }
            });
        },

        // Reset to default state
        reset: () => {
            set(defaultState);
        },

        // Cancel processing
        cancelProcessing: async (executionId: string) => {
            try {
                const result = await coordinateExecutionControl(executionId, async () => {
                    const response = await timedFetch(`/api/magician/v3/executions/${encodeURIComponent(executionId)}/cancel`, {
                        method: 'POST',
                        headers: scopedRequestHeaders({
                            'Content-Type': 'application/json'
                        })
                    });

                    if (!response.ok) {
                        const error = await response.json();
                        throw new Error(error.error || 'Failed to cancel processing');
                    }

                    return response.json();
                });
                console.log('✅ Processing cancelled:', result);

                // State will be updated via WebSocket event
                return result;
            } catch (error) {
                console.error('❌ Failed to cancel processing:', error);
                update(state => ({
                    ...state,
                    error: error instanceof Error ? error.message : 'Failed to cancel processing'
                }));
                throw error;
            }
        }
    };
}

export const processingState = createProcessingStore();

// NOTE: Event processing is handled by the page component to avoid duplicates.
// The page subscribes to v2Events and calls processingState.updateFromEvent() for each event.
