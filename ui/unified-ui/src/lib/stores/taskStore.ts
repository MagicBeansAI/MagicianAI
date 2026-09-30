/**
 * Task Store - Manages TODO items separately from execution
 *
 * Key behaviors:
 * - Tasks are created without triggering execution
 * - Execution only starts when user clicks "Do it for me" then "Execute"
 * - Plan review step between planning and execution
 * - Configurable concurrent execution limit (default: 1)
 *
 * Unified Agentic Architecture:
 * - Tasks now carry an `agent_id` (compulsory — the Personal agent assigned).
 * - Tasks optionally carry a `schedule` (cron + timezone).
 * - Tasks carry a `created_by` provenance field (user | agent | delegation).
 */

import { browser } from '$app/environment';
import { writable, derived, get } from 'svelte/store';
import type { TaskCreatedBy, TaskSchedule } from '$lib/types/agents';
import { getV2EventSequence, v2Events, type V2WebSocketEvent } from '$lib/realtime/v2-websocket';
import { showError } from '$lib/shared/stores/notifications';
import { scopeIdentityStore, scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';
import { coordinateExecutionControl } from '$lib/magician/execution/controlClient';

// =============================================================================
// Helpers
// =============================================================================

/** Serialize a frontend TaskSchedule to the backend's externally-tagged enum shape. */
export function serializeScheduleForApi(schedule: TaskSchedule): Record<string, unknown> {
    const result: Record<string, unknown> = {
        kind: { Cron: { expression: schedule.cron, timezone: schedule.timezone || null } },
        timezone: schedule.timezone || null,
        missed_fire_policy: 'skip',
        concurrent_execution_policy: 'skip'
    };
    if (schedule.execution_history_retention) {
        result.execution_history_retention = schedule.execution_history_retention;
    }
    if (typeof schedule.max_runs === 'number') {
        result.max_runs = schedule.max_runs;
    }
    if (typeof schedule.paused === 'boolean') {
        result.paused = schedule.paused;
    }
    return result;
}

/** Serialize a frontend TaskCreatedBy string to the backend's internally-tagged enum shape. */
export function serializeCreatedByForApi(createdBy: TaskCreatedBy): Record<string, unknown> {
    // Backend uses #[serde(tag = "type")] — expects { type: "user" }, not plain "user"
    if (createdBy === 'autonomous') return { type: 'agent', agent_id: 'system' };
    if (createdBy === 'delegation') return { type: 'delegation', parent_task_id: '', delegating_agent_id: '' };
    return { type: createdBy };
}

/**
 * Extract a frontend TaskSchedule from the backend's response.
 * Backend shape: { kind: { Cron: { expression: "...", timezone: ... } }, timezone: "..." }
 * Frontend shape: { cron: "...", timezone: "..." }
 */
export function extractSchedule(raw: unknown): TaskSchedule | undefined {
    if (!raw || typeof raw !== 'object') return undefined;
    const obj = raw as Record<string, unknown>;
    // Extract retention policy if present (shared by both shapes)
    const retention = obj.execution_history_retention as TaskSchedule['execution_history_retention'] | undefined;
    const maxRuns = typeof obj.max_runs === 'number' ? obj.max_runs : undefined;
    const paused = typeof obj.paused === 'boolean' ? obj.paused : undefined;
    // Already in frontend shape (e.g. from createTask round-trip)
    if (typeof obj.cron === 'string' && obj.cron) {
        return {
            cron: obj.cron,
            timezone: (obj.timezone as string) || undefined,
            execution_history_retention: retention,
            max_runs: maxRuns,
            paused,
        };
    }
    // Backend externally-tagged enum shape
    const kind = obj.kind as Record<string, unknown> | undefined;
    if (kind && typeof kind === 'object') {
        const cronVariant = kind.Cron as { expression?: string; timezone?: string } | undefined;
        if (cronVariant?.expression) {
            return {
                cron: cronVariant.expression,
                timezone: (obj.timezone as string) || cronVariant.timezone || undefined,
                execution_history_retention: retention,
                max_runs: maxRuns,
                paused,
            };
        }
        // Warn about unsupported schedule variants (Interval, Once, OnEvent)
        const variantKeys = Object.keys(kind);
        if (variantKeys.length > 0) {
            console.warn(`extractSchedule: unsupported schedule kind "${variantKeys[0]}", ignoring`);
        }
    }
    return undefined;
}

export function resolveTaskExecutionId(raw: Record<string, unknown>): string | undefined {
    const executionId = raw.active_root_execution_id ?? raw.latest_root_execution_id ?? raw.last_completed_root_execution_id;
    return typeof executionId === 'string' && executionId.length > 0 ? executionId : undefined;
}

export function resolveTaskActiveExecutionId(raw: Record<string, unknown>): string | undefined {
    const executionId = raw.active_root_execution_id;
    return typeof executionId === 'string' && executionId.trim().length > 0
        ? executionId.trim()
        : undefined;
}

export function normalizeCreatedBy(raw: unknown): TaskCreatedBy {
    const value = typeof raw === 'string' ? raw.trim().toLowerCase() : '';
    if (value === 'agent' || value === 'autonomous') return 'autonomous';
    if (value === 'delegation') return 'delegation';
    if (value === 'system') return 'system';
    return 'user';
}

export function flattenV3TaskRecord(raw: Record<string, unknown>): Record<string, unknown> {
    const manifest = (raw.manifest && typeof raw.manifest === 'object')
        ? raw.manifest as Record<string, unknown>
        : {};
    const state = (raw.state && typeof raw.state === 'object')
        ? raw.state as Record<string, unknown>
        : {};

    return {
        id: manifest.task_id ?? state.task_id,
        title: manifest.title,
        description: manifest.description,
        status: state.status,
        agent_id: manifest.agent_id,
        ui_thread_id: manifest.ui_thread_id,
        priority: manifest.priority,
        due_date: manifest.due_date,
        tags: manifest.tags,
        created_by: manifest.created_by,
        chat_session_id: manifest.chat_session_id,
        lifecycle: manifest.lifecycle,
        sync_mode: manifest.sync_mode,
        output_mode: manifest.output_mode,
        depends_on: manifest.depends_on,
        approved: manifest.approved,
        is_blocked: false,
        schedule: manifest.schedule,
        active_root_execution_id: state.active_root_execution_id,
        latest_root_execution_id: state.latest_root_execution_id,
        last_completed_root_execution_id: state.last_completed_root_execution_id,
        currentStepTitle: raw.current_step_title ?? state.current_step_title,
        currentSubstepTitle: raw.current_substep_title ?? state.current_substep_title,
        completion_summary: state.completion_summary,
        completion_outcome: state.completion_outcome,
        completion_artifact_names: state.completion_artifact_names,
        synthesis_pending: state.synthesis_pending,
        synthesis_failed_execution_id: state.synthesis_failed_execution_id,
        last_progress_at: state.last_progress_at,
        created_at: manifest.created_at,
        updated_at: state.updated_at ?? manifest.updated_at
    };
}

/**
 * Epoch millis for the server's `last_progress_at`, or `undefined`.
 *
 * Deliberately NOT routed through `parseTimestampToIso`, which falls back to
 * `new Date()` for anything it cannot read. That fallback is right for a
 * display timestamp and catastrophic here: a missing or malformed progress
 * instant would become *now*, so a wedged run would look freshly advanced and
 * could never be reported as stalled — the exact failure the field exists to
 * prevent.
 *
 * Absence and anything unreadable collapse to the same `undefined`. Consumers
 * subtract this from `now` and render the result as a duration, so a sentinel
 * would print as a duration rather than degrade to "unknown".
 *
 * Only a parseable RFC3339 string is accepted, because that is the only shape
 * the server sends. Quietly accepting a bare number would invent a second wire
 * representation nothing produces — and make `0` (the classic "never"
 * sentinel) read as 1970 and render as a stall of half a century.
 */
export function parseLastProgressAt(raw: unknown): number | undefined {
    if (typeof raw !== 'string' || !raw.trim()) return undefined;
    const millis = Date.parse(raw);
    return Number.isFinite(millis) ? millis : undefined;
}

// =============================================================================
// Types
// =============================================================================

export type TaskStatus =
    | 'pending'    // Just created, no AI plan yet
    | 'planning'   // AI is generating plan
    | 'ready'      // Plan generated, awaiting user review/execute
    | 'running'    // Execution in progress
    | 'paused'     // Execution paused by user
    | 'completed'  // Successfully finished
    | 'failed'     // Execution failed
    | 'cancelled'  // Execution cancelled by user
    | 'deferred'   // Precondition not met; retry scheduled
    | 'archived';  // Retired from active work, retained for history

export type TaskPriority = 'p1' | 'p2' | 'p3' | 'p4';
export type TaskOutputMode = 'accumulate' | 'overwrite';
export type TaskPlanStatus = 'planning' | 'draft' | 'eliciting' | 'approved' | 'rejected' | 'failed';

export interface TaskTag {
    name: string;
    color: string;
}

export interface PlanStep {
    id: string;
    description: string;
    status: 'pending' | 'in_progress' | 'completed' | 'failed' | 'skipped' | 'cancelled';
    tool_name?: string;
    providing_agent_id?: string;
    duration_ms?: number;
    error?: string;
    confidence?: number;
    depends_on?: string[];
}

export type TaskSource = 'task' | 'execution';
export type TaskStorageBackend = 'v2' | 'v3';

export interface Task {
    id: string;
    title: string;
    description: string;
    status: TaskStatus;
    uiThreadId?: string;
    priority?: TaskPriority;
    dueDate?: string; // ISO date string
    tags: TaskTag[];
    errorMessage?: string;
    retryAt?: number;  // millis epoch — when to retry a deferred task

    // Unified Agentic Architecture: assigned agent and schedule
    agentId?: string;       // Personal agent assigned to this task (compulsory at creation)
    agentName?: string;     // Display name of the assigned agent (denormalized for UI)
    schedule?: TaskSchedule; // Optional recurring schedule (cron + timezone)
    createdBy?: TaskCreatedBy; // Provenance: user | autonomous | system | delegation
    outputMode?: TaskOutputMode; // Task output projection mode

    // Dependency and approval tracking
    dependsOn?: string[];      // task IDs this task depends on
    approved?: boolean;        // whether task is approved for execution
    isBlocked?: boolean;       // derived: has unresolved dependencies

    // Source: 'task' = manually created via tasks API, 'execution' = loaded from execution APIs
    // Only 'task' source items can be updated via tasks API endpoints
    source: TaskSource;

    // Plan state (actual plan content lives in execution turn storage)
    hasPlan?: boolean;
    planStatus?: TaskPlanStatus;
    latestPlanId?: string;
    approvedPlanId?: string;
    planSteps?: PlanStep[];
    planGeneratedAt?: number;

    // Execution tracking
    executionId?: string;
    activeExecutionId?: string;
    currentStepIndex?: number;
    currentStepTitle?: string;
    currentSubstepTitle?: string;
    progress?: number;         // 0-100

    // Clarification
    pendingQuestion?: {
        id: string;
        question: string;
    };
    pendingQuestions?: Array<{
        id: string;
        question: string;
    }>;

    // Completion result (populated by AgenticExecutionCompleted)
    completionSummary?: string;
    completionOutcome?: string;
    completionArtifactNames?: string[];

    // Synthesis pipeline state (see Rust `TaskState.synthesis_pending`
    // / `synthesis_failed_execution_id`). `synthesisPending = true`
    // means the task is in its terminal status on disk but the
    // synthesized outputs (1.1 / 1.2 / 1.3 in magician's pipeline) are
    // still in flight in a background spawn — downstream consumers
    // should wait, and UI surfaces a "synthesizing…" affordance.
    // `synthesisFailedExecutionId` is set when synthesis exhausted its
    // retries; UI renders a "retry synthesis" affordance pointing at
    // that execution.
    synthesisPending?: boolean;
    synthesisFailedExecutionId?: string;

    // Transitional storage routing during the V3 cutover.
    storageBackend?: TaskStorageBackend;
    readOnly?: boolean;

    // Phase 3 — chat-spawning provenance + lifetime classifier.
    // `chatSessionId` is set when the task was spawned from a chat
    // tool-call; `lifecycle` discriminates Persistent (default,
    // user-visible on /tasks) vs Internal (chat/runtime-spawned work,
    // shown only on /tasks?type=internal, auto-cleaned when its spawning
    // chat session is cleared). The /tasks page hides `internal` tasks
    // to avoid drowning the view in transient dispatches (image gen,
    // browser, sub-goals, debug runs, etc.).
    chatSessionId?: string;
    lifecycle?: 'persistent' | 'internal';

    // Recurring Monitors Phase 7 — mirror of the server-owned
    // `monitor_revision` on the V3 list row (omitted from the wire while 0).
    // `> 0` means the task IS a monitor (managed on /tasks?type=monitors);
    // 0/undefined means it is eligible for explicit convert-to-monitor.
    monitorRevision?: number;

    /**
     * This task's run is holding a staged code-change proposal that is still
     * `Pending`, so it is waiting on someone to approve or reject the diff.
     * Mirrors the server's `awaiting_diff_approval` on the V3 list row.
     *
     * **Why the row carries it at all.** A task blocked on a diff reaches the
     * list as `paused`, and `paused` is also what a user-paused run and a
     * plan-time clarification look like — nothing else on the row tells them
     * apart, so the reader is not told that the run is waiting on *them*. The
     * task panel can answer that question (it reads `/execution-panel`), but
     * only for the one task whose panel is open; the list cannot afford a
     * per-row request and does not need one now that the row says so itself.
     *
     * **Server-derived from the proposal store on every read, never from an
     * event.** That is the whole reason to prefer it over the HITL event
     * stream: an event can be dropped or predate a restart, and a directory
     * cannot, so a freshly-started process reports exactly what the process
     * that staged the diff reported.
     *
     * `false` for a terminal task even while a `Pending` proposal is still on
     * disk — that proposal is orphaned rather than actionable, and the server
     * makes that call so no client has to re-derive it.
     *
     * **Absent on the wire when false** (`skip_serializing_if`), so `undefined`
     * is the ordinary case and means "not waiting", not "unknown".
     */
    awaitingDiffApproval?: boolean;

    // Execution history (past runs — included on single-task GETs)
    executionHistory?: TaskExecutionRecord[];

    // Metadata
    createdAt: string;
    updatedAt: string;

    /**
     * Epoch millis at which the run last actually advanced — a step started or
     * finished. **Never `updatedAt`**: that moves on any write, so stall
     * detection reading it would be refreshed by the wedged run's own
     * heartbeats. Mirrors the server's `last_progress_at`.
     *
     * `undefined` means "no progress instant recorded", which surfaces must
     * render as no duration at all rather than substituting a placeholder. The
     * pure verdict modules spell absence `null`; the coercion belongs in the
     * adapter that maps this store onto them, not here.
     */
    lastProgressAt?: number;
}

export interface DeleteTaskOptions {
    removeFiles?: boolean;
}

export interface TaskExecutionRecord {
    execution_id: string;
    started_at: number;
    ended_at?: number;
    status: TaskStatus;
    error_message?: string;
    completion_summary?: string;
    completion_outcome?: string;
    completion_artifact_names?: string[];
}

export interface ExecutionState {
    taskId: string;
    executionId: string;
    status: 'running' | 'paused' | 'completed' | 'failed';
    progress: number;
    currentStep: string;
    currentStepIndex: number;
    totalSteps: number;
    clarifications: Array<{
        id: string;
        question: string;
        answered: boolean;
    }>;
    error?: string;
    startedAt: number;
}

/**
 * The six filter lanes, in the order the toolbar renders them.
 *
 * These are the **server's** wire names for `GET /v3/tasks?view=` — the
 * predicates themselves live in `magician/src/magician_v2/api/task_lanes.rs`,
 * so a lane is answered over the whole corpus rather than over whatever page a
 * client happens to hold.
 */
export const TASK_LANES = ['all', 'inbox', 'today', 'overdue', 'running', 'completed'] as const;
export type TaskLane = (typeof TASK_LANES)[number];

export type TaskFilter = TaskLane | { custom: string };

/**
 * The server's membership answer for ONE lane.
 *
 * Tagged with the lane it answers so a stale answer can never be applied to a
 * different lane: switching filters costs a round trip, and rendering the
 * previous lane's rows under the newly-highlighted pill is the same class of
 * defect as a badge that disagrees with the rows beneath it.
 *
 * Ids rather than rows, deliberately. The rows on screen must stay the store's
 * own records so an optimistic edit — a rename, a status change, a tick — shows
 * up immediately; only *membership* is the server's to decide.
 */
export interface TaskLaneAnswer {
    view: TaskLane;
    ids: ReadonlySet<string>;
}

export interface TaskStoreState {
    tasks: Task[];
    /**
     * VibeDev runs created `Internal` (the default — off the persistent `/tasks`
     * feed). Loaded separately from `/v3/tasks/internal?ui_thread_id=vibedev` so the
     * cockpit run-history rail can show internal runs WITHOUT leaking them into the
     * global `/tasks` list (which reads `tasks`). Kept fresh on the same refresh
     * cadence as `tasks`.
     */
    vibedevInternalTasks: Task[];
    selectedTaskId: string | null;
    filter: TaskFilter;
    filteredTasks: Task[]; // Computed filtered tasks for reactive access
    searchQuery: string;
    executingTask: ExecutionState | null; // Single execution (limit: 1)
    maxConcurrentExecutions: number; // Configurable, default 1
    isLoading: boolean;
    error: string | null;
    /** Task IDs that are in the 5-second grace period after completion */
    pendingCompletions: Set<string>;
    /**
     * The server's answer for the active lane, or `null` when there isn't one
     * yet (first load, a failed request, or a tag filter — which is not a lane).
     * Absent, membership falls back to the mirror in `matchesTaskLane`.
     */
    laneAnswer: TaskLaneAnswer | null;
    /**
     * Every lane's total over the whole corpus, as the server counted it before
     * applying any lane filter — `null` when the server did not report them.
     *
     * A lane missing from this record renders **no badge**, never `0`: absence
     * and zero are different claims, and a fabricated zero is the worse one.
     */
    laneCounts: Readonly<Record<string, number>> | null;
}

// =============================================================================
// Default State
// =============================================================================

const defaultState: TaskStoreState = {
    tasks: [],
    vibedevInternalTasks: [],
    selectedTaskId: null,
    filter: 'all',
    filteredTasks: [],
    searchQuery: '',
    executingTask: null,
    maxConcurrentExecutions: 1, // Configurable for future
    isLoading: true,
    error: null,
    pendingCompletions: new Set(),
    laneAnswer: null,
    laneCounts: null
};

// Module-level storage for completion timers (can't store timeouts in Svelte store)
const completionTimers: Map<string, ReturnType<typeof setTimeout>> = new Map();

/** Grace period in milliseconds before completed tasks move to "Completed" view */
const COMPLETION_GRACE_PERIOD_MS = 5000;
/**
 * The lane request's page size.
 *
 * Large enough to hold the corpus on purpose: `limit`/`offset` are only there
 * to reach the endpoint's paged branch, which is where `view=` and `counts`
 * live. Phase 0 changes who computes the lane, not how much is loaded — the
 * real page arrives with Phase 1.
 */
const TASK_LANE_PAGE_LIMIT = 100000;
const TASK_REALTIME_DEBOUNCE_MS = 500;
const TASK_PLAN_POLL_INTERVAL_MS = 500;
const TASK_PLAN_POLL_TIMEOUT_MS = 15000;
let taskLoadGeneration = 0;

function scopedTaskHeaders(headers?: HeadersInit): Headers {
    return scopedRequestHeaders(headers);
}

async function scopedTaskFetch(input: string, init: RequestInit = {}): Promise<Response> {
    return fetch(input, {
        ...init,
        headers: scopedTaskHeaders(init.headers),
    });
}

function currentTaskScopeKey(): string {
    const scope = get(scopeIdentityStore);
    return `${scope.principal}:${scope.workspace}`;
}

function nextTaskLoadToken(): { generation: number; scopeKey: string } {
    return {
        generation: taskLoadGeneration,
        scopeKey: currentTaskScopeKey()
    };
}

function isStaleTaskLoad(generation: number, scopeKey: string): boolean {
    return generation !== taskLoadGeneration || currentTaskScopeKey() !== scopeKey;
}

export function taskApiErrorMessage(payload: unknown): string | null {
    if (!payload || typeof payload !== 'object') {
        return null;
    }
    const record = payload as Record<string, unknown>;
    const nested = (record.details && typeof record.details === 'object')
        ? record.details as Record<string, unknown>
        : null;
    const direct = typeof record.error === 'string'
        ? record.error
        : typeof record.message === 'string'
            ? record.message
            : null;
    if (direct && direct.trim().length > 0) {
        return direct.trim();
    }
    const nestedReason = nested && typeof nested.reason === 'string' ? nested.reason : null;
    return nestedReason && nestedReason.trim().length > 0 ? nestedReason.trim() : null;
}

export function isTaskPlanVersionConflict(payload: unknown): boolean {
    const message = taskApiErrorMessage(payload);
    return message === 'already_resolved'
        || message?.startsWith('task_plan_version_mismatch:') === true
        || message?.startsWith('task_plan_already_resolved:') === true;
}

export async function readTaskApiError(response: Response, fallbackMessage: string): Promise<Error> {
    let message = `${fallbackMessage} (${response.status})`;
    try {
        const payload = await response.json();
        const parsed = taskApiErrorMessage(payload);
        if (parsed) {
            message = `${message}: ${parsed}`;
        }
    } catch {
        // best effort only
    }
    return new Error(message);
}

export function normalizeV3TaskStatus(status: unknown): TaskStatus {
    switch (status) {
        case 'pending':
        case 'planning':
        case 'ready':
        case 'running':
        case 'paused':
        case 'completed':
        case 'failed':
        case 'cancelled':
        case 'deferred':
        case 'archived':
            return status;
        case 'waiting_for_user':
        case 'waiting_for_confirmation':
        case 'paused_by_user':
            return 'paused';
        case 'waiting_for_children':
            return 'running';
        case 'sleeping':
            return 'deferred';
        default:
            return 'pending';
    }
}

export function normalizeTaskPlanStatus(status: unknown): TaskPlanStatus | undefined {
    switch (status) {
        case 'planning':
        case 'draft':
        case 'eliciting':
        case 'approved':
        case 'rejected':
        case 'failed':
            return status;
        default:
            return undefined;
    }
}

export function normalizeExecutionPlanStepStatus(status: unknown): PlanStep['status'] {
    switch (status) {
        case 'completed':
        case 'failed':
        case 'skipped':
        case 'cancelled':
        case 'pending':
            return status;
        case 'running':
        case 'executing':
        case 'in_progress':
            return 'in_progress';
        default:
            return 'pending';
    }
}

export function extractPlanStepsFromPlanGraph(planGraph: unknown): PlanStep[] {
    if (!planGraph || typeof planGraph !== 'object') return [];
    const graph = planGraph as {
        steps?: Array<{
            id: string;
            task?: string;
            tool?: string;
            providing_agent_id?: string;
            confidence?: number;
            depends_on?: string[];
            metadata?: { description?: string };
        }>;
        edges?: Array<{ from: string; to: string }>;
    };
    const steps = Array.isArray(graph.steps) ? graph.steps : [];
    if (steps.length === 0) return [];

    const edges = Array.isArray(graph.edges) ? graph.edges : [];
    const edgeDeps = new Map<string, string[]>();
    for (const edge of edges) {
        if (!edge || typeof edge.from !== 'string' || typeof edge.to !== 'string') continue;
        const deps = edgeDeps.get(edge.to) || [];
        deps.push(edge.from);
        edgeDeps.set(edge.to, deps);
    }

    return steps.map((step) => ({
        id: step.id,
        description: step.task || step.metadata?.description || `Step ${step.id}`,
        status: 'pending' as const,
        tool_name: step.tool,
        providing_agent_id: step.providing_agent_id,
        confidence: step.confidence,
        depends_on: step.depends_on?.length ? step.depends_on : (edgeDeps.get(step.id) || []),
    }));
}

export function extractPlanStepsFromExecutionPanel(executionPanel: unknown): PlanStep[] {
    if (!executionPanel || typeof executionPanel !== 'object') return [];
    const selectedExecution = (executionPanel as {
        debug?: {
            selected_execution?: {
                step_statuses?: Array<{
                    number: number;
                    name: string;
                    status: string;
                    step_id?: string | null;
                    capability?: string | null;
                    confidence?: number | null;
                    delegate_agent_id?: string | null;
                }>;
            } | null;
        };
    }).debug?.selected_execution;
    const stepStatuses = Array.isArray(selectedExecution?.step_statuses)
        ? selectedExecution.step_statuses
        : [];
    if (stepStatuses.length === 0) return [];

    return stepStatuses.map((step) => ({
        id: step.step_id || `step-${step.number}`,
        description: step.name || `Step ${step.number}`,
        status: normalizeExecutionPlanStepStatus(step.status),
        tool_name: step.capability || undefined,
        providing_agent_id: step.delegate_agent_id || undefined,
        confidence: typeof step.confidence === 'number' ? step.confidence : undefined,
    }));
}

function mergeExecutionPlanSteps(existing: PlanStep[] | undefined, next: PlanStep[]): PlanStep[] {
    void existing;
    return next;
}

export function firstPendingPlanQuestion(raw: unknown): { id: string; question: string } | undefined {
    const first = Array.isArray(raw) ? raw[0] : raw;
    if (!first || typeof first !== 'object') return undefined;
    const question = first as Record<string, unknown>;
    const id = typeof question.question_id === 'string'
        ? question.question_id
        : typeof question.id === 'string'
            ? question.id
            : '';
    const text = typeof question.question_text === 'string'
        ? question.question_text
        : typeof question.question === 'string'
            ? question.question
            : '';
    if (!id || !text) return undefined;
    return { id, question: text };
}

export function pendingPlanQuestions(raw: unknown): Array<{ id: string; question: string }> {
    if (Array.isArray(raw)) {
        return raw
            .map((entry) => firstPendingPlanQuestion(entry))
            .filter((entry): entry is { id: string; question: string } => Boolean(entry));
    }
    const single = firstPendingPlanQuestion(raw);
    return single ? [single] : [];
}

export function taskHasExecutablePlan(
    task: Pick<Task, 'planStatus' | 'hasPlan' | 'planSteps'> | null | undefined
): boolean {
    if (!task) return false;
    if (task.planStatus) {
        return task.planStatus === 'approved';
    }
    return Boolean(task.hasPlan || (task.planSteps && task.planSteps.length > 0));
}

function taskCanResetDirectExecutionToReady(
    task: Pick<Task, 'executionId' | 'planStatus' | 'hasPlan' | 'planSteps'> | null | undefined
): boolean {
    if (!task?.executionId) return false;
    if (task.planStatus) return false;
    if (task.hasPlan) return false;
    return !task.planSteps || task.planSteps.length === 0;
}

export function taskResetStatus(
    task: Pick<Task, 'executionId' | 'planStatus' | 'hasPlan' | 'planSteps'> | null | undefined
): TaskStatus {
    return taskHasExecutablePlan(task) || taskCanResetDirectExecutionToReady(task)
        ? 'ready'
        : 'pending';
}

export function deriveTaskStatusFromPlan(task: Task, planStatus: TaskPlanStatus | undefined): TaskStatus {
    switch (planStatus) {
        case 'planning':
        case 'eliciting':
            return 'planning';
        case 'draft':
            return 'pending';
        case 'approved':
            return task.approved && !task.isBlocked ? 'ready' : 'pending';
        case 'rejected':
            return 'pending';
        case 'failed':
            return 'failed';
        default:
            return task.status;
    }
}

export function deriveTaskStatusWithoutPlan(task: Task): TaskStatus {
    switch (task.status) {
        case 'running':
        case 'paused':
        case 'completed':
        case 'cancelled':
        case 'deferred':
        case 'archived':
            return task.status;
        case 'failed':
            if (task.executionId) {
                return 'failed';
            }
            return taskCanResetDirectExecutionToReady(task) ? 'ready' : 'pending';
        case 'ready':
            if (!task.planStatus) {
                return task.status;
            }
            return taskCanResetDirectExecutionToReady(task) ? 'ready' : 'pending';
        default:
            return taskCanResetDirectExecutionToReady(task) ? 'ready' : 'pending';
    }
}

export function applyTaskPlanPayload(task: Task, raw: Record<string, unknown>): Task {
    const planStatus = normalizeTaskPlanStatus(raw.status);
    const nextSteps = extractPlanStepsFromPlanGraph(raw.plan_graph);
    const hasPlan = nextSteps.length > 0 || !!raw.plan_graph;
    const pendingQuestions = pendingPlanQuestions(raw.pending_questions);
    const pendingQuestion = pendingQuestions[0];
    const planGeneratedAtRaw = raw.updated_at !== undefined
        ? Date.parse(parseTimestampToIso(raw.updated_at))
        : Number.NaN;
    const nextStatus =
        task.status === 'running' || task.status === 'paused'
            ? task.status
            : (
                task.executionId
                && (
                    task.status === 'failed'
                    || task.status === 'completed'
                    || task.status === 'cancelled'
                )
            )
                ? task.status
            : deriveTaskStatusFromPlan(task, planStatus);
    const nextProgress =
        task.status === 'running' || task.status === 'paused'
            ? task.progress
            : (nextSteps.length > 0 ? 0 : undefined);

    return {
        ...task,
        hasPlan,
        planStatus,
        latestPlanId: typeof raw.plan_id === 'string' ? raw.plan_id : task.latestPlanId,
        planSteps: nextSteps,
        planGeneratedAt: Number.isFinite(planGeneratedAtRaw) ? planGeneratedAtRaw : task.planGeneratedAt,
        pendingQuestion,
        pendingQuestions,
        progress: nextProgress,
        errorMessage:
            planStatus === 'failed'
                ? (typeof raw.error === 'string' && raw.error.trim().length > 0
                    ? raw.error
                    : task.errorMessage || 'Planning failed')
                : (nextStatus === 'failed' ? task.errorMessage : undefined),
        status: nextStatus,
        updatedAt: raw.updated_at !== undefined ? parseTimestampToIso(raw.updated_at) : task.updatedAt
    };
}

export function clearTaskPlanState(task: Task): Task {
    const nextStatus = deriveTaskStatusWithoutPlan(task);
    return {
        ...task,
        hasPlan: false,
        planStatus: undefined,
        latestPlanId: undefined,
        approvedPlanId: undefined,
        planSteps:
            task.status === 'running' || task.status === 'paused'
                ? task.planSteps
                : [],
        planGeneratedAt: undefined,
        pendingQuestion: undefined,
        pendingQuestions: undefined,
        progress:
            task.status === 'running' || task.status === 'paused'
                ? task.progress
                : undefined,
        errorMessage: nextStatus === 'failed' ? task.errorMessage : undefined,
        status: nextStatus
    };
}

export function applyTaskPlanSteps(task: Task, nextSteps: PlanStep[]): Task {
    const mergedSteps = mergeExecutionPlanSteps(task.planSteps, nextSteps);
    const completedSteps = mergedSteps.filter((step) =>
        step.status === 'completed'
        || step.status === 'failed'
        || step.status === 'skipped'
        || step.status === 'cancelled'
    ).length;

    return {
        ...task,
        planSteps: mergedSteps,
        hasPlan: task.hasPlan || mergedSteps.length > 0,
        progress: mergedSteps.length > 0
            ? Math.round((completedSteps / mergedSteps.length) * 100)
            : task.progress,
    };
}

export function parseTaskPlanExecutionGateError(errorCode: string): string | null {
    if (errorCode.startsWith('task_plan_approval_required:')) {
        return 'This plan must be approved before execution.';
    }
    if (errorCode.startsWith('task_plan_not_ready:')) {
        const [, , status] = errorCode.split(':');
        if (status === 'planning') {
            return 'Planning is still in progress for this task.';
        }
        if (status === 'eliciting') {
            return 'This plan is waiting for clarification before it can run.';
        }
        if (status === 'approved') {
            return 'The approved plan is not ready to run yet.';
        }
        return 'This plan is not ready for execution yet.';
    }
    return null;
}

export function parseTimestampToIso(raw: unknown): string {
    if (typeof raw === 'number') return new Date(raw).toISOString();
    if (typeof raw === 'string' && raw.trim()) {
        const millis = Date.parse(raw);
        if (!Number.isNaN(millis)) return new Date(millis).toISOString();
    }
    return new Date().toISOString();
}

function findTaskById(tasks: Task[], taskId: string): Task | undefined {
    return tasks.find((task) => task.id === taskId);
}

function ensureMutableTask(task: Task | undefined, operation: string): Task {
    if (!task) throw new Error('Task not found');
    if (task.readOnly) {
        const message = `Task is read-only in this surface (${operation})`;
        showError(message);
        throw new Error(message);
    }
    return task;
}

function isScopedTaskRefreshEvent(event: V2WebSocketEvent): boolean {
    const scope = get(scopeIdentityStore);

    switch (event.event_type) {
        // V3PlanningClarificationNeeded / V3PlanningClarificationResolved
        // cases retired in H7.3 + H8 — canonical
        // `HitlRequested { source: "clarification" }` is the wire shape.
        case 'V3PlanningStarted':
        case 'V3PlanningProgress':
        case 'V3PlanningCompleted':
        case 'V3PlanningFailed':
            return (
                event.data.principal === scope.principal
                && event.data.workspace === scope.workspace
            );
        case 'TaskCreated':
        case 'TaskUpdated':
        case 'TaskDeleted':
            return (
                event.data.principal === scope.principal
                && event.data.workspace === scope.workspace
            );
        case 'FeedItemCreated':
            return (
                event.data.item.principal === scope.principal
                && event.data.item.workspace === scope.workspace
                && event.data.item.item_type === 'task'
            );
        case 'FeedItemUpdated':
        case 'FeedItemRemoved':
            return (
                event.data.principal === scope.principal
                && event.data.workspace === scope.workspace
                && (
                    (typeof event.data.task_id === 'string' && event.data.task_id.trim().length > 0)
                    || event.data.id.startsWith('task:')
                    || event.data.id.startsWith('v3:task:')
                )
            );
        default:
            return false;
    }
}

/**
 * Today, as `YYYY-MM-DD` in the **reader's** timezone.
 *
 * One derivation, used by the lane predicates, by the grace-period overlay and
 * by the `today=` the list request sends — the server has no idea where the
 * reader is, and a lane computed against a different day than the badge above
 * it is a wrong answer that looks entirely right.
 *
 * Built from the local calendar fields rather than `toISOString()`, which is
 * the bug this replaces: `setHours(0,0,0,0)` then `toISOString()` renders local
 * midnight *in UTC*, so everywhere east of Greenwich it named yesterday for the
 * whole day.
 */
export function readerLocalDate(now: Date = new Date()): string {
    const pad = (value: number) => String(value).padStart(2, '0');
    return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

/**
 * A mirror of the server's lane predicates (`task_lanes.rs`), used **only when
 * there is no server answer to use**.
 *
 * Three callers need it and none of them can ask `GET /v3/tasks?view=`: the
 * warroom's in-flight lanes and the thread-scoped task list both derive lanes
 * over a pool the endpoint cannot describe, and the `/tasks` list itself needs
 * something to render across the round trip a filter switch costs. Where the
 * server HAS answered, `computeFilteredTasks` uses that answer and this is not
 * consulted — see `splitTasksByLane`.
 *
 * Evaluated against stored state only. The grace period is an optimistic-UI
 * concept layered on top by `applyPendingCompletionOverlay`, never folded in
 * here, or the two would have to agree by coincidence.
 */
export function matchesTaskLane(lane: TaskLane, task: Task, today: string): boolean {
    const dueDate = task.dueDate ?? '';
    switch (lane) {
        case 'inbox':
            return task.tags.length === 0 && task.status === 'pending';
        case 'today':
            return dueDate.startsWith(today);
        case 'overdue':
            // An empty due date is absence, not a date before every date.
            return dueDate.length > 0 && dueDate < today && task.status !== 'completed';
        case 'running':
            return task.status === 'running' || task.status === 'paused';
        case 'completed':
            return task.status === 'completed';
        // "All" is the active work queue: every non-completed task, including
        // running/paused/failed/cancelled/deferred. Finished work has its own pill.
        case 'all':
        default:
            return task.status !== 'completed';
    }
}

/**
 * A lane's rows, and the just-ticked tasks its answer leaves out.
 *
 * The server's answer wins when it describes this lane; otherwise the mirror
 * above stands in. `held` is whatever the reader has ticked in the last few
 * seconds that the lane does not contain — the input the overlay needs, and the
 * reason the overlay never has to guess whether a task is already on screen.
 */
export function splitTasksByLane(
    tasks: Task[],
    lane: TaskLane,
    pendingCompletions: ReadonlySet<string>,
    laneAnswer: TaskLaneAnswer | null = null
): { laneRows: Task[]; held: Task[] } {
    const answer = laneAnswer && laneAnswer.view === lane ? laneAnswer : null;
    const today = readerLocalDate();
    const inLane = answer
        ? (task: Task) => answer.ids.has(task.id)
        : (task: Task) => matchesTaskLane(lane, task, today);
    const laneRows: Task[] = [];
    const held: Task[] = [];
    for (const task of tasks) {
        if (inLane(task)) laneRows.push(task);
        else if (pendingCompletions.has(task.id)) held.push(task);
    }
    return { laneRows, held };
}

/**
 * What the grace period changes about ONE lane's membership — **the single
 * definition the rows and the badge above them both go through.**
 *
 * `pendingCompletions` holds tasks the reader just ticked. They linger in the
 * lane they were ticked in instead of vanishing under the cursor, and are held
 * *out* of Completed until the grace period ends. The server cannot see any of
 * that and must not model it: it answers a lane from stored state, and moving
 * the grace period there would make a ticked task disappear instantly, which is
 * the exact thing the grace period exists to prevent.
 *
 * Rows apply `adds`/`removes` to the list; badges apply their *sizes* to the
 * server's count. Two implementations would agree on the day they were written
 * and drift after, and the visible symptom — Inbox reading `5` above six rows —
 * is the defect class this codebase keeps hitting.
 *
 * Only three lanes have a rule:
 * - **inbox** takes back an untagged ticked task the lane no longer lists.
 * - **today** takes back a ticked task still due today. In practice the server's
 *   Today lane is status-blind, so it never dropped the task and `held` is empty
 *   — the rule is here because the answer must not depend on that.
 * - **completed** holds ticked tasks out until the grace period ends.
 *
 * `all`, `overdue` and `running` never had a grace rule and do not get one.
 */
export function pendingCompletionLaneDelta(
    lane: TaskLane,
    laneRows: readonly Task[],
    heldTasks: readonly Task[],
    pendingCompletions: ReadonlySet<string>
): { adds: Task[]; removes: Task[] } {
    // A held task the lane already lists must not be added twice — for rows
    // that would duplicate a row, and for the badge it would count one task
    // twice. Filtering here rather than at each caller is what keeps the two
    // answers equal.
    const listed = new Set(laneRows.map((task) => task.id));
    const held = heldTasks.filter(
        (task) => pendingCompletions.has(task.id) && !listed.has(task.id)
    );

    switch (lane) {
        case 'inbox':
            return { adds: held.filter((task) => task.tags.length === 0), removes: [] };
        case 'today': {
            const today = readerLocalDate();
            return {
                adds: held.filter((task) => task.dueDate?.startsWith(today) ?? false),
                removes: []
            };
        }
        case 'completed':
            return {
                adds: [],
                removes: laneRows.filter((task) => pendingCompletions.has(task.id))
            };
        default:
            return { adds: [], removes: [] };
    }
}

/** The lane's rows as the reader should see them, grace period included. */
export function applyPendingCompletionOverlay(
    lane: TaskLane,
    laneRows: readonly Task[],
    heldTasks: readonly Task[],
    pendingCompletions: ReadonlySet<string>
): Task[] {
    const { adds, removes } = pendingCompletionLaneDelta(
        lane,
        laneRows,
        heldTasks,
        pendingCompletions
    );
    if (adds.length === 0 && removes.length === 0) return [...laneRows];
    const removedIds = new Set(removes.map((task) => task.id));
    return [...laneRows.filter((task) => !removedIds.has(task.id)), ...adds];
}

/**
 * One lane's badge: the server's corpus-wide total, moved by exactly what the
 * overlay moved the rows by.
 *
 * Same `pendingCompletionLaneDelta` as the rows, for the reason spelled out
 * there. The count is the server's because it describes the whole corpus rather
 * than the rows that happen to be loaded — which is the other half of what this
 * endpoint's `counts` exists for.
 */
export function laneCountWithPendingCompletions(
    lane: TaskLane,
    serverCount: number,
    laneRows: readonly Task[],
    heldTasks: readonly Task[],
    pendingCompletions: ReadonlySet<string>
): number {
    const { adds, removes } = pendingCompletionLaneDelta(
        lane,
        laneRows,
        heldTasks,
        pendingCompletions
    );
    return Math.max(0, serverCount + adds.length - removes.length);
}

// Helper to compute filtered tasks
export function computeFilteredTasks(
    tasks: Task[],
    filter: TaskFilter,
    searchQuery: string,
    pendingCompletions: Set<string> = new Set(),
    laneAnswer: TaskLaneAnswer | null = null
): Task[] {
    // Ephemeral chat-spawned tasks live in a separate storage root
    // (`internal_tasks/`) exposed only via `/v3/tasks/internal`, so
    // the persistent `/v3/tasks` feed never carries them — no
    // ephemeral filter needed here.
    let filtered: Task[];

    // Apply filter
    if (typeof filter === 'object' && 'custom' in filter) {
        // Custom tag filter — NOT a lane. `view=` has no tag concept, so this
        // stays exactly as it was: the server is never asked about it.
        // Include tasks in grace period that match the tag
        filtered = tasks.filter(t =>
            t.tags.some(tag => tag.name === filter.custom) ||
            (pendingCompletions.has(t.id) && t.tags.some(tag => tag.name === filter.custom))
        );
    } else {
        const { laneRows, held } = splitTasksByLane(tasks, filter, pendingCompletions, laneAnswer);
        filtered = applyPendingCompletionOverlay(filter, laneRows, held, pendingCompletions);
    }

    // Apply search
    if (searchQuery.trim()) {
        const query = searchQuery.toLowerCase();
        filtered = filtered.filter(t =>
            t.title.toLowerCase().includes(query) ||
            t.tags.some(tag => tag.name.toLowerCase().includes(query))
        );
    }

    return filtered;
}

/**
 * `counts` off the list response, or `null` when the server did not report it.
 *
 * The server emits `counts` only alongside a `today=` (two of the six lanes are
 * date lanes it refuses to guess at) and never on the legacy unpaged branch, so
 * absence is a real state rather than a defensive one. Lanes are read
 * individually and a non-numeric one is dropped: a badge is only ever rendered
 * for a number the server actually sent.
 */
export function readTaskLaneCounts(raw: unknown): Record<string, number> | null {
    if (!raw || typeof raw !== 'object') return null;
    const record = raw as Record<string, unknown>;
    const counts: Record<string, number> = {};
    for (const lane of TASK_LANES) {
        const value = record[lane];
        if (typeof value === 'number' && Number.isFinite(value)) {
            counts[lane] = value;
        }
    }
    return Object.keys(counts).length > 0 ? counts : null;
}

// =============================================================================
// Store Implementation
// =============================================================================

function createTaskStore() {
    const { subscribe, set, update } = writable<TaskStoreState>(defaultState);
    let activeConsumers = 0;
    let realtimeUnsubscribe: (() => void) | null = null;
    let scopeUnsubscribe: (() => void) | null = null;
    let debounceHandle: ReturnType<typeof setTimeout> | null = null;
    let lastObservedEventSequence = 0;
    let lastScopeKey = '';

    function scheduleRefresh(): void {
        if (!browser) return;
        if (debounceHandle) {
            clearTimeout(debounceHandle);
        }
        debounceHandle = setTimeout(() => {
            debounceHandle = null;
            void loadTasksImpl();
        }, TASK_REALTIME_DEBOUNCE_MS);
    }

    function startRealtimeBridge(): void {
        if (realtimeUnsubscribe || !browser) return;
        realtimeUnsubscribe = v2Events.subscribe((events) => {
            if (events.length === 0) return;

            let shouldRefresh = false;
            let nextSequence = lastObservedEventSequence;
            for (const event of events) {
                const sequence = getV2EventSequence(event);
                if (sequence <= lastObservedEventSequence) continue;
                nextSequence = Math.max(nextSequence, sequence);
                if (isScopedTaskRefreshEvent(event)) {
                    shouldRefresh = true;
                }
            }

            if (nextSequence <= lastObservedEventSequence) return;
            lastObservedEventSequence = nextSequence;

            if (shouldRefresh) {
                scheduleRefresh();
            }
        });
    }

    function stopRealtimeBridge(): void {
        if (realtimeUnsubscribe) {
            realtimeUnsubscribe();
            realtimeUnsubscribe = null;
        }
        if (debounceHandle) {
            clearTimeout(debounceHandle);
            debounceHandle = null;
        }
        if (fallbackPollHandle) {
            clearTimeout(fallbackPollHandle);
            fallbackPollHandle = null;
        }
    }

    async function persistTaskStatus(taskId: string, status: TaskStatus): Promise<void> {
        const response = await scopedTaskFetch(`/api/magician/v3/tasks/${encodeURIComponent(taskId)}/status`, {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ status })
        });
        if (!response.ok) {
            throw await readTaskApiError(response, `Failed to update task status to ${status}`);
        }
    }

    function applyLocalTaskStatus(taskId: string, status: TaskStatus): void {
        update(state => {
            const keepsActiveExecution = ['planning', 'running', 'paused'].includes(status);
            const tasks = state.tasks.map(task =>
                task.id === taskId
                    ? {
                        ...task,
                        status,
                        activeExecutionId: keepsActiveExecution ? task.activeExecutionId : undefined,
                        updatedAt: new Date().toISOString()
                    }
                    : task
            );
            const shouldClearExecuting =
                state.executingTask?.taskId === taskId &&
                !keepsActiveExecution;
            return {
                ...state,
                tasks,
                filteredTasks: computeFilteredTasks(tasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer),
                executingTask: shouldClearExecuting ? null : state.executingTask
            };
        });
    }

    async function cancelExecutionImpl(executionId: string): Promise<unknown> {
        return coordinateExecutionControl(executionId, async () => {
            const response = await scopedTaskFetch(
                `/api/magician/v3/executions/${encodeURIComponent(executionId)}/cancel`,
                { method: 'POST' }
            );
            if (!response.ok) {
                throw await readTaskApiError(response, 'Failed to cancel execution');
            }
            return response.json().catch(() => ({}));
        });
    }

    function startScopeBridge(): void {
        if (scopeUnsubscribe || !browser) return;
        const currentScope = get(scopeIdentityStore);
        lastScopeKey = `${currentScope.principal}:${currentScope.workspace}`;
        scopeUnsubscribe = scopeIdentityStore.subscribe((scope) => {
            const scopeKey = `${scope.principal}:${scope.workspace}`;
            if (scopeKey === lastScopeKey) return;
            lastScopeKey = scopeKey;
            taskLoadGeneration += 1;
            for (const timer of completionTimers.values()) {
                clearTimeout(timer);
            }
            completionTimers.clear();
            update((state) => ({
                ...state,
                tasks: [],
                vibedevInternalTasks: [],
                selectedTaskId: null,
                filteredTasks: [],
                executingTask: null,
                isLoading: true,
                error: null,
                pendingCompletions: new Set(),
                // The previous scope's lane answer and counts describe a corpus
                // this scope cannot see. Dropped rather than carried: a badge
                // from someone else's workspace is worse than no badge.
                laneAnswer: null,
                laneCounts: null
            }));
            scheduleRefresh();
        });
    }

    function stopScopeBridge(): void {
        if (scopeUnsubscribe) {
            scopeUnsubscribe();
            scopeUnsubscribe = null;
        }
        lastScopeKey = '';
    }

    // Backstop poll: the realtime bridge is the primary update path, but it is purely
    // event-driven — a terminal `TaskUpdated` dropped on a websocket reconnect can strand
    // a finished/cancelled run's badge at "running". While ANY task is genuinely in-flight,
    // re-load on a low-frequency jittered timer so the served (canonical) terminal status
    // is reconciled even if the event was missed. Self-stops the instant everything settles
    // (no active task → no reschedule), so it never polls an idle board.
    let fallbackPollHandle: ReturnType<typeof setTimeout> | null = null;
    function evaluateFallbackPoll(): void {
        if (fallbackPollHandle) {
            clearTimeout(fallbackPollHandle);
            fallbackPollHandle = null;
        }
        if (!browser) return;
        const active = get({ subscribe }).tasks.some(
            (t) => t.status === 'running' || t.status === 'paused' || t.status === 'planning'
        );
        if (!active) return;
        const delay = 15000 + Math.floor(Math.random() * 5000); // 15–20s, jittered
        fallbackPollHandle = setTimeout(() => {
            fallbackPollHandle = null;
            void loadTasksImpl();
        }, delay);
    }

    // Hoisted to factory scope (was local to loadTasksImpl) so both the
    // list loader and `fetchTaskRecordById` (single by-id fetch used by the
    // chat ExecutionPanel opener for Internal tasks the regular /tasks feed never
    // loads) share ONE conversion and can't drift.
    const convertV3Task = (t: Record<string, unknown>): Task => {
        const pendingQuestions = pendingPlanQuestions(t.pending_questions);
        const pendingQuestion =
            pendingQuestions[0]
            ?? firstPendingPlanQuestion(t.pending_question);

        const planStatus = normalizeTaskPlanStatus(t.plan_status);
        const parsedPlanUpdatedAt =
            typeof t.plan_updated_at === 'string'
                ? Date.parse(parseTimestampToIso(t.plan_updated_at))
                : Number.NaN;
        return {
            id: t.id as string,
            title: t.title as string,
            description: (t.description as string) || '',
            status: normalizeV3TaskStatus(t.status),
            priority: t.priority as TaskPriority | undefined,
            dueDate: t.due_date as string | undefined,
            tags: Array.isArray(t.tags)
                ? (t.tags as unknown[]).flatMap(tag => {
                    if (!tag || typeof tag !== 'object') return [];
                    const record = tag as Record<string, unknown>;
                    const name = typeof record.name === 'string' ? record.name : '';
                    if (!name) return [];
                    return [{
                        name,
                        color: typeof record.color === 'string' ? record.color : ''
                    }];
                })
                : [],
            uiThreadId: (t.ui_thread_id as string | undefined) || 'general',
            agentId: t.agent_id as string | undefined,
            agentName: t.agent_id as string | undefined,
            schedule: extractSchedule(t.schedule),
            createdBy: normalizeCreatedBy(t.created_by),
            outputMode: (t.output_mode as TaskOutputMode | undefined) ?? 'accumulate',
            chatSessionId: typeof t.chat_session_id === 'string' ? t.chat_session_id : undefined,
            // Backend now serializes a single `internal` variant; tolerate
            // legacy `ephemeral_owned_by_chat`/`internal_debug` wire values
            // by collapsing anything non-persistent to `internal`.
            lifecycle: ((t.lifecycle as string | undefined) ?? 'persistent') === 'persistent' ? 'persistent' : 'internal',
            monitorRevision: typeof t.monitor_revision === 'number' ? t.monitor_revision : 0,
            // Compared against `true` rather than cast or coerced, because the
            // server omits the key entirely while it is false: there is usually
            // nothing here to convert, and a cast would spread `undefined` onto
            // every ordinary row. Only an affirmative `true` from the server
            // means "waiting" — a key this client cannot read is not evidence
            // that a human is blocking a run, and the indicator it drives is
            // one no reader should see on a task that is fine.
            awaitingDiffApproval: t.awaiting_diff_approval === true,
            dependsOn: (t.depends_on as string[]) || [],
            approved: (t.approved as boolean) ?? true,
            isBlocked: (t.is_blocked as boolean) ?? false,
            source: 'task' as TaskSource,
            executionId: resolveTaskExecutionId(t),
            activeExecutionId: resolveTaskActiveExecutionId(t),
            hasPlan: Boolean(t.has_plan),
            planStatus,
            latestPlanId: typeof t.latest_plan_id === 'string' ? t.latest_plan_id : undefined,
            approvedPlanId: typeof t.approved_plan_id === 'string' ? t.approved_plan_id : undefined,
            planGeneratedAt: Number.isFinite(parsedPlanUpdatedAt) ? parsedPlanUpdatedAt : undefined,
            pendingQuestion,
            pendingQuestions: pendingQuestions.length > 0 ? pendingQuestions : undefined,
            planSteps: [],
            progress: undefined,
            errorMessage:
                normalizeV3TaskStatus(t.status) === 'failed'
                    ? ((t.completion_outcome as string | undefined)
                        || (planStatus === 'failed' ? 'Planning failed' : 'Execution failed'))
                    : undefined,
            retryAt: undefined,
            completionSummary: typeof t.completion_summary === 'string' ? t.completion_summary : undefined,
            completionOutcome: typeof t.completion_outcome === 'string' ? t.completion_outcome : undefined,
            completionArtifactNames: Array.isArray(t.completion_artifact_names)
                ? (t.completion_artifact_names as unknown[]).filter((n): n is string => typeof n === 'string')
                : undefined,
            synthesisPending: typeof t.synthesis_pending === 'boolean' ? t.synthesis_pending : false,
            synthesisFailedExecutionId: typeof t.synthesis_failed_execution_id === 'string' && t.synthesis_failed_execution_id.length > 0
                ? t.synthesis_failed_execution_id
                : undefined,
            storageBackend: 'v3',
            readOnly: false,
            lastProgressAt: parseLastProgressAt(t.last_progress_at),
            createdAt: parseTimestampToIso(t.created_at),
            updatedAt: parseTimestampToIso(t.updated_at)
        };
    };

    /**
     * A freshly-read task record, with the enrichment the wire does not carry
     * preserved off the record it replaces.
     *
     * **Hoisted out of `loadTasksImpl` so the single-task refresh below shares
     * it**, for the same reason `convertV3Task` was hoisted: a second copy would
     * drift, and the drift would be invisible. `/v3/tasks` reports no plan steps
     * — `convertV3Task` sets `planSteps: []` unconditionally — so a record applied
     * without this merge silently drops the denominator out of `step 4 of 7` and
     * the panel's Run act with it. That is a refresh making the panel *less*
     * informative, which is the failure mode a poll can produce many times a
     * minute without anything looking broken.
     *
     * The plan enrichment is kept only while it still describes this task's
     * current plan or its current run; `executionHistory` is kept whenever there
     * is one, since nothing on the list path ever fetches it.
     */
    function mergeLoadedTask(
        existing: Task | undefined,
        next: Task,
        pendingCompletions: Set<string>
    ): Task {
        let merged = next;
        if (existing) {
            const existingPlanSteps = existing.planSteps?.length ? existing.planSteps : undefined;
            const shouldPreservePlanEnrichment =
                Boolean(existingPlanSteps)
                && (
                    (
                        Boolean(next.hasPlan)
                        && existing.latestPlanId === next.latestPlanId
                        && existing.planGeneratedAt === next.planGeneratedAt
                    )
                    || (
                        (next.status === 'running' || next.status === 'paused')
                        && existing.executionId === next.executionId
                    )
                );
            merged = {
                ...next,
                ...(shouldPreservePlanEnrichment && existingPlanSteps
                    ? {
                        planSteps: existingPlanSteps,
                        hasPlan: existing.hasPlan || next.hasPlan,
                        progress: existing.progress ?? next.progress
                    }
                    : {}),
                ...(existing.executionHistory ? { executionHistory: existing.executionHistory } : {}),
            };
        }
        if (pendingCompletions.has(merged.id)) {
            return { ...merged, status: 'completed' as TaskStatus };
        }
        return merged;
    }

    /**
     * Re-read ONE task and replace it in the list, in place.
     *
     * **Why this exists rather than another `loadTasks()`.** An open task panel
     * polls while its task is live, and the verdict it renders — the status word,
     * the step, the stall clock, the error text — comes off the *task record*
     * rather than off the run payload. Without this the panel would stream live
     * events under a headline frozen at `step 4 of 7`, which looks broken in a new
     * way rather than merely stale. Refreshing the whole list at the panel's
     * cadence would work and costs three things this does not: every task on the
     * page re-converted several times a minute, `isLoading` flickering the list's
     * own controls, and a list-sized response for one row.
     *
     * **It reads the LIST route filtered to one id, not `/v3/tasks/{id}`**, and the
     * difference is load-bearing rather than stylistic. The by-id route serialises
     * the stored `TaskRecord`, which has no plan projection on it at all — no
     * `has_plan`, no `plan_status`, no `latest_plan_id`, no `pending_questions`.
     * Those are computed per row when the list is built. So refreshing a task from
     * it would drop the Plan act, its questions and the ask above them every time
     * the panel polled, and refill them whenever the list next loaded: a flicker
     * with a plausible explanation for every individual frame. The list route
     * answers the same shape it always answers, so the merge below is the one the
     * list itself uses and nothing needs to know which caller it came from.
     *
     * **It replaces and never inserts.** The route resolves internal tasks too, and
     * an upsert would leak one into the `/tasks` list and its filters the first time
     * a chat-owned task's panel was opened. A task the list does not have is a task
     * this cannot refresh, and it says so by answering `false`.
     *
     * It touches neither `isLoading` nor `error` — a background refresh is not the
     * page loading, and a failed one is reported to whoever asked rather than
     * painted over the list.
     */
    async function refreshTaskImpl(taskId: string): Promise<boolean> {
        const fresh = await fetchTaskRowByIdImpl(taskId);
        if (!fresh) return false;
        let applied = false;
        update(state => {
            const existing = state.tasks.find(t => t.id === fresh.id);
            if (!existing) return state;
            applied = true;
            const merged = mergeLoadedTask(existing, fresh, state.pendingCompletions);
            const tasks = state.tasks.map(t => (t.id === merged.id ? merged : t));
            return {
                ...state,
                tasks,
                filteredTasks: computeFilteredTasks(
                    tasks,
                    state.filter,
                    state.searchQuery,
                    state.pendingCompletions,
                    state.laneAnswer
                )
            };
        });
        return applied;
    }

    /**
     * One task, as the **list** describes it — every field a list row carries,
     * including the plan projection the stored record has none of.
     *
     * `query` matches the id (among title, agent and status), so this is the list
     * request with a filter rather than a second endpoint; `limit=1` keeps it to
     * the row asked for. The id is checked on the way back because the filter also
     * matches titles, and a task whose title contains another task's id would
     * otherwise answer for it.
     */
    async function fetchTaskRowByIdImpl(taskId: string): Promise<Task | null> {
        try {
            const response = await scopedTaskFetch(
                `/api/magician/v3/tasks?query=${encodeURIComponent(taskId)}&limit=1`
            );
            if (!response.ok) return null;
            const data = await response.json();
            const rows = Array.isArray(data?.tasks) ? data.tasks : [];
            for (const row of rows) {
                if (!row || typeof row !== 'object') continue;
                const task = convertV3Task(row as Record<string, unknown>);
                if (task.id === taskId) return task;
            }
            return null;
        } catch {
            // A background refresh. The panel that asked reports its own staleness;
            // the list keeps the row it already has.
            return null;
        }
    }

    async function fetchTaskRecordByIdImpl(taskId: string): Promise<Task | null> {
        try {
            const response = await scopedTaskFetch(
                `/api/magician/v3/tasks/${encodeURIComponent(taskId)}`
            );
            if (!response.ok) return null;
            const data = await response.json();
            if (!data?.task || typeof data.task !== 'object') return null;
            return convertV3Task(flattenV3TaskRecord(data.task as Record<string, unknown>));
        } catch (error) {
            console.error('fetchTaskRecordById failed:', error);
            return null;
        }
    }

    /**
     * Ask the server which tasks are in a lane, and what every lane's total is.
     *
     * **`limit`/`offset` are what reach the lane at all.** `GET /v3/tasks`
     * answers a bare `{tasks}` when both are absent — the pre-pagination shape,
     * which predates `view=` and carries neither a lane nor `counts`. So this
     * request is paged, with a page big enough to hold the corpus: Phase 0
     * moves the lane to the server without changing how much is loaded, and the
     * real page comes in Phase 1.
     *
     * It is a *second* request rather than the list load's own, because the
     * list load fills `tasks`, which is the app's task CORPUS — the mention
     * picker, the command palette, the thread task list and the vibe cockpit
     * all read it whole. Narrowing it to `/tasks`' current lane would silently
     * shrink every one of them.
     *
     * A tag filter sends no `view` (tags are not a lane) but still sends
     * `today`, because the badges are wanted either way.
     */
    async function fetchTaskLaneImpl(
        filter: TaskFilter
    ): Promise<{ answer: TaskLaneAnswer | null; counts: Record<string, number> | null } | null> {
        const view = typeof filter === 'string' ? filter : null;
        const params = new URLSearchParams({
            limit: String(TASK_LANE_PAGE_LIMIT),
            offset: '0',
            today: readerLocalDate()
        });
        if (view) params.set('view', view);
        try {
            const response = await scopedTaskFetch(`/api/magician/v3/tasks?${params.toString()}`);
            if (!response.ok) return null;
            const data = await response.json();
            const rows = Array.isArray(data?.tasks) ? data.tasks : [];
            const ids = new Set<string>();
            for (const row of rows) {
                if (!row || typeof row !== 'object') continue;
                const id = (row as Record<string, unknown>).id;
                if (typeof id === 'string' && id.length > 0) ids.add(id);
            }
            return {
                answer: view ? { view, ids } : null,
                counts: readTaskLaneCounts(data?.counts)
            };
        } catch {
            // No answer is a state the list already handles: membership falls
            // back to the mirror and the badges render nothing rather than a
            // number nobody stands behind.
            return null;
        }
    }

    /**
     * Re-ask for the lane alone — what a filter switch costs.
     *
     * The list keeps rendering while this is in flight (the mirror covers the
     * round trip), so there is no blank frame and no window where the pill and
     * the rows disagree.
     */
    async function refreshTaskLaneImpl(): Promise<void> {
        if (!browser) return;
        const { generation, scopeKey } = nextTaskLoadToken();
        const lane = await fetchTaskLaneImpl(get({ subscribe }).filter);
        if (isStaleTaskLoad(generation, scopeKey)) return;
        update(state => {
            const laneAnswer = lane?.answer ?? null;
            const laneCounts = lane?.counts ?? null;
            return {
                ...state,
                laneAnswer,
                laneCounts,
                filteredTasks: computeFilteredTasks(
                    state.tasks,
                    state.filter,
                    state.searchQuery,
                    state.pendingCompletions,
                    laneAnswer
                )
            };
        });
    }

    async function loadTasksImpl() {
        const { generation, scopeKey } = nextTaskLoadToken();
        update(state => ({ ...state, isLoading: true, error: null }));
        // Started first and awaited below: the lane and the corpus describe the
        // same moment, and applying them in one update leaves no frame where
        // the list is filtered by one and counted by the other.
        const lanePromise = fetchTaskLaneImpl(get({ subscribe }).filter);

        try {
            let tasks: Task[] = [];
            try {
                const v3Response = await scopedTaskFetch('/api/magician/v3/tasks');
                if (v3Response.ok) {
                    const v3Result = await v3Response.json();
                    const rawV3 = Array.isArray(v3Result.tasks) ? v3Result.tasks : [];
                    for (const rawTask of rawV3) {
                        if (!rawTask || typeof rawTask !== 'object') continue;
                        const task = convertV3Task(rawTask as Record<string, unknown>);
                        tasks.push(task);
                    }
                }
            } catch {
                // V3 task endpoint may not exist yet, that's ok
            }

            tasks.sort((a, b) =>
                new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime()
            );

            // VibeDev runs default to `Internal` (off the persistent `/tasks` feed),
            // so the cockpit run-history rail loads them separately, filtered to its
            // own thread. Kept OUT of `tasks` so the global `/tasks` list stays clean.
            let vibedevInternalTasks: Task[] = [];
            try {
                const internalResponse = await scopedTaskFetch(
                    '/api/magician/v3/tasks/internal?ui_thread_id=vibedev&limit=200&sort=updated_at&order=desc'
                );
                if (internalResponse.ok) {
                    const internalResult = await internalResponse.json();
                    const rawInternal = Array.isArray(internalResult.tasks) ? internalResult.tasks : [];
                    for (const rawTask of rawInternal) {
                        if (!rawTask || typeof rawTask !== 'object') continue;
                        vibedevInternalTasks.push(convertV3Task(rawTask as Record<string, unknown>));
                    }
                }
            } catch {
                // Internal feed optional — cockpit still works from the selected run.
            }

            const lane = await lanePromise;

            if (isStaleTaskLoad(generation, scopeKey)) {
                return;
            }

            update(state => {
                const laneAnswer = lane?.answer ?? null;
                const laneCounts = lane?.counts ?? null;
                const existingById = new Map(state.tasks.map(t => [t.id, t]));
                const mergedTasks = tasks.map(t =>
                    mergeLoadedTask(existingById.get(t.id), t, state.pendingCompletions)
                );

                const refreshedExecutingTask = (() => {
                    if (!state.executingTask) return null;
                    const liveTask = mergedTasks.find(task => task.id === state.executingTask?.taskId);
                    if (!liveTask) return null;
                    if (!['running', 'planning', 'paused'].includes(liveTask.status)) {
                        return null;
                    }
                    if (
                        liveTask.executionId
                        && liveTask.executionId !== state.executingTask.executionId
                    ) {
                        return {
                            ...state.executingTask,
                            executionId: liveTask.executionId
                        };
                    }
                    return state.executingTask;
                })();

                return {
                    ...state,
                    tasks: mergedTasks,
                    vibedevInternalTasks,
                    laneAnswer,
                    laneCounts,
                    filteredTasks: computeFilteredTasks(mergedTasks, state.filter, state.searchQuery, state.pendingCompletions, laneAnswer),
                    executingTask: refreshedExecutingTask,
                    isLoading: false
                };
            });
        } catch (error) {
            if (isStaleTaskLoad(generation, scopeKey)) {
                return;
            }
            update(state => ({
                ...state,
                isLoading: false,
                error: error instanceof Error ? error.message : 'Failed to load tasks'
            }));
        }
        // (Re)arm the backstop poll based on the just-loaded statuses: keeps polling while a
        // run is in-flight, stops once everything is terminal. Runs after success OR a
        // non-stale error (stale loads return early; the newer load re-arms it).
        evaluateFallbackPoll();
    }

    async function refreshTaskPlanIntoStore(taskId: string): Promise<Record<string, unknown> | null> {
        const { generation, scopeKey } = nextTaskLoadToken();
        const response = await scopedTaskFetch(`/api/magician/v3/tasks/${taskId}/plan`);
        if (!response.ok) {
            const errBody = await response.json().catch(() => ({}));
            const errorCode = typeof errBody?.error === 'string' ? errBody.error : '';
            if (errorCode.startsWith(`task_plan_not_found:${taskId}`)) {
                if (isStaleTaskLoad(generation, scopeKey)) {
                    return null;
                }
                update((state) => {
                    const tasks = state.tasks.map((task) =>
                        task.id === taskId ? clearTaskPlanState(task) : task
                    );
                    return {
                        ...state,
                        tasks,
                        filteredTasks: computeFilteredTasks(
                            tasks,
                            state.filter,
                            state.searchQuery,
                            state.pendingCompletions,
                            state.laneAnswer
                        )
                    };
                });
                return null;
            }
            throw new Error(errorCode || `Failed to load task plan (HTTP ${response.status})`);
        }

        const result = await response.json().catch(() => null);
        const plan = result?.plan && typeof result.plan === 'object'
            ? result.plan as Record<string, unknown>
            : null;
        if (!plan || isStaleTaskLoad(generation, scopeKey)) return plan;

        update((state) => {
            const tasks = state.tasks.map((task) =>
                task.id === taskId ? applyTaskPlanPayload(task, plan) : task
            );
            return {
                ...state,
                tasks,
                filteredTasks: computeFilteredTasks(
                    tasks,
                    state.filter,
                    state.searchQuery,
                    state.pendingCompletions,
                    state.laneAnswer
                )
            };
        });

        return plan;
    }

    async function reconcileTaskPlanConflict(taskId: string, scopeKey: string): Promise<void> {
        await loadTasksImpl();
        if (currentTaskScopeKey() !== scopeKey) return;
        await refreshTaskPlanIntoStore(taskId).catch(() => null);
    }

    async function refreshExecutionPlanStepsIntoStore(
        taskId: string,
        executionId: string
    ): Promise<void> {
        const { generation, scopeKey } = nextTaskLoadToken();
        const response = await scopedTaskFetch(
            `/api/magician/v3/tasks/${taskId}/execution-panel?execution_id=${encodeURIComponent(executionId)}`
        );
        if (!response.ok) {
            return;
        }

        const panelState = await response.json().catch(() => null);
        const nextSteps = extractPlanStepsFromExecutionPanel(panelState);
        if (nextSteps.length === 0 || isStaleTaskLoad(generation, scopeKey)) {
            return;
        }

        update((state) => {
            const tasks = state.tasks.map((task) =>
                task.id === taskId ? applyTaskPlanSteps(task, nextSteps) : task
            );
            return {
                ...state,
                tasks,
                filteredTasks: computeFilteredTasks(
                    tasks,
                    state.filter,
                    state.searchQuery,
                    state.pendingCompletions,
                    state.laneAnswer
                ),
            };
        });
    }

    return {
        subscribe,

        // =================================================================
        // Task CRUD Operations
        // =================================================================

        /**
         * Create a new task (no execution triggered)
         *
         * `agent_id` is compulsory for the new architecture -- the Personal agent
         * that will own this task.  `schedule` is optional (cron + timezone).
         */
        createTask: async (title: string, description: string, options?: {
            priority?: TaskPriority;
            dueDate?: string;
            tags?: TaskTag[];
            agentId?: string;
            agentName?: string;
            schedule?: TaskSchedule;
            createdBy?: TaskCreatedBy;
            outputMode?: TaskOutputMode;
            uiThreadId?: string;
            dependsOn?: string[];  // task IDs from @ mentions
            referenceTaskIds?: string[]; // completed task ids used as continuation context
            saveAsTask?: boolean; // VibeDev: promote the run to a user-visible (Persistent) task; default internal
        }) => {
            const { generation, scopeKey } = nextTaskLoadToken();
            // Optimistically create a local task with temporary ID
            const tempId = crypto.randomUUID();
            const now = new Date().toISOString();
            const optimisticTask: Task = {
                id: tempId,
                title: title.trim(),
                description,
                status: 'pending',
                uiThreadId: options?.uiThreadId || 'general',
                priority: options?.priority,
                dueDate: options?.dueDate,
                tags: options?.tags || [],
                agentId: options?.agentId,
                agentName: options?.agentName,
                schedule: options?.schedule,
                createdBy: options?.createdBy ?? 'user',
                outputMode: options?.outputMode ?? 'accumulate',
                dependsOn: options?.dependsOn || [],
                source: 'task', // Manually created tasks
                createdAt: now,
                updatedAt: now
            };

            // Add to local state immediately for responsive UI
            update(state => {
                const newTasks = [optimisticTask, ...state.tasks];
                return {
                    ...state,
                    tasks: newTasks,
                    filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
                };
            });

            // Persist to backend with snake_case field names
            try {
                const apiRequest: Record<string, unknown> = {
                    title: title.trim(),
                    description,
                    ui_thread_id: options?.uiThreadId || 'general',
                    priority: options?.priority,
                    due_date: options?.dueDate,
                    tags: options?.tags?.map(tag => ({
                        id: tag.name,
                        name: tag.name,
                        color: tag.color || null
                    })) || [],
                    created_by: options?.createdBy === 'autonomous' ? 'agent' : (options?.createdBy ?? 'user'),
                    output_mode: options?.outputMode ?? 'accumulate'
                };

                // agent_id is compulsory — every task must be owned by a Personal agent
                if (!options?.agentId) {
                    throw new Error('agent_id is required: every task must be owned by a Personal agent');
                }
                apiRequest.agent_id = options.agentId;

                // Include depends_on when provided — task IDs from @ mentions
                if (options?.dependsOn?.length) {
                    apiRequest.depends_on = options.dependsOn;
                }

                // Include completed source task ids when a caller wants the
                // backend continuation/artifact context without dependency-gating
                // the new task. The API validates that these ids are completed.
                if (options?.referenceTaskIds?.length) {
                    apiRequest.reference_task_ids = options.referenceTaskIds;
                }

                // Include schedule when provided — serialize to backend's externally-tagged enum shape
                if (options?.schedule?.cron) {
                    apiRequest.schedule = serializeScheduleForApi(options.schedule);
                }

                // VibeDev intent gate: when true the backend creates a user-visible
                // (Persistent) task; otherwise a VibeDev run is created Internal.
                if (options?.saveAsTask) {
                    apiRequest.save_as_task = true;
                }

                const response = await scopedTaskFetch('/api/magician/v3/tasks', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify(apiRequest)
                });

                if (response.ok) {
                    const data = await response.json();
                    const backendTask = flattenV3TaskRecord(data.task as Record<string, unknown>);
                    const backendTaskId = typeof backendTask.id === 'string' ? backendTask.id : tempId;
                    const backendTaskTitle = typeof backendTask.title === 'string' ? backendTask.title : title.trim();
                    const backendTaskDescription =
                        typeof backendTask.description === 'string' ? backendTask.description : description;
                    const backendTaskUiThreadId =
                        typeof backendTask.ui_thread_id === 'string'
                            ? backendTask.ui_thread_id
                            : (options?.uiThreadId || 'general');
                    const backendTaskAgentId =
                        typeof backendTask.agent_id === 'string' ? backendTask.agent_id : options?.agentId;
                    const backendTaskAgentName =
                        typeof backendTask.agent_name === 'string' ? backendTask.agent_name : options?.agentName;
                    const createdTask: Task = {
                        id: backendTaskId,
                        title: backendTaskTitle,
                        description: backendTaskDescription,
                        status: normalizeV3TaskStatus(backendTask.status),
                        uiThreadId: backendTaskUiThreadId,
                        priority: backendTask.priority as TaskPriority | undefined,
                        dueDate: backendTask.due_date as string | undefined,
                        tags: Array.isArray(backendTask.tags)
                            ? (backendTask.tags as unknown[]).flatMap(tag => {
                                if (!tag || typeof tag !== 'object') return [];
                                const record = tag as Record<string, unknown>;
                                const name = typeof record.name === 'string' ? record.name : '';
                                if (!name) return [];
                                return [{ name, color: typeof record.color === 'string' ? record.color : '' }];
                            })
                            : [],
                        agentId: backendTaskAgentId,
                        agentName: backendTaskAgentName,
                        schedule: extractSchedule(backendTask.schedule) || options?.schedule,
                        createdBy: normalizeCreatedBy(backendTask.created_by ?? options?.createdBy),
                        outputMode: (backendTask.output_mode as TaskOutputMode | undefined) ?? options?.outputMode ?? 'accumulate',
                        dependsOn: (backendTask.depends_on as string[]) || [],
                        approved: (backendTask.approved as boolean) ?? true,
                        isBlocked: (backendTask.is_blocked as boolean) ?? false,
                        source: 'task',
                        executionId: resolveTaskExecutionId(backendTask),
                        activeExecutionId: resolveTaskActiveExecutionId(backendTask),
                        hasPlan: false,
                        planSteps: [],
                        progress: undefined,
                        errorMessage: undefined,
                        retryAt: undefined,
                        storageBackend: 'v3',
                        readOnly: false,
                        createdAt: backendTask.created_at ? parseTimestampToIso(backendTask.created_at) : now,
                        updatedAt: backendTask.updated_at ? parseTimestampToIso(backendTask.updated_at) : now
                    };
                    if (isStaleTaskLoad(generation, scopeKey)) {
                        return createdTask;
                    }

                    // Update local state with backend's ID and data
                    // Map all fields consistently with convertTask to avoid
                    // undefined values in the UI until next loadTasks().
                    update(state => {
                        const newTasks = state.tasks.map(t =>
                            t.id === tempId
                                ? createdTask
                                : t
                        );
                        return {
                            ...state,
                            tasks: newTasks,
                            filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
                        };
                    });

                    return createdTask;
                } else {
                    const errBody = await response.json().catch(() => ({}));
                    console.error('Failed to create task:', response.status, errBody);
                    // Remove the optimistic task on failure
                    if (!isStaleTaskLoad(generation, scopeKey)) {
                        update(state => {
                            const newTasks = state.tasks.filter(t => t.id !== tempId);
                            return {
                                ...state,
                                tasks: newTasks,
                                filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
                            };
                        });
                    }
                    throw new Error(errBody.error || `Failed to create task (HTTP ${response.status})`);
                }
            } catch (error) {
                console.error('Failed to persist task:', error);
                // Remove the optimistic task on error
                if (!isStaleTaskLoad(generation, scopeKey)) {
                    update(state => {
                        const newTasks = state.tasks.filter(t => t.id !== tempId);
                        return {
                            ...state,
                            tasks: newTasks,
                            filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
                        };
                    });
                }
                throw error;
            }
        },

        /**
         * Update task properties
         * All items are now source: 'task' after load-time migration
         */
        updateTask: async (
            taskId: string,
            updates: Partial<{
                title: string;
                description: string;
                uiThreadId: string;
                priority: TaskPriority | null;
                dueDate: string | null;
                tags: TaskTag[];
                schedule: TaskSchedule | null;
                outputMode: TaskOutputMode | null;
            }>
        ) => {
            const { generation, scopeKey } = nextTaskLoadToken();
            ensureMutableTask(findTaskById(get({ subscribe }).tasks, taskId), 'update');
            const normalizedUpdates: Partial<Pick<Task, 'title' | 'description' | 'uiThreadId' | 'priority' | 'dueDate' | 'tags' | 'schedule' | 'outputMode'>> = {};
            if (updates.title !== undefined) normalizedUpdates.title = updates.title;
            if (updates.description !== undefined) normalizedUpdates.description = updates.description;
            if (updates.uiThreadId !== undefined) normalizedUpdates.uiThreadId = updates.uiThreadId;
            if (updates.priority !== undefined) normalizedUpdates.priority = updates.priority ?? undefined;
            if (updates.dueDate !== undefined) normalizedUpdates.dueDate = updates.dueDate ?? undefined;
            if (updates.tags !== undefined) normalizedUpdates.tags = updates.tags;
            if (updates.schedule !== undefined) normalizedUpdates.schedule = updates.schedule ?? undefined;
            if (updates.outputMode !== undefined) normalizedUpdates.outputMode = updates.outputMode ?? undefined;

            // Update local state
            update(state => {
                const newTasks = state.tasks.map(t =>
                    t.id === taskId
                        ? { ...t, ...normalizedUpdates, updatedAt: new Date().toISOString() }
                        : t
                );
                return {
                    ...state,
                    tasks: newTasks,
                    filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
                };
            });

            // Persist to backend (convert camelCase to snake_case for API)
            try {
                const apiUpdates: Record<string, unknown> = {};
                if (updates.title !== undefined) apiUpdates.title = updates.title;
                if (updates.description !== undefined) apiUpdates.description = updates.description;
                if (updates.uiThreadId !== undefined) apiUpdates.ui_thread_id = updates.uiThreadId;
                if (updates.priority !== undefined) apiUpdates.priority = updates.priority;
                if (updates.dueDate !== undefined) apiUpdates.due_date = updates.dueDate;
                if (updates.tags !== undefined) {
                    apiUpdates.tags = updates.tags.map(tag => ({
                        id: tag.name,
                        name: tag.name,
                        color: tag.color || null
                    }));
                }
                if (updates.schedule !== undefined) {
                    apiUpdates.schedule = updates.schedule === null ? null : serializeScheduleForApi(updates.schedule);
                }
                if (updates.outputMode !== undefined) {
                    apiUpdates.output_mode = updates.outputMode;
                }

                const response = await scopedTaskFetch(`/api/magician/v3/tasks/${taskId}`, {
                    method: 'PUT',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify(apiUpdates)
                });
                if (isStaleTaskLoad(generation, scopeKey)) {
                    return;
                }

                if (!response.ok) {
                    console.error(`Failed to update task: ${response.status} ${response.statusText}`);
                    // Rollback: reload tasks from server to restore correct state
                    await loadTasksImpl();
                    showError('Failed to update task');
                }
            } catch (error) {
                console.error('Failed to update task:', error);
                if (isStaleTaskLoad(generation, scopeKey)) {
                    return;
                }
                // Rollback: reload tasks from server to restore correct state
                await loadTasksImpl();
                showError('Failed to update task');
            }
        },

        /**
         * Delete a task
         * All items are now source: 'task' after load-time migration
         */
        deleteTask: async (taskId: string, options: DeleteTaskOptions = {}) => {
            const { generation, scopeKey } = nextTaskLoadToken();
            const currentState = get({ subscribe });
            const task = findTaskById(currentState.tasks, taskId);
            ensureMutableTask(task, 'delete');
            const removedTask = task ?? null;
            const removedIndex = currentState.tasks.findIndex((entry) => entry.id === taskId);
            const wasPendingCompletion = currentState.pendingCompletions.has(taskId);
            const wasSelected = currentState.selectedTaskId === taskId;
            // Clear any completion timer if exists
            const existingTimer = completionTimers.get(taskId);
            if (existingTimer) {
                clearTimeout(existingTimer);
                completionTimers.delete(taskId);
            }

            update(state => {
                const newTasks = state.tasks.filter(t => t.id !== taskId);
                const newPendingCompletions = new Set(state.pendingCompletions);
                newPendingCompletions.delete(taskId);

                return {
                    ...state,
                    tasks: newTasks,
                    pendingCompletions: newPendingCompletions,
                    filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, newPendingCompletions, state.laneAnswer),
                    selectedTaskId: state.selectedTaskId === taskId ? null : state.selectedTaskId
                };
            });

            // Delete from tasks endpoint
            try {
                const params = new URLSearchParams();
                if (options.removeFiles !== undefined) {
                    params.set('remove_files', options.removeFiles ? 'true' : 'false');
                }
                const query = params.toString();
                const url = `/api/magician/v3/tasks/${encodeURIComponent(taskId)}${query ? `?${query}` : ''}`;
                const response = await scopedTaskFetch(url, {
                    method: 'DELETE'
                });
                if (!response.ok) {
                    throw await readTaskApiError(response, 'Failed to delete task');
                }
            } catch (error) {
                console.error('Failed to delete task:', error);
                if (removedTask && !isStaleTaskLoad(generation, scopeKey)) {
                    update(state => {
                        const alreadyPresent = state.tasks.some((entry) => entry.id === taskId);
                        const restoredTasks = alreadyPresent
                            ? state.tasks
                            : [
                                ...state.tasks.slice(0, Math.max(0, removedIndex)),
                                removedTask,
                                ...state.tasks.slice(Math.max(0, removedIndex))
                            ];
                        const restoredPendingCompletions = new Set(state.pendingCompletions);
                        if (wasPendingCompletion) {
                            restoredPendingCompletions.add(taskId);
                        }
                        return {
                            ...state,
                            tasks: restoredTasks,
                            pendingCompletions: restoredPendingCompletions,
                            filteredTasks: computeFilteredTasks(
                                restoredTasks,
                                state.filter,
                                state.searchQuery,
                                restoredPendingCompletions,
                                state.laneAnswer
                            ),
                            selectedTaskId: state.selectedTaskId ?? (wasSelected ? taskId : null)
                        };
                    });
                }
                throw error;
            }
        },

        /**
         * Mark task as completed with 5-second grace period
         * During grace period, task stays visible in current view
         * After grace period, task moves to "Completed" view
         * All items are now source: 'task' after load-time migration
         */
        completeTask: async (taskId: string) => {
            const { generation, scopeKey } = nextTaskLoadToken();
            ensureMutableTask(findTaskById(get({ subscribe }).tasks, taskId), 'complete');
            // Clear any existing timer for this task
            const existingTimer = completionTimers.get(taskId);
            if (existingTimer) {
                clearTimeout(existingTimer);
                completionTimers.delete(taskId);
            }

            // Capture previous status for rollback
            const currentState = get({ subscribe });
            const prevTask = currentState.tasks.find(t => t.id === taskId);
            const prevStatus = prevTask?.status ?? ('pending' as TaskStatus);

            // Mark as completed and add to pending completions
            update(state => {
                const newPendingCompletions = new Set(state.pendingCompletions);
                newPendingCompletions.add(taskId);

                const newTasks = state.tasks.map(t =>
                    t.id === taskId
                        ? { ...t, status: 'completed' as TaskStatus, updatedAt: new Date().toISOString() }
                        : t
                );
                return {
                    ...state,
                    tasks: newTasks,
                    pendingCompletions: newPendingCompletions,
                    filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, newPendingCompletions, state.laneAnswer)
                };
            });

            // Start grace period timer
            const timer = setTimeout(async () => {
                completionTimers.delete(taskId);

                // Remove from pending completions after grace period
                update(state => {
                    const newPendingCompletions = new Set(state.pendingCompletions);
                    newPendingCompletions.delete(taskId);

                    return {
                        ...state,
                        pendingCompletions: newPendingCompletions,
                        filteredTasks: computeFilteredTasks(state.tasks, state.filter, state.searchQuery, newPendingCompletions, state.laneAnswer)
                    };
                });

                // Persist to backend
                try {
                    const response = await scopedTaskFetch(`/api/magician/v3/tasks/${taskId}/status`, {
                        method: 'PUT',
                        headers: { 'Content-Type': 'application/json' },
                        body: JSON.stringify({ status: 'completed' })
                    });
                    if (!response.ok) {
                        throw new Error(`${response.status}`);
                    }
                    // Sync actual status from backend (scheduled tasks reset to Ready)
                    try {
                        const data = await response.json();
                        const flattened = data?.task && typeof data.task === 'object'
                            ? flattenV3TaskRecord(data.task as Record<string, unknown>)
                            : null;
                        if (
                            flattened?.status
                            && flattened.status !== 'completed'
                            && !isStaleTaskLoad(generation, scopeKey)
                        ) {
                            update(state => {
                                const newTasks = state.tasks.map(t =>
                                    t.id === taskId
                                        ? { ...t, status: normalizeV3TaskStatus(flattened.status) }
                                        : t
                                );
                                return {
                                    ...state,
                                    tasks: newTasks,
                                    filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
                                };
                            });
                        }
                    } catch { /* non-JSON response — status update succeeded, next poll will sync */ }
                } catch (error) {
                    console.error('Failed to persist task completion:', error);
                    if (isStaleTaskLoad(generation, scopeKey)) {
                        return;
                    }
                    // Rollback optimistic update
                    update(state => {
                        const newTasks = state.tasks.map(t =>
                            t.id === taskId
                                ? { ...t, status: prevStatus, updatedAt: new Date().toISOString() }
                                : t
                        );
                        return {
                            ...state,
                            tasks: newTasks,
                            filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
                        };
                    });
                    showError('Failed to complete task');
                }
            }, COMPLETION_GRACE_PERIOD_MS);

            completionTimers.set(taskId, timer);
        },

        /**
         * Uncomplete a task (revert from completed status)
         * If within grace period, cancels the pending completion
         * All items are now source: 'task' after load-time migration
         */
        uncompleteTask: async (taskId: string) => {
            const { generation, scopeKey } = nextTaskLoadToken();
            ensureMutableTask(findTaskById(get({ subscribe }).tasks, taskId), 'uncomplete');
            const existingTimer = completionTimers.get(taskId);
            const wasInGracePeriod = !!existingTimer;
            if (existingTimer) {
                clearTimeout(existingTimer);
                completionTimers.delete(taskId);
            }

            // Determine target status based on whether the latest plan is executable
            const currentState = get({ subscribe });
            const task = currentState.tasks.find(t => t.id === taskId);
            const targetStatus = taskResetStatus(task);

            // Optimistic update
            update(state => {
                const newPendingCompletions = new Set(state.pendingCompletions);
                newPendingCompletions.delete(taskId);

                const newTasks = state.tasks.map(t =>
                    t.id === taskId
                        ? { ...t, status: targetStatus, updatedAt: new Date().toISOString() }
                        : t
                );
                return {
                    ...state,
                    tasks: newTasks,
                    pendingCompletions: newPendingCompletions,
                    filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, newPendingCompletions, state.laneAnswer)
                };
            });

            // Only persist if completion had already been sent to backend
            if (!wasInGracePeriod) {
                try {
                    const response = await scopedTaskFetch(`/api/magician/v3/tasks/${taskId}/status`, {
                        method: 'PUT',
                        headers: { 'Content-Type': 'application/json' },
                        body: JSON.stringify({ status: targetStatus })
                    });
                    if (!response.ok) {
                        throw new Error(`${response.status}`);
                    }
                } catch (error) {
                    console.error('Failed to uncomplete task:', error);
                    if (isStaleTaskLoad(generation, scopeKey)) {
                        return;
                    }
                    // Rollback to completed
                    update(state => {
                        const newTasks = state.tasks.map(t =>
                            t.id === taskId
                                ? { ...t, status: 'completed' as TaskStatus, updatedAt: new Date().toISOString() }
                                : t
                        );
                        return {
                            ...state,
                            tasks: newTasks,
                            filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
                        };
                    });
                    showError('Failed to uncomplete task');
                }
            }
        },

        /**
         * Check if a task is in the grace period
         */
        isInGracePeriod: (taskId: string): boolean => {
            return completionTimers.has(taskId);
        },

        /**
         * Update task status (generic status update)
         */
        updateTaskStatus: async (taskId: string, status: TaskStatus) => {
            ensureMutableTask(findTaskById(get({ subscribe }).tasks, taskId), 'status_update');
            await persistTaskStatus(taskId, status);
            applyLocalTaskStatus(taskId, status);
        },

        /**
         * Stop the active execution before moving the task back to its runnable state.
         * If status persistence fails after cancellation, reload the authoritative task
         * instead of leaving an optimistic local status behind.
         */
        abortTask: async (taskId: string): Promise<TaskStatus> => {
            const { generation, scopeKey } = nextTaskLoadToken();
            const task = ensureMutableTask(
                findTaskById(get({ subscribe }).tasks, taskId)
                    ?? await fetchTaskRecordByIdImpl(taskId)
                    ?? undefined,
                'status_update'
            );
            const executionId = task.activeExecutionId?.trim();
            if (!executionId) {
                throw new Error('Task has no active execution.');
            }
            const nextStatus = taskResetStatus(task);

            await cancelExecutionImpl(executionId);
            try {
                await persistTaskStatus(taskId, nextStatus);
            } catch (error) {
                if (!isStaleTaskLoad(generation, scopeKey)) {
                    await loadTasksImpl().catch(() => undefined);
                }
                const reason = error instanceof Error ? error.message : 'unknown status error';
                throw new Error(`Execution stopped, but the task status could not be updated: ${reason}`);
            }
            if (!isStaleTaskLoad(generation, scopeKey)) {
                applyLocalTaskStatus(taskId, nextStatus);
            }
            return nextStatus;
        },

        resetTaskToReady: async (taskId: string): Promise<TaskStatus> => {
            const { generation, scopeKey } = nextTaskLoadToken();
            const taskInList = findTaskById(get({ subscribe }).tasks, taskId);
            const task = taskInList ?? await fetchTaskRecordByIdImpl(taskId) ?? undefined;
            ensureMutableTask(task, 'status_update');

            const response = await scopedTaskFetch(`/api/magician/v3/tasks/${taskId}/status`, {
                method: 'PUT',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ status: 'ready' })
            });
            if (!response.ok) {
                const body = await response.text().catch(() => '');
                throw new Error(body || `Failed to reset task (HTTP ${response.status})`);
            }

            const payload = await response.json().catch(() => null);
            let resetStatus: TaskStatus = 'ready';
            if (payload && typeof payload === 'object') {
                const rawTask = (payload as Record<string, unknown>).task;
                if (rawTask && typeof rawTask === 'object') {
                    const taskRecord = rawTask as Record<string, unknown>;
                    const state = taskRecord.state;
                    const rawStatus =
                        state && typeof state === 'object'
                            ? (state as Record<string, unknown>).status
                            : taskRecord.status;
                    if (rawStatus !== undefined) {
                        resetStatus = normalizeV3TaskStatus(rawStatus);
                    }
                }
            }
            if (isStaleTaskLoad(generation, scopeKey)) {
                return resetStatus;
            }

            update((state) => {
                const tasks = state.tasks.map((candidate) =>
                    candidate.id === taskId
                        ? {
                            ...candidate,
                            status: resetStatus,
                            errorMessage: resetStatus === 'failed' ? candidate.errorMessage : undefined,
                            updatedAt: new Date().toISOString()
                        }
                        : candidate
                );
                return {
                    ...state,
                    tasks,
                    filteredTasks: computeFilteredTasks(tasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
                };
            });

            if (taskInList) {
                await loadTasksImpl();
            }
            return resetStatus;
        },

        /**
         * Set completion result on a task (from AgenticExecutionCompleted event)
         */
        setCompletionResult: (taskId: string, summary: string, outcome: string, artifactNames: string[]) => {
            update(state => {
                const newTasks = state.tasks.map(task =>
                    task.id === taskId
                        ? {
                            ...task,
                            completionSummary: summary,
                            completionOutcome: outcome,
                            completionArtifactNames: artifactNames,
                            updatedAt: new Date().toISOString()
                        }
                        : task
                );
                return {
                    ...state,
                    tasks: newTasks,
                    filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
                };
            });
        },

        /**
         * Update plan steps for a task (used by the plan editor)
         */
        updatePlanSteps: (taskId: string, steps: PlanStep[]) => {
            update(state => {
                const newTasks = state.tasks.map(task =>
                    task.id === taskId
                        ? {
                            ...task,
                            planSteps: steps,
                            updatedAt: new Date().toISOString()
                        }
                        : task
                );
                return {
                    ...state,
                    tasks: newTasks,
                    filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
                };
            });
        },

        // =================================================================
        // "Do it for me" Flow
        // =================================================================

        /**
         * Start planning for a task (first step of "Do it for me")
         * Generates AI plan without executing
         */
        planTask: async (taskId: string) => {
            const { generation, scopeKey } = nextTaskLoadToken();
            const previousTask = findTaskById(get({ subscribe }).tasks, taskId);
            ensureMutableTask(previousTask, 'plan');
            update((state) => {
                const tasks = state.tasks.map((task) =>
                    task.id === taskId
                        ? {
                            ...task,
                            status: 'planning' as TaskStatus,
                            planStatus: 'planning' as TaskPlanStatus,
                            pendingQuestion: undefined,
                            pendingQuestions: undefined,
                            errorMessage: undefined,
                            updatedAt: new Date().toISOString()
                        }
                        : task
                );
                return {
                    ...state,
                    tasks,
                    filteredTasks: computeFilteredTasks(tasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
                };
            });

            try {
                const response = await scopedTaskFetch(`/api/magician/v3/tasks/${taskId}/plan`, {
                    method: 'POST'
                });
                if (!response.ok) {
                    const errBody = await response.json().catch(() => ({}));
                    throw new Error(errBody.error || `Failed to start planning (HTTP ${response.status})`);
                }

                let result = await response.json().catch(() => null);
                let plan = result?.plan && typeof result.plan === 'object'
                    ? result.plan as Record<string, unknown>
                    : null;
                const deadline = Date.now() + TASK_PLAN_POLL_TIMEOUT_MS;

                while (
                    plan
                    && normalizeTaskPlanStatus(plan.status) === 'planning'
                    && Date.now() < deadline
                ) {
                    await new Promise((resolve) => setTimeout(resolve, TASK_PLAN_POLL_INTERVAL_MS));
                    const pollResponse = await scopedTaskFetch(`/api/magician/v3/tasks/${taskId}/plan`);
                    if (!pollResponse.ok) break;
                    result = await pollResponse.json().catch(() => null);
                    if (!result?.plan || typeof result.plan !== 'object') break;
                    plan = result.plan as Record<string, unknown>;
                }

                if (plan) {
                    if (isStaleTaskLoad(generation, scopeKey)) {
                        return plan;
                    }
                    update((state) => {
                        const tasks = state.tasks.map((task) =>
                            task.id === taskId ? applyTaskPlanPayload(task, plan) : task
                        );
                        return {
                            ...state,
                            tasks,
                            filteredTasks: computeFilteredTasks(tasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
                        };
                    });
                }

                if (!isStaleTaskLoad(generation, scopeKey)) {
                    await loadTasksImpl();
                }

                const planStatus = plan ? normalizeTaskPlanStatus(plan.status) : undefined;
                if (planStatus === 'failed') {
                    throw new Error(
                        (typeof plan?.error === 'string' && plan.error.trim().length > 0)
                            ? plan.error
                            : 'Planning failed'
                    );
                }

                return plan;
            } catch (error) {
                if (isStaleTaskLoad(generation, scopeKey)) {
                    throw error;
                }
                try {
                    await loadTasksImpl();
                } catch {
                    if (previousTask) {
                        update((state) => {
                            const tasks = state.tasks.map((task) =>
                                task.id === taskId ? previousTask : task
                            );
                            return {
                                ...state,
                                tasks,
                                filteredTasks: computeFilteredTasks(
                                    tasks,
                                    state.filter,
                                    state.searchQuery,
                                    state.pendingCompletions,
                                    state.laneAnswer
                                )
                            };
                        });
                    }
                }
                update((state) => ({
                    ...state,
                    error: error instanceof Error ? error.message : 'Planning failed'
                }));
                throw error;
            }
        },

        approvePlan: async (taskId: string, planId: string) => {
            const { generation, scopeKey } = nextTaskLoadToken();
            ensureMutableTask(findTaskById(get({ subscribe }).tasks, taskId), 'plan_approve');
            const normalizedPlanId = planId.trim();
            if (!normalizedPlanId) throw new Error('planId is required');
            const response = await scopedTaskFetch(`/api/magician/v3/tasks/${taskId}/plan/approve?plan_id=${encodeURIComponent(normalizedPlanId)}`, {
                method: 'POST'
            });
            if (!response.ok) {
                const errBody = await response.json().catch(() => ({}));
                if (isTaskPlanVersionConflict(errBody) && !isStaleTaskLoad(generation, scopeKey)) {
                    await reconcileTaskPlanConflict(taskId, scopeKey);
                }
                throw new Error(errBody.error || 'Failed to approve plan');
            }
            const result = await response.json().catch(() => null);
            if (!isStaleTaskLoad(generation, scopeKey)) {
                await loadTasksImpl();
            }
            return result;
        },

        rejectPlan: async (taskId: string, planId: string) => {
            const { generation, scopeKey } = nextTaskLoadToken();
            ensureMutableTask(findTaskById(get({ subscribe }).tasks, taskId), 'plan_reject');
            const normalizedPlanId = planId.trim();
            if (!normalizedPlanId) throw new Error('planId is required');
            const response = await scopedTaskFetch(`/api/magician/v3/tasks/${taskId}/plan/reject?plan_id=${encodeURIComponent(normalizedPlanId)}`, {
                method: 'POST'
            });
            if (!response.ok) {
                const errBody = await response.json().catch(() => ({}));
                if (isTaskPlanVersionConflict(errBody) && !isStaleTaskLoad(generation, scopeKey)) {
                    await reconcileTaskPlanConflict(taskId, scopeKey);
                }
                throw new Error(errBody.error || 'Failed to reject plan');
            }
            const result = await response.json().catch(() => null);
            if (!isStaleTaskLoad(generation, scopeKey)) {
                await loadTasksImpl();
            }
            return result;
        },

        replanTask: async (taskId: string) => {
            const { generation, scopeKey } = nextTaskLoadToken();
            ensureMutableTask(findTaskById(get({ subscribe }).tasks, taskId), 'plan_replan');
            const response = await scopedTaskFetch(`/api/magician/v3/tasks/${taskId}/plan/replan`, {
                method: 'POST'
            });
            if (!response.ok) {
                const errBody = await response.json().catch(() => ({}));
                throw new Error(errBody.error || 'Failed to restart planning');
            }
            const result = await response.json().catch(() => null);
            if (!isStaleTaskLoad(generation, scopeKey)) {
                await loadTasksImpl();
            }
            return result;
        },

        refreshTaskPlan: async (taskId: string) => {
            ensureMutableTask(findTaskById(get({ subscribe }).tasks, taskId), 'plan_refresh');
            return refreshTaskPlanIntoStore(taskId);
        },

        /**
         * Execute a planned task (after user reviews and approves)
         */
        executeTask: async (taskId: string) => {
            const { generation, scopeKey } = nextTaskLoadToken();
            const state = get({ subscribe });

            // Check execution limit
            if (state.executingTask !== null) {
                throw new Error('Another task is already executing. Please wait for it to complete.');
            }

            const task = ensureMutableTask(
                state.tasks.find(t => t.id === taskId)
                    ?? await fetchTaskRecordByIdImpl(taskId)
                    ?? undefined,
                'execute'
            );

            try {
                const response = await scopedTaskFetch(`/api/magician/v3/tasks/${taskId}/execute`, {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({})
                });

                if (!response.ok) {
                    const errBody = await response.json().catch(() => ({}));
                    const errorCode = typeof errBody?.error === 'string'
                        ? errBody.error
                        : `Failed to start execution (HTTP ${response.status})`;
                    const gateMessage = parseTaskPlanExecutionGateError(errorCode);
                    if (gateMessage) {
                        await refreshTaskPlanIntoStore(taskId).catch(() => null);
                        throw new Error(gateMessage);
                    }
                    throw new Error(errorCode);
                }

                const result = await response.json();
                const executionId = result.execution?.state?.execution_id as string | undefined;
                if (!executionId) {
                    throw new Error('Execution started but no execution id was returned');
                }
                if (isStaleTaskLoad(generation, scopeKey)) {
                    return result;
                }

                // Update task and execution state
                update(state => ({
                    ...state,
                    tasks: state.tasks.map(t =>
                        t.id === taskId
                            ? {
                                ...t,
                                status: 'running' as TaskStatus,
                                executionId: executionId,
                                activeExecutionId: executionId,
                                currentStepIndex: 0,
                                progress: 0,
                                updatedAt: new Date().toISOString()
                            }
                            : t
                    ),
                    executingTask: {
                        taskId,
                        executionId,
                        status: 'running',
                        progress: 0,
                        currentStep: 'Starting...',
                        currentStepIndex: 0,
                        totalSteps: 0,
                        clarifications: [],
                        startedAt: Date.now()
                    }
                }));

                return result;
            } catch (error) {
                if (isStaleTaskLoad(generation, scopeKey)) {
                    throw error;
                }
                update(state => ({
                    ...state,
                    error: error instanceof Error ? error.message : 'Execution failed'
                }));
                throw error;
            }
        },

        /**
         * Execute a task directly with the runtime-context harness.
         */
        executeTaskDirect: async (taskId: string) => {
            const { generation, scopeKey } = nextTaskLoadToken();
            const state = get({ subscribe });

            if (state.executingTask !== null) {
                throw new Error('Another task is already executing. Please wait for it to complete.');
            }

            const task = state.tasks.find(t => t.id === taskId);
            if (!task) {
                throw new Error('Task not found');
            }

            try {
                const response = await scopedTaskFetch(`/api/magician/v3/tasks/${taskId}/execute`, {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({})
                });

                if (!response.ok) {
                    const errBody = await response.json().catch(() => ({}));
                    const errorCode = typeof errBody?.error === 'string'
                        ? errBody.error
                        : `Failed to start direct execution (HTTP ${response.status})`;
                    const gateMessage = parseTaskPlanExecutionGateError(errorCode);
                    if (gateMessage) {
                        await refreshTaskPlanIntoStore(taskId).catch(() => null);
                        throw new Error(gateMessage);
                    }
                    throw new Error(errorCode);
                }

                const result = await response.json();
                const executionId = result.execution?.state?.execution_id as string | undefined;
                if (!executionId) {
                    throw new Error('Execution started but no execution id was returned');
                }
                if (isStaleTaskLoad(generation, scopeKey)) {
                    return result;
                }

                update(state => ({
                    ...state,
                    tasks: state.tasks.map(t =>
                        t.id === taskId
                            ? {
                                ...t,
                                status: 'running' as TaskStatus,
                                executionId,
                                activeExecutionId: executionId,
                                currentStepIndex: 0,
                                progress: 0,
                                updatedAt: new Date().toISOString()
                            }
                            : t
                    ),
                    executingTask: {
                        taskId,
                        executionId,
                        status: 'running',
                        progress: 0,
                        currentStep: 'Executing directly...',
                        currentStepIndex: 0,
                        totalSteps: 0,
                        clarifications: [],
                        startedAt: Date.now()
                    }
                }));

                return result;
            } catch (error) {
                if (isStaleTaskLoad(generation, scopeKey)) {
                    throw error;
                }
                update(state => ({
                    ...state,
                    error: error instanceof Error ? error.message : 'Direct execution failed'
                }));
                throw error;
            }
        },

        /**
         * Cancel an active execution via V3 scoped endpoint.
         * Returns the response JSON on success, throws on failure.
         */
        cancelExecution: async (executionId: string) => {
            return cancelExecutionImpl(executionId);
        },

        // =================================================================
        // Execution Updates (from WebSocket events)
        // =================================================================

        /**
         * Update execution progress from WebSocket event
         */
        updateExecutionProgress: (stepIndex: number, stepStatus: string, progress: number) => {
            update(state => {
                if (!state.executingTask) return state;

                const task = state.tasks.find(t => t.id === state.executingTask?.taskId);
                const currentStep = task?.planSteps?.[stepIndex]?.description || '';

                return {
                    ...state,
                    tasks: state.tasks.map(t =>
                        t.id === state.executingTask?.taskId
                            ? {
                                ...t,
                                currentStepIndex: stepIndex,
                                progress,
                                planSteps: t.planSteps?.map((s, i) =>
                                    i === stepIndex ? { ...s, status: stepStatus as PlanStep['status'] } : s
                                )
                            }
                            : t
                    ),
                    executingTask: {
                        ...state.executingTask,
                        currentStepIndex: stepIndex,
                        currentStep,
                        progress
                    }
                };
            });
        },

        /**
         * Handle execution completion
         */
        completeExecution: (success: boolean, error?: string) => {
            update(state => {
                if (!state.executingTask) return state;
                const taskId = state.executingTask.taskId;

                const newTasks = state.tasks.map(t => {
                    if (t.id !== taskId) return t;
                    // Scheduled tasks reset to Ready after completion so
                    // the scheduler can fire them again on the next tick.
                    const hasSchedule = !!t.schedule;
                    const terminalStatus = success ? 'completed' as TaskStatus : 'failed' as TaskStatus;
                    const effectiveStatus = hasSchedule
                        ? 'ready' as TaskStatus
                        : terminalStatus;
                    return {
                        ...t,
                        status: effectiveStatus,
                        progress: success ? (hasSchedule ? 0 : 100) : t.progress,
                        // Clear stale completion fields on scheduled reset only on success —
                        // failure fields stay visible until fetchTaskDetail refreshes.
                        // Per-execution records preserve the data in history regardless.
                        ...(hasSchedule && success ? {
                            completionSummary: undefined,
                            completionOutcome: undefined,
                            completionArtifactNames: undefined,
                        } : {})
                    };
                });
                return {
                    ...state,
                    tasks: newTasks,
                    filteredTasks: computeFilteredTasks(newTasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer),
                    executingTask: null,
                    error: success ? null : error || 'Execution failed'
                };
            });
        },

        /**
         * Add clarification question
         */
        addClarification: (questionId: string, question: string) => {
            update(state => {
                if (!state.executingTask) return state;

                return {
                    ...state,
                    tasks: state.tasks.map(t =>
                        t.id === state.executingTask?.taskId
                            ? {
                                ...t,
                                pendingQuestion: { id: questionId, question },
                                pendingQuestions: [{ id: questionId, question }]
                            }
                            : t
                    ),
                    executingTask: {
                        ...state.executingTask,
                        clarifications: [
                            ...state.executingTask.clarifications,
                            { id: questionId, question, answered: false }
                        ]
                    }
                };
            });
        },

        // =================================================================
        // UI State
        // =================================================================

        selectTask: async (taskId: string | null) => {
            update(state => ({ ...state, selectedTaskId: taskId }));

            // If selecting a task, fetch its full details including plan
            if (taskId) {
                try {
                    const state = get({ subscribe });
                    const task = state.tasks.find(t => t.id === taskId);
                    if (!task) {
                        return;
                    }

                    if (task.hasPlan || task.status === 'planning') {
                        await refreshTaskPlanIntoStore(taskId).catch(() => null);
                    }

	                const refreshedTask = get({ subscribe }).tasks.find(t => t.id === taskId) || task;
	                    const executionId = refreshedTask.executionId;
	                    if (executionId) {
	                        await refreshExecutionPlanStepsIntoStore(taskId, executionId);
	                        return;
	                    }
	                    if (refreshedTask.planSteps?.length) {
	                        return;
	                    }
	                } catch (error) {
	                    console.error('Failed to fetch task details:', error);
	                }
            }
        },

        setFilter: (filter: TaskFilter) => {
            update(state => ({
                ...state,
                filter,
                filteredTasks: computeFilteredTasks(state.tasks, filter, state.searchQuery, state.pendingCompletions, state.laneAnswer)
            }));
            // The lane the reader just asked for is the server's to answer, and
            // the previous answer describes a different lane. Rendering
            // continues off the mirror until this lands.
            void refreshTaskLaneImpl();
        },

        setSearchQuery: (query: string) => {
            update(state => ({
                ...state,
                searchQuery: query,
                filteredTasks: computeFilteredTasks(state.tasks, state.filter, query, state.pendingCompletions, state.laneAnswer)
            }));
        },

        clearError: () => {
            update(state => ({ ...state, error: null }));
        },

        // =================================================================
        // Data Loading
        // =================================================================

        /**
         * Load tasks from backend.
         *
         * Root executions are task-backed, so this path stays task-only.
         */
        loadTasks: loadTasksImpl,

        /**
         * Re-read one task and replace it in the list. `false` when the task could
         * not be read, or is not in the list to replace. See `refreshTaskImpl`.
         */
        refreshTask: refreshTaskImpl,

        /**
         * Fetch full task detail (including execution history) and merge into store.
         * Used by the execution panel to show past run results.
         */
        clearExecutionHistory: async (taskId: string, keepLatest: boolean) => {
            void keepLatest;
            update(state => {
                const tasks = state.tasks.map(t =>
                    t.id === taskId ? { ...t, executionHistory: undefined } : t
                );
                return {
                    ...state,
                    tasks,
                    filteredTasks: computeFilteredTasks(tasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer),
                };
            });
        },

        /**
         * Fetch a single task by id (GET /v3/tasks/{id}) and return a
         * fully-converted Task WITHOUT inserting it into the store list.
         * The backend resolves both `tasks/` and `internal_tasks/` by id,
         * so this works for Internal tasks too — and because it never
         * touches `state.tasks`, Internal tasks don't leak into the
         * `/tasks` list/filters. Used by the chat ExecutionPanel opener to
         * resolve ids the `/tasks` feed (loadTasks) never pulls. Returns
         * null when the task can't be resolved. The single-task endpoint
         * returns the nested {manifest,state} shape, so flatten first.
         */
        fetchTaskRecordById: async (taskId: string): Promise<Task | null> => {
            return fetchTaskRecordByIdImpl(taskId);
        },

        fetchTaskDetail: async (taskId: string) => {
            const { generation, scopeKey } = nextTaskLoadToken();
            try {
                const response = await scopedTaskFetch(`/api/magician/v3/tasks/${taskId}`);
                if (!response.ok) return;
                const data = await response.json();
                const flattened = data?.task && typeof data.task === 'object'
                    ? flattenV3TaskRecord(data.task as Record<string, unknown>)
                    : null;
                if (!flattened || isStaleTaskLoad(generation, scopeKey)) return;
                update(state => {
                    const tasks = state.tasks.map(t =>
                        t.id === taskId
                            ? {
                                ...t,
                                executionHistory: undefined,
                                status: normalizeV3TaskStatus(flattened.status),
                                title: (flattened.title as string) || t.title,
                                description: (flattened.description as string) || t.description,
                                executionId: resolveTaskExecutionId(flattened) || t.executionId,
                                activeExecutionId: resolveTaskActiveExecutionId(flattened),
                                completionSummary:
                                    typeof flattened.completion_summary === 'string'
                                        ? flattened.completion_summary
                                        : undefined,
                                completionOutcome:
                                    typeof flattened.completion_outcome === 'string'
                                        ? flattened.completion_outcome
                                        : undefined,
                                completionArtifactNames: Array.isArray(flattened.completion_artifact_names)
                                    ? flattened.completion_artifact_names.filter(
                                        (name): name is string => typeof name === 'string'
                                    )
                                    : undefined,
                                // Refreshed with the rest of the detail rather than
                                // left to the list poll: the selected task is the one
                                // whose stall the panel is reporting, and a stale
                                // progress instant over-reports silence.
                                lastProgressAt: parseLastProgressAt(flattened.last_progress_at),
                            }
                            : t
                    );
                    return {
                        ...state,
                        tasks,
                        filteredTasks: computeFilteredTasks(tasks, state.filter, state.searchQuery, state.pendingCompletions, state.laneAnswer),
                    };
                });
            } catch {
                // Silent failure — execution history is supplementary
            }
        },

        start(): void {
            activeConsumers += 1;
            if (activeConsumers !== 1) return;
            if (v2Events.getConnectionState() === 'CLOSED') {
                v2Events.connectGlobal();
            }
            startScopeBridge();
            startRealtimeBridge();
        },

        stop(): void {
            activeConsumers = Math.max(0, activeConsumers - 1);
            if (activeConsumers !== 0) return;
            stopScopeBridge();
            stopRealtimeBridge();
        },

        reset: () => set(defaultState)
    };
}

// =============================================================================
// Derived Stores
// =============================================================================

export const taskStore = createTaskStore();

/**
 * Approve a single task for execution.
 * Calls the backend approval endpoint and reloads tasks.
 */
export async function approveTask(taskId: string): Promise<void> {
    const resp = await scopedTaskFetch(`/api/magician/v3/tasks/${encodeURIComponent(taskId)}/approve`, { method: 'POST' });
    if (!resp.ok) throw await readTaskApiError(resp, 'Failed to approve task');
    await taskStore.loadTasks(); // reload tasks after approval
}

/**
 * Batch-approve multiple tasks for execution.
 * Calls the backend batch approval endpoint and reloads tasks.
 */
export async function batchApproveTasks(taskIds: string[]): Promise<void> {
    const results = await Promise.allSettled(
        taskIds.map(async taskId => {
            const resp = await scopedTaskFetch(`/api/magician/v3/tasks/${encodeURIComponent(taskId)}/approve`, {
                method: 'POST'
            });
            if (!resp.ok) {
                throw await readTaskApiError(resp, `Failed to approve task ${taskId}`);
            }
        })
    );
    await taskStore.loadTasks();
    const failures = results.flatMap((result) => {
        if (result.status === 'fulfilled') {
            return [];
        }
        return [result.reason instanceof Error ? result.reason.message : 'Failed to approve task'];
    });
    if (failures.length > 0) {
        if (failures.length === 1) {
            throw new Error(failures[0]);
        }
        throw new Error(
            `Failed to approve ${failures.length} of ${taskIds.length} tasks: ${failures[0]}`
        );
    }
}

/**
 * Currently selected task
 */
export const selectedTask = derived(taskStore, $store =>
    $store.selectedTaskId
        ? $store.tasks.find(t => t.id === $store.selectedTaskId) || null
        : null
);

/**
 * Check if execution is possible (not at limit)
 */
export const canStartExecution = derived(taskStore, $store =>
    $store.executingTask === null
);

/**
 * Running tasks count
 */
export const runningTasksCount = derived(taskStore, $store =>
    $store.tasks.filter(t => t.status === 'running' || t.status === 'paused').length
);

/**
 * The six filter badges — the **corpus**, not the loaded page.
 *
 * Every number here is the server's, counted over the whole scoped pool before
 * any lane filter was applied. Counting the client's pool instead is what made
 * a badge describe whatever happened to be loaded, and it is the half of this
 * that survives Phase 1 unchanged.
 *
 * The active lane's badge is moved by the *same* `pendingCompletionLaneDelta`
 * that moves the rows beneath it, so during a grace period the number and the
 * list cannot disagree. The other five are reported as the server counted them:
 * their rows are not on screen, and there is no answer to adjust against.
 *
 * **A lane the server did not report is `null`, never `0`.** The toolbar renders
 * no badge for `null`; a fabricated zero would be a claim nobody made.
 */
export const taskCounts = derived(taskStore, $store => {
    const counts: Record<TaskLane, number | null> = {
        all: null,
        inbox: null,
        today: null,
        overdue: null,
        running: null,
        completed: null
    };
    const server = $store.laneCounts;
    if (!server) return counts;

    const activeLane = typeof $store.filter === 'string' ? $store.filter : null;
    for (const lane of TASK_LANES) {
        const total = server[lane];
        if (typeof total !== 'number') continue;
        if (lane !== activeLane) {
            counts[lane] = total;
            continue;
        }
        const { laneRows, held } = splitTasksByLane(
            $store.tasks,
            lane,
            $store.pendingCompletions,
            $store.laneAnswer
        );
        counts[lane] = laneCountWithPendingCompletions(
            lane,
            total,
            laneRows,
            held,
            $store.pendingCompletions
        );
    }
    return counts;
});
