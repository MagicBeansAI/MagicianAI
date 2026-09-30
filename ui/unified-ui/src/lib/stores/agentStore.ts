/**
 * Agent Store - Tracks agent lifecycle and approval surface from realtime events.
 *
 * Supports both:
 * - Legacy typed phase-0 events (`AgentCycleStarted`, `AgentCycleCompleted`, `AgentTriggered`)
 * - Generic `AgentEvent` envelope events (`agent.cycle.*`, `approval.*`, ...)
 * - REST hydration + mutations for CRUD and runtime controls
 *
 * Unified Agentic Architecture: agents now have `kind`, `tools`,
 * `excluded_tools`, `delegation_targets`, and coordination config (`max_delegation_depth`).
 * Goals and triggers have been REMOVED from agents — schedules live on tasks.
 */

import { browser } from '$app/environment';
import { writable, derived, get } from 'svelte/store';
import { timedFetch } from '$lib/shared/fetch';
import type {
    V2WebSocketEvent,
    AgentCycleStartedEvent,
    AgentCycleCompletedEvent,
    AgentTriggeredEvent,
    AgentEventEnvelope
} from '$lib/realtime/v2-websocket';
import { getCurrentScopeIdentity, scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

// =============================================================================
// Types
// =============================================================================

export type AgentStatus = 'idle' | 'triggered' | 'running' | 'paused' | 'completed' | 'partial' | 'error' | 'disabled';
export type AgentKind = 'Personal' | 'Worker';

/** Memory isolation mode for personal agents. */
export type UserMemoryIsolation = 'shared' | 'fully_isolated';

/** Focus area priority levels. */
export type FocusAreaPriority = 'high' | 'medium' | 'low';

/** A focus area for autonomous agent operation. */
export interface FocusArea {
    name: string;
    description: string;
    priority: FocusAreaPriority;
    schedule?: string;
    program?: string;
    scope?: string[];
}

/** Configuration for autonomous agent behavior. */
export interface AutonomousConfig {
    schedule: string;      // cron expression
    focus_areas: FocusArea[];
    max_tasks_per_cycle: number;
    max_steps_per_plan: number;
}

export interface HarnessConfig {
    program_section?: string;
}

export interface AgentSummary {
    agent_id: string;
    name?: string;
    description?: string;
    trust_level?: string;
    kind?: AgentKind;
    disabled?: boolean;
    configured_disabled?: boolean;
    tools?: string[];
    excluded_tools?: string[];
    max_delegation_depth?: number;
    delegation_targets?: string[];
    aliases?: string[];
    wake_spellings?: string[];
    persona?: string;
    is_primary?: boolean;
    onboarding_completed?: boolean;
    autonomous_config?: AutonomousConfig;
    harness?: HarnessConfig;
    readable_agents?: string[];
    user_memory_isolation?: UserMemoryIsolation;
    version?: number;
    etag?: string;
    created_at?: number;
    status: AgentStatus;
    current_goal_id?: string;
    current_cycle_id?: string;
    current_execution_id?: string;
    last_goal?: string;
    last_outcome?: string;
    last_trigger?: string;
    iterations_used?: number;
    pending_approvals?: number;
    last_approval_id?: string;
    last_approval_status?: string;
    updated_at: number; // timestamp ms
}

export interface AgentStoreState {
    isLoading: boolean;
    error: string | null;
    lastLoadedAt: number | null;
    totalCount: number;
    mutationCount: number;
    primaryAgentId: string | null;
}

export interface LoadAgentsOptions {
    offset?: number;
    limit?: number;
    replace?: boolean;
    clearError?: boolean;
}

export interface TriggerAgentRequest {
    goal_id?: string;
    trigger?: string;
}

export interface TriggerAgentResponse {
    agent_id: string;
    goal_id: string;
    trigger: string;
    trigger_seq: number;
    cycle_id: string;
    execution_id?: string;
    definition_version: number;
    status: string;
    queue_position?: number;
}

export interface AgentPauseResumeResponse {
    agent_id: string;
    status: string;
    changed: boolean;
}

export interface HarnessRuntimeStatus {
    enabled: boolean;
    paused: boolean;
    configured_paused: boolean | null;
    env_override: boolean | null;
    effective_source: 'config' | 'environment' | 'config_error' | string;
    config_error?: string;
}

export interface HarnessRuntimeUpdateResponse extends HarnessRuntimeStatus {
    changed: boolean;
    env_override_cleared: boolean;
    harness_agents_seen: number;
    active_cycles_targeted: number;
    pending_dispatches_cleared: number;
    warnings?: string[];
}

export interface SystemAgentSummary {
    agent_id: string;
    name?: string;
    description?: string;
}

type ApprovalLifecycleStatus = 'pending' | 'approved' | 'rejected' | 'expired' | 'resolved';

interface AgentApprovalLifecycleEntry {
    status: ApprovalLifecycleStatus;
    updatedAt: number;
}

interface ApprovalLifecycleTransitionResult {
    pendingCount: number;
    applied: boolean;
}

interface AgentLifecycleWatermark {
    updatedAt: number;
    priority: number;
}

interface AgentDefinitionPayload extends Record<string, unknown> {
    agent_id?: string;
    name?: string;
    description?: string;
    trust_level?: string;
    kind?: string;
    disabled?: boolean;
    tools?: string[];
    excluded_tools?: string[];
    coordination?: Record<string, unknown>;
    delegation_targets?: string[];
    is_primary?: boolean;
    autonomous_config?: Record<string, unknown>;
    harness?: Record<string, unknown>;
    readable_agents?: string[];
    user_memory_isolation?: string;
}

interface AgentDefinitionRecordResponse {
    definition: AgentDefinitionPayload;
    version?: number;
    etag?: string;
    created_at?: string;
    updated_at?: string;
    status?: string;
    current_goal_id?: string;
    current_cycle_id?: string;
    current_execution_id?: string;
}

interface AgentMetaState {
    isLoading: boolean;
    error: string | null;
    lastLoadedAt: number | null;
    totalCount: number;
    pendingMutations: Record<string, number>;
    primaryAgentId: string | null;
}

const AGENT_SUCCESS_OUTCOMES = new Set(['success', 'goal_achieved', 'goal_achieved_partial']);
const AGENT_PAUSED_OUTCOMES = new Set(['paused']);
// tactical pattern T3: partial_progress is its own category — the agent
// produced material work but didn't fully meet the goal. Not a
// failure (real artifacts were delivered) and not a clean success
// (gaps remain). UI surfaces this as a distinct "partial" badge
// (yellow / amber), not the red error styling. `goal_achieved_partial`
// is the outcome label the executor stamps; `partial_progress` is the
// memory-tier outcome_kind. Both flow through this category.
const AGENT_PARTIAL_OUTCOMES = new Set(['partial_progress', 'goal_achieved_partial']);
const AGENT_ERROR_OUTCOMES = new Set([
    'failure',
    'cancelled',
    'error',
    'goal_failed',
    'circuit_open',
    'user_intervened',
    'budget_exhausted'
]);
const APPROVAL_REQUESTED_EVENTS = new Set(['approval.requested', 'agent.approval.requested']);
const APPROVAL_RESOLVED_EVENTS = new Set(['approval.resolved', 'agent.approval.resolved']);
const APPROVAL_EXPIRED_EVENTS = new Set(['approval.expired', 'agent.approval.expired']);
const TERMINAL_APPROVAL_STATUSES = new Set<ApprovalLifecycleStatus>([
    'approved',
    'rejected',
    'expired',
    'resolved'
]);
const APPROVAL_RECONCILE_DEBOUNCE_MS = 300;
const AGENT_LIFECYCLE_PRIORITY = {
    CREATED: 10,
    UPDATED: 20,
    RESUMED: 30,
    TRIGGERED: 40,
    CYCLE_STARTED: 50,
    PAUSED: 60,
    CYCLE_PAUSED: 70,
    CYCLE_TERMINAL: 80,
    DELETED: 90
} as const;

const VALID_AGENT_KINDS = new Set<string>(['Personal', 'Worker']);

const defaultMetaState: AgentMetaState = {
    isLoading: false,
    error: null,
    lastLoadedAt: null,
    totalCount: 0,
    pendingMutations: {},
    primaryAgentId: null
};

const agentApprovalLifecycleIndex = new Map<string, Map<string, AgentApprovalLifecycleEntry>>();
const pendingApprovalReconcileTimers = new Map<string, ReturnType<typeof setTimeout>>();
const pendingApprovalReconcileInFlight = new Set<string>();
const pendingApprovalReconcileQueued = new Set<string>();
const agentLifecycleEventWatermarks = new Map<string, AgentLifecycleWatermark>();
let agentLoadGeneration = 0;
let agentScopeUnsubscribe: (() => void) | null = null;
let lastAgentScopeKey = '';

// =============================================================================
// Helpers
// =============================================================================

function asRecord(value: unknown): Record<string, unknown> | null {
    return typeof value === 'object' && value !== null
        ? (value as Record<string, unknown>)
        : null;
}

function readString(payload: Record<string, unknown>, field: string): string | undefined {
    const value = payload[field];
    return typeof value === 'string' && value.trim().length > 0 ? value : undefined;
}

function readNumber(payload: Record<string, unknown>, field: string): number | undefined {
    const value = payload[field];
    return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function readBoolean(payload: Record<string, unknown>, field: string): boolean | undefined {
    const value = payload[field];
    return typeof value === 'boolean' ? value : undefined;
}

function readStringArray(payload: Record<string, unknown>, field: string): string[] {
    const value = payload[field];
    if (!Array.isArray(value)) return [];
    return value.filter((item): item is string => typeof item === 'string' && item.trim().length > 0);
}

function normalizeTimestamp(value: number): number {
    return Number.isFinite(value) ? value : Date.now();
}

function normalizeTimestampValue(value: unknown): number | undefined {
    if (typeof value === 'number' && Number.isFinite(value)) {
        // If epoch seconds slip through, normalize to ms.
        return value < 1_000_000_000_000 ? value * 1000 : value;
    }
    if (typeof value === 'string' && value.trim().length > 0) {
        const parsed = Date.parse(value);
        return Number.isFinite(parsed) ? parsed : undefined;
    }
    return undefined;
}

function normalizeAgentKind(value: unknown): AgentKind | undefined {
    if (typeof value !== 'string') return undefined;
    const trimmed = value.trim();
    // Normalize case-insensitively
    for (const valid of VALID_AGENT_KINDS) {
        if (trimmed.toLowerCase() === valid.toLowerCase()) return valid as AgentKind;
    }
    return undefined;
}

const VALID_USER_MEMORY_ISOLATIONS = new Set<string>(['shared', 'fully_isolated']);

function normalizeUserMemoryIsolation(value: unknown): UserMemoryIsolation | undefined {
    if (typeof value !== 'string') return undefined;
    const trimmed = value.trim().toLowerCase();
    if (VALID_USER_MEMORY_ISOLATIONS.has(trimmed)) return trimmed as UserMemoryIsolation;
    return undefined;
}

function parseAutonomousConfig(value: unknown): AutonomousConfig | undefined {
    const record = asRecord(value);
    if (!record) return undefined;

    const schedule = readString(record, 'schedule');
    if (!schedule) return undefined;

    const rawFocusAreas = Array.isArray(record.focus_areas) ? record.focus_areas : [];
    const focusAreas: FocusArea[] = rawFocusAreas.flatMap((fa: unknown) => {
            const faRec = asRecord(fa);
            if (!faRec) return [];
            const name = readString(faRec, 'name');
            const description = readString(faRec, 'description');
            const priority = readString(faRec, 'priority');
            const scheduleOverride = readString(faRec, 'schedule');
            const program = readString(faRec, 'program');
            const scope = readStringArray(faRec, 'scope');
            if (!name || !description) return [];
            const validPriority = (priority === 'high' || priority === 'medium' || priority === 'low')
                ? priority as FocusAreaPriority
                : 'medium';
            return [{
                name,
                description,
                priority: validPriority,
                schedule: scheduleOverride,
                program,
                scope: scope.length > 0 ? scope : undefined
            }];
        });

    return {
        schedule,
        focus_areas: focusAreas,
        max_tasks_per_cycle: readNumber(record, 'max_tasks_per_cycle') ?? 5,
        max_steps_per_plan: readNumber(record, 'max_steps_per_plan') ?? 10
    };
}

function parseHarnessConfig(value: unknown): HarnessConfig | undefined {
    const record = asRecord(value);
    if (!record) return undefined;
    const programSection = readString(record, 'program_section');
    return programSection ? { program_section: programSection } : {};
}

function hasOwnField(record: Record<string, unknown>, key: string): boolean {
    return Object.prototype.hasOwnProperty.call(record, key);
}

function statusFromOutcome(outcome: string): AgentStatus {
    const normalized = outcome.trim().toLowerCase();
    if (AGENT_PARTIAL_OUTCOMES.has(normalized)) return 'partial';
    if (AGENT_SUCCESS_OUTCOMES.has(normalized)) return 'completed';
    if (AGENT_PAUSED_OUTCOMES.has(normalized)) return 'paused';
    if (AGENT_ERROR_OUTCOMES.has(normalized)) return 'error';
    // Fail closed for unknown outcomes so fleet state does not show false-green.
    return 'error';
}

function statusFromApiStatus(value: string | undefined, fallback: AgentStatus): AgentStatus {
    if (!value) return fallback;
    const normalized = value.trim().toLowerCase().replace(/[\s-]/g, '_');
    if (normalized === 'idle') return 'idle';
    if (normalized === 'triggered') return 'triggered';
    if (normalized === 'running' || normalized === 'executing') return 'running';
    if (normalized === 'paused' || normalized === 'waiting_user' || normalized === 'waiting_for_confirmation') {
        return 'paused';
    }
    if (normalized === 'completed' || normalized === 'success') return 'completed';
    if (normalized === 'disabled') return 'disabled';
    if (normalized === 'error' || normalized === 'failed' || normalized === 'failure') return 'error';
    return fallback;
}

function lifecyclePriorityFromStatus(status: AgentStatus): number {
    switch (status) {
        case 'triggered':
            return AGENT_LIFECYCLE_PRIORITY.TRIGGERED;
        case 'running':
            return AGENT_LIFECYCLE_PRIORITY.CYCLE_STARTED;
        case 'paused':
            return AGENT_LIFECYCLE_PRIORITY.CYCLE_PAUSED;
        case 'completed':
        case 'error':
        case 'disabled':
            return AGENT_LIFECYCLE_PRIORITY.CYCLE_TERMINAL;
        case 'idle':
        default:
            // Idle can represent either creation or resume; keep this low so
            // same-timestamp progression events can still advance.
            return 15;
    }
}

function parseAgentRecord(
    raw: unknown,
    etagFromHeader?: string
): AgentDefinitionRecordResponse | null {
    const record = asRecord(raw);
    if (!record) return null;

    const definition = asRecord(record.definition) as AgentDefinitionPayload | null;
    if (!definition) return null;

    return {
        definition,
        version: readNumber(record, 'version'),
        etag: readString(record, 'etag') || etagFromHeader,
        created_at: readString(record, 'created_at'),
        updated_at: readString(record, 'updated_at'),
        status: readString(record, 'status'),
        current_goal_id: readString(record, 'current_goal_id'),
        current_cycle_id: readString(record, 'current_cycle_id'),
        current_execution_id: readString(record, 'current_execution_id')
    };
}

function parseSystemAgentRecord(raw: unknown): SystemAgentSummary | null {
    const record = asRecord(raw);
    const agentId = readString(record || {}, 'agent_id');
    if (!record || !agentId) return null;

    return {
        agent_id: agentId,
        name: readString(record, 'name'),
        description: readString(record, 'description')
    };
}

function summaryFromRecord(
    record: AgentDefinitionRecordResponse,
    existing?: AgentSummary
): AgentSummary | null {
    const definition = record.definition;
    const definitionRecord = definition as Record<string, unknown>;
    const agentId = typeof definition.agent_id === 'string' && definition.agent_id.trim().length > 0
        ? definition.agent_id
        : undefined;
    if (!agentId) return null;

    const recordUpdatedAt = normalizeTimestampValue(record.updated_at)
        ?? normalizeTimestampValue(record.created_at);
    const updatedAt = Math.max(
        existing?.updated_at ?? 0,
        recordUpdatedAt ?? Date.now()
    );
    const definitionDisabled = typeof definition.disabled === 'boolean' ? definition.disabled : false;
    const statusFallback = existing?.status === 'disabled' && !definitionDisabled ? 'idle' : existing?.status || 'idle';
    const apiStatus = record.status
        ? statusFromApiStatus(record.status, statusFallback)
        : statusFallback;
    const runtimeStatus = definitionDisabled ? 'disabled' : apiStatus;
    const hasActiveRuntime =
        runtimeStatus === 'running' || runtimeStatus === 'paused' || runtimeStatus === 'triggered';

    // Extract coordination fields
    const coordination = asRecord(definition.coordination);
    const hasCoordination = hasOwnField(definitionRecord, 'coordination');
    const hasLegacyDelegationDepth = hasOwnField(definitionRecord, 'max_delegation_depth');
    const maxDelegationDepth = coordination
        ? readNumber(coordination, 'max_delegation_depth')
        : readNumber(definitionRecord, 'max_delegation_depth');

    return {
        ...(existing || { agent_id: agentId }),
        agent_id: agentId,
        name: typeof definition.name === 'string' ? definition.name : existing?.name,
        description: typeof definition.description === 'string'
            ? definition.description
            : existing?.description,
        trust_level: typeof definition.trust_level === 'string'
            ? definition.trust_level
            : existing?.trust_level,
        kind: normalizeAgentKind(definition.kind) ?? existing?.kind,
        disabled: definitionDisabled || runtimeStatus === 'disabled',
        configured_disabled: definitionDisabled,
        tools: hasOwnField(definitionRecord, 'tools')
            ? readStringArray(definitionRecord, 'tools')
            : existing?.tools,
        excluded_tools: hasOwnField(definitionRecord, 'excluded_tools')
            ? readStringArray(definitionRecord, 'excluded_tools')
            : existing?.excluded_tools,
        max_delegation_depth: hasCoordination || hasLegacyDelegationDepth
            ? maxDelegationDepth
            : existing?.max_delegation_depth,
        delegation_targets: hasOwnField(definitionRecord, 'delegation_targets')
            ? readStringArray(definitionRecord, 'delegation_targets')
            : existing?.delegation_targets,
        // Server-owned definition records omit some optional fields when they
        // are cleared, so a missing key means "removed", not "keep the old
        // cached summary value".
        aliases: hasOwnField(definitionRecord, 'aliases')
            ? readStringArray(definitionRecord, 'aliases')
            : [],
        wake_spellings: hasOwnField(definitionRecord, 'wake_spellings')
            ? readStringArray(definitionRecord, 'wake_spellings')
            : [],
        persona: typeof definition.persona === 'string'
            ? definition.persona
            : existing?.persona,
        is_primary: typeof definition.is_primary === 'boolean'
            ? definition.is_primary
            : existing?.is_primary,
        onboarding_completed: typeof definition.onboarding_completed === 'boolean'
            ? definition.onboarding_completed
            : existing?.onboarding_completed,
        autonomous_config: hasOwnField(definitionRecord, 'autonomous_config')
            ? parseAutonomousConfig(definition.autonomous_config)
            : undefined,
        harness: hasOwnField(definitionRecord, 'harness')
            ? parseHarnessConfig(definition.harness)
            : undefined,
        readable_agents: hasOwnField(definitionRecord, 'readable_agents')
            ? readStringArray(definitionRecord, 'readable_agents')
            : [],
        user_memory_isolation: hasOwnField(definitionRecord, 'user_memory_isolation')
            ? normalizeUserMemoryIsolation(definition.user_memory_isolation)
            : existing?.user_memory_isolation,
        version: record.version ?? existing?.version,
        etag: record.etag || existing?.etag,
        created_at: normalizeTimestampValue(record.created_at) ?? existing?.created_at,
        status: runtimeStatus,
        current_goal_id: hasActiveRuntime
            ? record.current_goal_id ?? existing?.current_goal_id
            : undefined,
        current_cycle_id: hasActiveRuntime
            ? record.current_cycle_id ?? existing?.current_cycle_id
            : undefined,
        current_execution_id: hasActiveRuntime
            ? record.current_execution_id ?? existing?.current_execution_id
            : undefined,
        last_goal: existing?.last_goal,
        last_outcome: existing?.last_outcome,
        last_trigger: existing?.last_trigger,
        iterations_used: existing?.iterations_used,
        pending_approvals: existing?.pending_approvals ?? 0,
        last_approval_id: existing?.last_approval_id,
        last_approval_status: existing?.last_approval_status,
        updated_at: updatedAt
    };
}

function shouldApplyAgentLifecycleUpdate(
    agentId: string,
    existing: AgentSummary | undefined,
    updatedAt: number,
    priority: number
): boolean {
    const watermark = agentLifecycleEventWatermarks.get(agentId);
    if (watermark) {
        if (updatedAt < watermark.updatedAt) {
            return false;
        }
        if (updatedAt === watermark.updatedAt && priority <= watermark.priority) {
            return false;
        }
    }
    if (existing) {
        if (updatedAt < existing.updated_at) {
            return false;
        }
        if (updatedAt === existing.updated_at && priority <= lifecyclePriorityFromStatus(existing.status)) {
            return false;
        }
    }
    return true;
}

function recordAgentLifecycleWatermark(agentId: string, updatedAt: number, priority: number): void {
    const previous = agentLifecycleEventWatermarks.get(agentId);
    if (!previous || updatedAt > previous.updatedAt || (updatedAt === previous.updatedAt && priority > previous.priority)) {
        agentLifecycleEventWatermarks.set(agentId, { updatedAt, priority });
    }
}

function applyAgentLifecycleUpdate(
    agentId: string,
    updatedAt: number,
    priority: number,
    updater: (existing: AgentSummary | undefined) => AgentSummary
): boolean {
    let applied = false;
    agentMap.update((map) => {
        const existing = map.get(agentId);
        if (!shouldApplyAgentLifecycleUpdate(agentId, existing, updatedAt, priority)) {
            return map;
        }
        const next = new Map(map);
        next.set(agentId, updater(existing));
        applied = true;
        return next;
    });

    if (applied) {
        recordAgentLifecycleWatermark(agentId, updatedAt, priority);
    }
    return applied;
}

function approvalLifecycleForAgent(agentId: string): Map<string, AgentApprovalLifecycleEntry> {
    let lifecycle = agentApprovalLifecycleIndex.get(agentId);
    if (!lifecycle) {
        lifecycle = new Map();
        agentApprovalLifecycleIndex.set(agentId, lifecycle);
    }
    return lifecycle;
}

function countPendingApprovals(
    lifecycle: Map<string, AgentApprovalLifecycleEntry>
): number {
    let pending = 0;
    for (const entry of lifecycle.values()) {
        if (entry.status === 'pending') {
            pending += 1;
        }
    }
    return pending;
}

function shouldApplyApprovalLifecycleTransition(
    previous: AgentApprovalLifecycleEntry | undefined,
    nextStatus: ApprovalLifecycleStatus,
    updatedAt: number
): boolean {
    if (!previous) return true;
    if (updatedAt < previous.updatedAt) return false;
    if (TERMINAL_APPROVAL_STATUSES.has(previous.status) && previous.status !== nextStatus) {
        return false;
    }
    if (previous.status !== 'pending' && nextStatus === 'pending') {
        return false;
    }
    return true;
}

function applyApprovalLifecycleTransition(
    agentId: string,
    approvalId: string,
    nextStatus: ApprovalLifecycleStatus,
    updatedAt: number
): ApprovalLifecycleTransitionResult {
    const lifecycle = approvalLifecycleForAgent(agentId);
    const previous = lifecycle.get(approvalId);
    let applied = false;
    if (shouldApplyApprovalLifecycleTransition(previous, nextStatus, updatedAt)) {
        lifecycle.set(approvalId, {
            status: nextStatus,
            updatedAt
        });
        applied = true;
    }
    return {
        pendingCount: countPendingApprovals(lifecycle),
        applied
    };
}

function clearApprovalReconcileTimer(agentId: string): void {
    const timer = pendingApprovalReconcileTimers.get(agentId);
    if (timer) {
        clearTimeout(timer);
        pendingApprovalReconcileTimers.delete(agentId);
    }
}

function clearApprovalLifecycleForAgent(agentId: string): void {
    agentApprovalLifecycleIndex.delete(agentId);
    clearApprovalReconcileTimer(agentId);
    pendingApprovalReconcileInFlight.delete(agentId);
    pendingApprovalReconcileQueued.delete(agentId);
}

function pruneApprovalLifecycleIndex(activeAgentIds: Set<string>): void {
    for (const agentId of agentApprovalLifecycleIndex.keys()) {
        if (!activeAgentIds.has(agentId)) {
            clearApprovalLifecycleForAgent(agentId);
        }
    }
}

function pruneAgentLifecycleWatermarks(activeAgentIds: Set<string>): void {
    for (const agentId of agentLifecycleEventWatermarks.keys()) {
        if (!activeAgentIds.has(agentId)) {
            agentLifecycleEventWatermarks.delete(agentId);
        }
    }
}

async function reconcilePendingApprovalsForAgent(agentId: string): Promise<void> {
    if (!get(agentMap).has(agentId)) {
        clearApprovalLifecycleForAgent(agentId);
        return;
    }
    if (pendingApprovalReconcileInFlight.has(agentId)) {
        pendingApprovalReconcileQueued.add(agentId);
        return;
    }
    const reconcileStartedAt = Date.now();
    pendingApprovalReconcileInFlight.add(agentId);
    try {
        const response = await timedFetch(
            `/api/magician/v2/approvals?status=pending&agent_id=${encodeURIComponent(agentId)}`
        );
        if (!response.ok) {
            return;
        }

        const payload = await response.json() as unknown;
        const root = asRecord(payload) || {};
        const rawApprovals = Array.isArray(root.approvals) ? root.approvals : [];
        const pendingIds = new Set<string>();
        for (const entry of rawApprovals) {
            const approvalRecord = asRecord(entry);
            if (!approvalRecord) continue;
            const approvalId = readString(approvalRecord, 'approval_id');
            if (approvalId) pendingIds.add(approvalId);
        }

        for (const approvalId of pendingIds) {
            applyApprovalLifecycleTransition(agentId, approvalId, 'pending', reconcileStartedAt);
        }
        const lifecycle = approvalLifecycleForAgent(agentId);
        for (const [approvalId, entry] of lifecycle.entries()) {
            if (entry.status === 'pending' && !pendingIds.has(approvalId)) {
                applyApprovalLifecycleTransition(agentId, approvalId, 'resolved', reconcileStartedAt);
            }
        }

        const pendingCount = countPendingApprovals(approvalLifecycleForAgent(agentId));
        updateAgentSummary(agentId, (existing) => {
            if (existing && existing.updated_at > reconcileStartedAt) {
                return existing;
            }
            return {
                ...(existing || { agent_id: agentId }),
                agent_id: agentId,
                status: existing?.status || 'idle',
                pending_approvals: pendingCount,
                updated_at: Math.max(existing?.updated_at ?? 0, reconcileStartedAt)
            };
        });
    } catch {
        // Best-effort reconciliation only.
    } finally {
        pendingApprovalReconcileInFlight.delete(agentId);
        if (pendingApprovalReconcileQueued.delete(agentId) && get(agentMap).has(agentId)) {
            schedulePendingApprovalReconcile(agentId);
        }
    }
}

function schedulePendingApprovalReconcile(agentId: string): void {
    if (!get(agentMap).has(agentId)) {
        return;
    }
    if (pendingApprovalReconcileInFlight.has(agentId)) {
        pendingApprovalReconcileQueued.add(agentId);
        return;
    }
    clearApprovalReconcileTimer(agentId);
    const timer = setTimeout(() => {
        pendingApprovalReconcileTimers.delete(agentId);
        void reconcilePendingApprovalsForAgent(agentId);
    }, APPROVAL_RECONCILE_DEBOUNCE_MS);
    pendingApprovalReconcileTimers.set(agentId, timer);
}

function updateAgentSummary(
    agentId: string,
    updater: (existing: AgentSummary | undefined) => AgentSummary
): void {
    agentMap.update((map) => {
        const existing = map.get(agentId);
        map.set(agentId, updater(existing));
        return new Map(map);
    });
}

function restoreAgentSnapshot(agentId: string, snapshot: AgentSummary | undefined): void {
    agentMap.update((map) => {
        const next = new Map(map);
        if (snapshot) {
            next.set(agentId, snapshot);
        } else {
            next.delete(agentId);
        }
        return next;
    });
}

function applyApprovalEvent(
    agentId: string,
    payload: Record<string, unknown>,
    updatedAt: number,
    status: ApprovalLifecycleStatus
): void {
    const approvalId = readString(payload, 'approval_id');
    if (!approvalId) {
        schedulePendingApprovalReconcile(agentId);
        return;
    }

    const transition = applyApprovalLifecycleTransition(agentId, approvalId, status, updatedAt);
    if (!transition.applied) {
        schedulePendingApprovalReconcile(agentId);
        return;
    }

    updateAgentSummary(agentId, (existing) => {
        return {
            ...(existing || { agent_id: agentId }),
            agent_id: agentId,
            status: existing?.status || 'idle',
            pending_approvals: transition.pendingCount,
            last_approval_id: approvalId,
            last_approval_status: status,
            updated_at: Math.max(existing?.updated_at ?? 0, updatedAt)
        };
    });

    schedulePendingApprovalReconcile(agentId);
}

function applyTypedTriggered(data: AgentTriggeredEvent): void {
    const updatedAt = normalizeTimestamp(data.timestamp);
    applyAgentLifecycleUpdate(data.agent_id, updatedAt, AGENT_LIFECYCLE_PRIORITY.TRIGGERED, (existing) => ({
        ...(existing || { agent_id: data.agent_id }),
        agent_id: data.agent_id,
        status: 'triggered',
        current_goal_id: data.goal_id,
        current_cycle_id: undefined,
        current_execution_id: undefined,
        last_trigger: data.trigger,
        updated_at: updatedAt
    }));
}

function applyTypedCycleStarted(data: AgentCycleStartedEvent): void {
    const updatedAt = normalizeTimestamp(data.timestamp);
    applyAgentLifecycleUpdate(data.agent_id, updatedAt, AGENT_LIFECYCLE_PRIORITY.CYCLE_STARTED, (existing) => ({
        ...(existing || { agent_id: data.agent_id }),
        agent_id: data.agent_id,
        status: 'running',
        current_goal_id: data.goal_id,
        current_cycle_id: data.cycle_id,
        current_execution_id: data.execution_id || existing?.current_execution_id,
        last_goal: data.goal,
        updated_at: updatedAt
    }));
}

function applyTypedCycleCompleted(data: AgentCycleCompletedEvent): void {
    const updatedAt = normalizeTimestamp(data.timestamp);
    const status = statusFromOutcome(data.outcome);
    const priority = status === 'paused'
        ? AGENT_LIFECYCLE_PRIORITY.CYCLE_PAUSED
        : AGENT_LIFECYCLE_PRIORITY.CYCLE_TERMINAL;
    const retainActiveContext = status === 'paused';
    applyAgentLifecycleUpdate(data.agent_id, updatedAt, priority, (existing) => ({
        ...(existing || { agent_id: data.agent_id }),
        agent_id: data.agent_id,
        status,
        current_goal_id: retainActiveContext ? data.goal_id : undefined,
        current_cycle_id: retainActiveContext ? data.cycle_id : undefined,
        current_execution_id: retainActiveContext
            ? data.execution_id || existing?.current_execution_id
            : undefined,
        last_outcome: data.outcome,
        iterations_used: data.iterations_used,
        updated_at: updatedAt
    }));
}

async function readApiError(response: Response): Promise<string> {
    let message = `Request failed (${response.status})`;
    try {
        const text = await response.text();
        if (!text) return message;

        try {
            const parsed = JSON.parse(text) as unknown;
            const root = asRecord(parsed);
            const rootMessage = root ? readString(root, 'message') : undefined;
            const errorRecord = root ? asRecord(root.error) : null;
            const nestedMessage = errorRecord ? readString(errorRecord, 'message') : undefined;
            message = `Request failed (${response.status}): ${nestedMessage || rootMessage || text}`;
        } catch {
            message = `Request failed (${response.status}): ${text}`;
        }
    } catch {
        // Best effort only.
    }
    return message;
}

async function expectOk(response: Response): Promise<void> {
    if (!response.ok) {
        throw new Error(await readApiError(response));
    }
}

function beginMutation(key: string): void {
    agentMeta.update((state) => {
        const next = { ...state.pendingMutations };
        next[key] = (next[key] || 0) + 1;
        return {
            ...state,
            pendingMutations: next
        };
    });
}

function endMutation(key: string): void {
    agentMeta.update((state) => {
        const next = { ...state.pendingMutations };
        if (!next[key]) {
            return state;
        }
        if (next[key] <= 1) {
            delete next[key];
        } else {
            next[key] -= 1;
        }
        return {
            ...state,
            pendingMutations: next
        };
    });
}

function scopedMutationKey(agentId: string, scopeKey: string = currentAgentScopeKey()): string {
    return `${scopeKey}::${agentId}`;
}

function syncTotalCountWithMap(): void {
    const size = get(agentMap).size;
    agentMeta.update((state) => ({
        ...state,
        totalCount: size
    }));
}

function syncPrimaryAgentIdWithMap(): void {
    const primaryAgentId = Array.from(get(agentMap).values()).find((agent) => agent.is_primary)?.agent_id ?? null;
    agentMeta.update((state) => ({
        ...state,
        primaryAgentId
    }));
}

function mergeAgentRecord(record: AgentDefinitionRecordResponse): AgentSummary | null {
    let merged: AgentSummary | null = null;
    agentMap.update((map) => {
        const next = new Map(map);
        const maybeSummary = summaryFromRecord(
            record,
            record.definition.agent_id ? next.get(record.definition.agent_id) : undefined
        );
        if (!maybeSummary) return next;

        if (maybeSummary.is_primary) {
            for (const [agentId, agent] of next) {
                if (agentId !== maybeSummary.agent_id && agent.is_primary) {
                    next.set(agentId, { ...agent, is_primary: false });
                }
            }
        }
        next.set(maybeSummary.agent_id, maybeSummary);
        merged = maybeSummary;
        return next;
    });
    if (merged) {
        syncTotalCountWithMap();
        syncPrimaryAgentIdWithMap();
    }
    return merged;
}

function setAgentError(error: string | null): void {
    agentMeta.update((state) => ({
        ...state,
        error
    }));
}

function currentAgentScopeKey(): string {
    const scope = getCurrentScopeIdentity();
    return `${scope.principal}:${scope.workspace}`;
}

function nextAgentLoadToken(): { generation: number; scopeKey: string } {
    return {
        generation: agentLoadGeneration,
        scopeKey: currentAgentScopeKey()
    };
}

function isStaleAgentLoad(generation: number, scopeKey: string): boolean {
    return generation !== agentLoadGeneration || currentAgentScopeKey() !== scopeKey;
}

function clearAgentsForScopeChange(): void {
    for (const agentId of Array.from(agentApprovalLifecycleIndex.keys())) {
        clearApprovalLifecycleForAgent(agentId);
    }
    for (const timer of pendingApprovalReconcileTimers.values()) {
        clearTimeout(timer);
    }
    pendingApprovalReconcileTimers.clear();
    pendingApprovalReconcileInFlight.clear();
    pendingApprovalReconcileQueued.clear();
    agentApprovalLifecycleIndex.clear();
    agentLifecycleEventWatermarks.clear();

    agentMap.set(new Map());
    systemAgentMap.set(new Map());
    agentMeta.set({
        ...defaultMetaState,
        totalCount: 0
    });
}

function startAgentScopeBridge(): void {
    if (agentScopeUnsubscribe || !browser) return;
    const currentScope = get(scopeIdentityStore);
    lastAgentScopeKey = `${currentScope.principal}:${currentScope.workspace}`;
    agentScopeUnsubscribe = scopeIdentityStore.subscribe((scope) => {
        const scopeKey = `${scope.principal}:${scope.workspace}`;
        if (scopeKey === lastAgentScopeKey) return;
        lastAgentScopeKey = scopeKey;
        agentLoadGeneration += 1;
        clearAgentsForScopeChange();
        void refreshAgents();
    });
}

// =============================================================================
// Store
// =============================================================================

const agentMap = writable<Map<string, AgentSummary>>(new Map());
const agentMeta = writable<AgentMetaState>(defaultMetaState);
const systemAgentMap = writable<Map<string, SystemAgentSummary>>(new Map());

/** Sorted list of all known agents (most recently updated first). */
export const agentList = derived(agentMap, ($map) =>
    Array.from($map.values()).sort((a, b) => b.updated_at - a.updated_at)
);

/** Personal agents only — used for the task creation agent picker. */
export const personalAgentList = derived(agentMap, ($map) =>
    Array.from($map.values())
        .filter((agent) => agent.kind === 'Personal')
        .sort((a, b) => (a.name || a.agent_id).localeCompare(b.name || b.agent_id))
);

/** The primary personal agent (the one with `is_primary === true`). */
export const primaryAgent = derived(agentMap, ($map) => {
    for (const agent of $map.values()) {
        if (agent.is_primary) return agent;
    }
    return null;
});

/** Sorted list of read-only system/internal agents. */
export const systemAgentList = derived(systemAgentMap, ($map) =>
    Array.from($map.values()).sort((a, b) => a.agent_id.localeCompare(b.agent_id))
);

/** Number of agents currently running a cycle. */
export const runningAgentCount = derived(agentMap, ($map) => {
    let count = 0;
    for (const agent of $map.values()) {
        if (agent.status === 'running' || agent.status === 'triggered') count++;
    }
    return count;
});

/** Agent status distribution for dashboard-level summaries. */
export const agentStatusCounts = derived(agentMap, ($map) => {
    const counts: Record<AgentStatus, number> = {
        idle: 0,
        triggered: 0,
        running: 0,
        paused: 0,
        completed: 0,
        partial: 0,
        error: 0,
        disabled: 0
    };

    for (const agent of $map.values()) {
        counts[agent.status] += 1;
    }

    return counts;
});

/** Agents currently blocked on approvals. */
export const agentsWithPendingApprovals = derived(agentList, ($agents) =>
    $agents.filter((agent) => (agent.pending_approvals || 0) > 0)
);

/** Attention queue for dashboard callouts. */
export const agentsNeedingAttention = derived(agentList, ($agents) =>
    $agents.filter((agent) => agent.status === 'error' || (agent.pending_approvals || 0) > 0)
);

/** Lookup map keyed by agent_id for detail routes. */
export const agentLookup = derived(agentMap, ($map) => Object.fromEntries($map.entries()));

/** Agent store operational state for UI loading/error surfaces. */
export const agentStoreState = derived(agentMeta, ($meta): AgentStoreState => ({
    isLoading: $meta.isLoading,
    error: $meta.error,
    lastLoadedAt: $meta.lastLoadedAt,
    totalCount: $meta.totalCount,
    mutationCount: Object.values($meta.pendingMutations).reduce((sum, value) => sum + value, 0),
    primaryAgentId: $meta.primaryAgentId
}));

export function getAgentSnapshot(agentId: string): AgentSummary | undefined {
    return get(agentMap).get(agentId);
}

export function isAgentMutating(agentId: string): boolean {
    return Boolean(get(agentMeta).pendingMutations[scopedMutationKey(agentId)]);
}

// =============================================================================
// Event Dispatcher
// =============================================================================

/**
 * Process an agent lifecycle event from the WebSocket and update the store.
 * Called from v2-websocket.ts handleEvent().
 */
export function handleAgentEvent(event: V2WebSocketEvent): void {
    switch (event.event_type) {
        case 'AgentTriggered': {
            applyTypedTriggered(event.data as AgentTriggeredEvent);
            break;
        }
        case 'AgentCycleStarted': {
            applyTypedCycleStarted(event.data as AgentCycleStartedEvent);
            break;
        }
        case 'AgentCycleCompleted': {
            applyTypedCycleCompleted(event.data as AgentCycleCompletedEvent);
            break;
        }
    }
}

/**
 * Process a generic `AgentEvent` envelope event.
 * Unknown event types are ignored by design for forward compatibility.
 */
export function handleAgentEnvelopeEvent(envelope: AgentEventEnvelope): void {
    const updatedAt = normalizeTimestamp(envelope.timestamp);
    const payload = asRecord(envelope.payload) || {};

    switch (envelope.event_type) {
        case 'agent.created': {
            const applied = applyAgentLifecycleUpdate(
                envelope.agent_id,
                updatedAt,
                AGENT_LIFECYCLE_PRIORITY.CREATED,
                (existing) => ({
                    ...(existing || { agent_id: envelope.agent_id }),
                    agent_id: envelope.agent_id,
                    status: 'idle',
                    updated_at: updatedAt
                })
            );
            if (applied) {
                syncTotalCountWithMap();
            }
            return;
        }
        case 'agent.deleted': {
            let accepted = false;
            let removed = false;
            agentMap.update((map) => {
                const existing = map.get(envelope.agent_id);
                if (!shouldApplyAgentLifecycleUpdate(
                    envelope.agent_id,
                    existing,
                    updatedAt,
                    AGENT_LIFECYCLE_PRIORITY.DELETED
                )) {
                    return map;
                }
                accepted = true;

                if (!existing) {
                    return map;
                }

                const next = new Map(map);
                removed = next.delete(envelope.agent_id);
                return next;
            });
            if (!accepted) {
                return;
            }
            recordAgentLifecycleWatermark(envelope.agent_id, updatedAt, AGENT_LIFECYCLE_PRIORITY.DELETED);
            clearApprovalLifecycleForAgent(envelope.agent_id);
            if (removed) {
                syncTotalCountWithMap();
                syncPrimaryAgentIdWithMap();
            }
            return;
        }
        case 'agent.updated': {
            applyAgentLifecycleUpdate(envelope.agent_id, updatedAt, AGENT_LIFECYCLE_PRIORITY.UPDATED, (existing) => ({
                ...(existing || { agent_id: envelope.agent_id }),
                agent_id: envelope.agent_id,
                status: existing?.status || 'idle',
                updated_at: updatedAt
            }));
            return;
        }
        case 'agent.paused': {
            applyAgentLifecycleUpdate(envelope.agent_id, updatedAt, AGENT_LIFECYCLE_PRIORITY.PAUSED, (existing) => ({
                ...(existing || { agent_id: envelope.agent_id }),
                agent_id: envelope.agent_id,
                status: 'paused',
                current_goal_id: undefined,
                current_cycle_id: undefined,
                current_execution_id: undefined,
                updated_at: updatedAt
            }));
            return;
        }
        case 'agent.resumed': {
            applyAgentLifecycleUpdate(envelope.agent_id, updatedAt, AGENT_LIFECYCLE_PRIORITY.RESUMED, (existing) => ({
                ...(existing || { agent_id: envelope.agent_id }),
                agent_id: envelope.agent_id,
                status: 'idle',
                current_goal_id: undefined,
                current_cycle_id: undefined,
                current_execution_id: undefined,
                updated_at: updatedAt
            }));
            return;
        }
        case 'agent.triggered': {
            applyAgentLifecycleUpdate(envelope.agent_id, updatedAt, AGENT_LIFECYCLE_PRIORITY.TRIGGERED, (existing) => ({
                ...(existing || { agent_id: envelope.agent_id }),
                agent_id: envelope.agent_id,
                status: 'triggered',
                current_goal_id: readString(payload, 'goal_id') || existing?.current_goal_id,
                current_cycle_id: undefined,
                current_execution_id: undefined,
                last_trigger: readString(payload, 'trigger')
                    || readString(payload, 'trigger_type')
                    || existing?.last_trigger,
                updated_at: updatedAt
            }));
            return;
        }
        case 'agent.cycle.started': {
            applyAgentLifecycleUpdate(envelope.agent_id, updatedAt, AGENT_LIFECYCLE_PRIORITY.CYCLE_STARTED, (existing) => ({
                ...(existing || { agent_id: envelope.agent_id }),
                agent_id: envelope.agent_id,
                status: 'running',
                current_goal_id: readString(payload, 'goal_id') || existing?.current_goal_id,
                current_cycle_id: readString(payload, 'cycle_id') || existing?.current_cycle_id,
                current_execution_id: readString(payload, 'execution_id') || existing?.current_execution_id,
                last_trigger: readString(payload, 'trigger') || existing?.last_trigger,
                updated_at: updatedAt
            }));
            return;
        }
        case 'agent.cycle.completed':
        case 'agent.cycle.failed':
        case 'agent.cycle.paused': {
            const outcome = readString(payload, 'outcome')
                || (envelope.event_type === 'agent.cycle.failed'
                    ? 'failure'
                    : envelope.event_type === 'agent.cycle.paused'
                        ? 'paused'
                        : 'success');
            const status = statusFromOutcome(outcome);
            const priority = status === 'paused'
                ? AGENT_LIFECYCLE_PRIORITY.CYCLE_PAUSED
                : AGENT_LIFECYCLE_PRIORITY.CYCLE_TERMINAL;
            const retainActiveContext = status === 'paused';
            applyAgentLifecycleUpdate(envelope.agent_id, updatedAt, priority, (existing) => ({
                ...(existing || { agent_id: envelope.agent_id }),
                agent_id: envelope.agent_id,
                status,
                current_goal_id: retainActiveContext
                    ? readString(payload, 'goal_id') || existing?.current_goal_id
                    : undefined,
                current_cycle_id: retainActiveContext
                    ? readString(payload, 'cycle_id') || existing?.current_cycle_id
                    : undefined,
                current_execution_id: retainActiveContext
                    ? readString(payload, 'execution_id') || existing?.current_execution_id
                    : undefined,
                last_outcome: outcome,
                iterations_used: readNumber(payload, 'iterations_used') ?? existing?.iterations_used,
                updated_at: updatedAt
            }));
            return;
        }
    }

    if (APPROVAL_REQUESTED_EVENTS.has(envelope.event_type)) {
        applyApprovalEvent(envelope.agent_id, payload, updatedAt, 'pending');
        return;
    }
    if (APPROVAL_RESOLVED_EVENTS.has(envelope.event_type)) {
        const decision = readString(payload, 'decision');
        const resolvedStatus: ApprovalLifecycleStatus =
            decision === 'approve' || decision === 'approved'
                ? 'approved'
                : decision === 'reject' || decision === 'rejected'
                    ? 'rejected'
                    : 'resolved';
        applyApprovalEvent(envelope.agent_id, payload, updatedAt, resolvedStatus);
        return;
    }
    if (APPROVAL_EXPIRED_EVENTS.has(envelope.event_type)) {
        applyApprovalEvent(envelope.agent_id, payload, updatedAt, 'expired');
    }
}

// =============================================================================
// REST Hydration + Mutations
// =============================================================================

/**
 * Load agents from REST API and merge into normalized state.
 */
export async function loadAgents(options: LoadAgentsOptions = {}): Promise<AgentSummary[]> {
    const { generation, scopeKey } = nextAgentLoadToken();
    const offset = options.offset ?? 0;
    const limit = options.limit;
    const replace = options.replace ?? offset === 0;
    const shouldClearError = options.clearError ?? true;

    if (shouldClearError) {
        setAgentError(null);
    }
    agentMeta.update((state) => ({
        ...state,
        isLoading: true
    }));

    try {
        const params = new URLSearchParams();
        if (offset > 0) params.set('offset', String(offset));
        if (typeof limit === 'number' && Number.isFinite(limit)) {
            params.set('limit', String(limit));
        }
        const query = params.toString();
        const url = query.length > 0 ? `/api/magician/v2/agents?${query}` : '/api/magician/v2/agents';

        const response = await timedFetch(url);
        await expectOk(response);

        const payload = await response.json() as unknown;
        const root = asRecord(payload) || {};
        const rawAgents = Array.isArray(root.agents) ? root.agents : [];
        const rawSystemAgents = Array.isArray(root.system_agents) ? root.system_agents : [];
        const parsedRecords = rawAgents
            .map((raw) => parseAgentRecord(raw))
            .filter((record): record is AgentDefinitionRecordResponse => Boolean(record));
        const parsedSystemAgents = rawSystemAgents
            .map((raw) => parseSystemAgentRecord(raw))
            .filter((record): record is SystemAgentSummary => Boolean(record));

        if (isStaleAgentLoad(generation, scopeKey)) {
            return [];
        }

        const normalizedSystemAgents = new Map<string, SystemAgentSummary>();
        for (const systemAgent of parsedSystemAgents) {
            normalizedSystemAgents.set(systemAgent.agent_id, systemAgent);
        }
        systemAgentMap.set(normalizedSystemAgents);
        const hydrated: AgentSummary[] = [];
        agentMap.update((map) => {
            const next = replace ? new Map<string, AgentSummary>() : new Map(map);
            for (const record of parsedRecords) {
                const agentId = record.definition.agent_id;
                if (!agentId) continue;
                const merged = summaryFromRecord(record, next.get(agentId) || map.get(agentId));
                if (!merged) continue;
                next.set(agentId, merged);
                hydrated.push(merged);
            }
            return next;
        });
        if (replace) {
            const activeAgentIds = new Set(get(agentMap).keys());
            pruneApprovalLifecycleIndex(activeAgentIds);
            pruneAgentLifecycleWatermarks(activeAgentIds);
        }

        // Detect primary agent from loaded data.
        const detectedPrimaryId = hydrated.find((a) => a.is_primary)?.agent_id ?? null;

        const totalCountFromResponse = readNumber(root, 'total_count');
        agentMeta.update((state) => ({
            ...state,
            isLoading: false,
            error: null,
            lastLoadedAt: Date.now(),
            totalCount: totalCountFromResponse ?? get(agentMap).size,
            primaryAgentId: detectedPrimaryId ?? (replace ? null : state.primaryAgentId)
        }));

        return hydrated;
    } catch (error) {
        if (isStaleAgentLoad(generation, scopeKey)) {
            return [];
        }
        const message = error instanceof Error ? error.message : 'Failed to load agents';
        agentMeta.update((state) => ({
            ...state,
            isLoading: false,
            error: message
        }));
        throw error;
    }
}

/**
 * Refresh full agent list from API (replace local cache).
 */
export async function refreshAgents(): Promise<AgentSummary[]> {
    return loadAgents({ offset: 0, replace: true, clearError: true });
}

/**
 * Load a single agent definition and merge into store.
 */
export async function loadAgent(agentId: string): Promise<AgentSummary | null> {
    const normalizedAgentId = agentId.trim();
    if (!normalizedAgentId) {
        throw new Error('agentId is required');
    }
    const { generation, scopeKey } = nextAgentLoadToken();

    setAgentError(null);
    agentMeta.update((state) => ({
        ...state,
        isLoading: true
    }));

	try {
		const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}`);
		if (isStaleAgentLoad(generation, scopeKey)) {
			return null;
		}
		if (response.status === 404) {
			agentMap.update((map) => {
				const next = new Map(map);
				next.delete(normalizedAgentId);
				return next;
            });
            clearApprovalLifecycleForAgent(normalizedAgentId);
            syncTotalCountWithMap();
            syncPrimaryAgentIdWithMap();
            agentMeta.update((state) => ({
                ...state,
                isLoading: false,
                lastLoadedAt: Date.now()
            }));
            return null;
        }

        await expectOk(response);
        const payload = await response.json() as unknown;
        const record = parseAgentRecord(payload, response.headers.get('etag') || undefined);
        if (!record) {
            throw new Error('Malformed agent record payload');
        }
        if (isStaleAgentLoad(generation, scopeKey)) {
            return null;
        }

        const merged = mergeAgentRecord(record);
        agentMeta.update((state) => ({
            ...state,
            isLoading: false,
            error: null,
            lastLoadedAt: Date.now(),
            totalCount: get(agentMap).size
        }));

        return merged;
    } catch (error) {
        if (isStaleAgentLoad(generation, scopeKey)) {
            return null;
        }
        const message = error instanceof Error ? error.message : 'Failed to load agent';
        agentMeta.update((state) => ({
            ...state,
            isLoading: false,
            error: message
        }));
        throw error;
    }
}

export async function reconcileAgentDefinitionChange(agentId: string): Promise<void> {
    const normalizedAgentId = agentId.trim();
    if (!normalizedAgentId) {
        return;
    }
    const { generation, scopeKey } = nextAgentLoadToken();

    try {
        const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}`);

        if (response.status === 404) {
            if (isStaleAgentLoad(generation, scopeKey)) {
                return;
            }
            agentMap.update((map) => {
                const next = new Map(map);
                next.delete(normalizedAgentId);
                return next;
            });
            clearApprovalLifecycleForAgent(normalizedAgentId);
            syncTotalCountWithMap();
            syncPrimaryAgentIdWithMap();
            return;
        }

        if (!response.ok) {
            return;
        }

        const payload = await response.json() as unknown;
        const record = parseAgentRecord(payload, response.headers.get('etag') || undefined);
        if (!record) {
            return;
        }
        if (isStaleAgentLoad(generation, scopeKey)) {
            return;
        }

        mergeAgentRecord(record);
    } catch {
        // Passive realtime reconciliation should not surface global fetch noise.
    }
}

/**
 * Create a new agent definition.
 */
export async function createAgent(definition: Record<string, unknown>): Promise<AgentSummary> {
    const requestedAgentId = typeof definition.agent_id === 'string' && definition.agent_id.trim().length > 0
        ? definition.agent_id
        : '__create__';
    const { generation, scopeKey } = nextAgentLoadToken();
    const mutationKey = scopedMutationKey(requestedAgentId, scopeKey);

    beginMutation(mutationKey);
    setAgentError(null);

    try {
        const response = await timedFetch('/api/magician/v2/agents', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(definition)
        });
        await expectOk(response);

        const payload = await response.json() as unknown;
        const record = parseAgentRecord(payload, response.headers.get('etag') || undefined);
        if (!record) {
            throw new Error('Malformed create agent response');
        }
        const stale = isStaleAgentLoad(generation, scopeKey);
        const createdSummary = summaryFromRecord(record, undefined);
        if (!createdSummary) {
            throw new Error('Created agent record missing `agent_id`');
        }
        if (stale) {
            return createdSummary;
        }

        const merged = mergeAgentRecord(record);
        if (!merged) {
            throw new Error('Created agent record missing `agent_id`');
        }

        agentMeta.update((state) => ({
            ...state,
            error: null,
            totalCount: get(agentMap).size
        }));
        return merged;
	} catch (error) {
		if (isStaleAgentLoad(generation, scopeKey)) {
			throw error;
		}
		const message = error instanceof Error ? error.message : 'Failed to create agent';
		setAgentError(message);
		throw error;
	} finally {
		endMutation(mutationKey);
    }
}

/**
 * Update an existing agent definition.
 */
export async function updateAgent(
    agentId: string,
    definition: Record<string, unknown>,
    options?: { ifMatch?: string }
): Promise<AgentSummary> {
    const normalizedAgentId = agentId.trim();
    if (!normalizedAgentId) {
        throw new Error('agentId is required');
    }
    const { generation, scopeKey } = nextAgentLoadToken();
    const mutationKey = scopedMutationKey(normalizedAgentId, scopeKey);

    beginMutation(mutationKey);
    setAgentError(null);

    try {
        const existing = get(agentMap).get(normalizedAgentId);
        const ifMatch = options?.ifMatch || existing?.etag;

        const headers: Record<string, string> = {
            'Content-Type': 'application/json'
        };
        if (ifMatch) {
            headers['If-Match'] = ifMatch;
        }

        const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}`, {
            method: 'PUT',
            headers,
            body: JSON.stringify(definition)
        });
        await expectOk(response);

        const payload = await response.json() as unknown;
        const record = parseAgentRecord(payload, response.headers.get('etag') || undefined);
        if (!record) {
            throw new Error('Malformed update agent response');
        }
        const stale = isStaleAgentLoad(generation, scopeKey);
        const updatedSummary = summaryFromRecord(record, undefined);
        if (!updatedSummary) {
            throw new Error('Updated agent record missing `agent_id`');
        }
        if (stale) {
            return updatedSummary;
        }

        const merged = mergeAgentRecord(record);
        if (!merged) {
            throw new Error('Updated agent record missing `agent_id`');
        }

        setAgentError(null);
        return merged;
	} catch (error) {
		if (isStaleAgentLoad(generation, scopeKey)) {
			throw error;
		}
		const message = error instanceof Error ? error.message : 'Failed to update agent';
		setAgentError(message);
		throw error;
	} finally {
		endMutation(mutationKey);
    }
}

/**
 * Partially update an agent definition using JSON Merge Patch (RFC 7386).
 * Only the provided keys are changed; omitted keys are preserved.
 * Send `null` for a key to remove it.
 */
export async function patchAgent(
    agentId: string,
    patch: Record<string, unknown>,
    options?: { ifMatch?: string }
): Promise<AgentSummary> {
    const normalizedAgentId = agentId.trim();
    if (!normalizedAgentId) {
        throw new Error('agentId is required');
    }
    const { generation, scopeKey } = nextAgentLoadToken();
    const mutationKey = scopedMutationKey(normalizedAgentId, scopeKey);

    beginMutation(mutationKey);
    setAgentError(null);

    try {
        const existing = get(agentMap).get(normalizedAgentId);
        const ifMatch = options?.ifMatch || existing?.etag;

        const headers: Record<string, string> = {
            'Content-Type': 'application/json'
        };
        if (ifMatch) {
            headers['If-Match'] = ifMatch;
        }

        const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}`, {
            method: 'PATCH',
            headers,
            body: JSON.stringify(patch)
        });
        await expectOk(response);

        const payload = await response.json() as unknown;
        const record = parseAgentRecord(payload, response.headers.get('etag') || undefined);
        if (!record) {
            throw new Error('Malformed patch agent response');
        }
        const stale = isStaleAgentLoad(generation, scopeKey);
        const patchedSummary = summaryFromRecord(record, undefined);
        if (!patchedSummary) {
            throw new Error('Patched agent record missing `agent_id`');
        }
        if (stale) {
            return patchedSummary;
        }

        const merged = mergeAgentRecord(record);
        if (!merged) {
            throw new Error('Patched agent record missing `agent_id`');
        }

        setAgentError(null);
        return merged;
	} catch (error) {
		if (isStaleAgentLoad(generation, scopeKey)) {
			throw error;
		}
		const message = error instanceof Error ? error.message : 'Failed to patch agent';
		setAgentError(message);
		throw error;
	} finally {
		endMutation(mutationKey);
    }
}

/**
 * Delete an agent definition.
 */
export async function deleteAgent(agentId: string): Promise<boolean> {
    const normalizedAgentId = agentId.trim();
    if (!normalizedAgentId) {
        throw new Error('agentId is required');
    }
    const { generation, scopeKey } = nextAgentLoadToken();
    const mutationKey = scopedMutationKey(normalizedAgentId, scopeKey);

    beginMutation(mutationKey);
    setAgentError(null);

    try {
        const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}`, {
            method: 'DELETE'
        });

        if (response.status === 404) {
            if (isStaleAgentLoad(generation, scopeKey)) {
                return false;
            }
            agentMap.update((map) => {
                const next = new Map(map);
                next.delete(normalizedAgentId);
                return next;
            });
            clearApprovalLifecycleForAgent(normalizedAgentId);
            syncTotalCountWithMap();
            syncPrimaryAgentIdWithMap();
            return false;
        }

        await expectOk(response);
        if (isStaleAgentLoad(generation, scopeKey)) {
            return true;
        }

        agentMap.update((map) => {
            const next = new Map(map);
            next.delete(normalizedAgentId);
            return next;
        });
        clearApprovalLifecycleForAgent(normalizedAgentId);
        syncTotalCountWithMap();
        syncPrimaryAgentIdWithMap();
        return true;
	} catch (error) {
		if (isStaleAgentLoad(generation, scopeKey)) {
			throw error;
		}
		const message = error instanceof Error ? error.message : 'Failed to delete agent';
		setAgentError(message);
		throw error;
	} finally {
		endMutation(mutationKey);
    }
}

/**
 * Set an agent as the primary personal agent.
 * Calls POST /agents/{agentId}/set-primary and updates local state.
 */
export async function setPrimary(agentId: string): Promise<void> {
    const normalizedAgentId = agentId.trim();
    if (!normalizedAgentId) {
        throw new Error('agentId is required');
    }
    const { generation, scopeKey } = nextAgentLoadToken();
    const mutationKey = scopedMutationKey(normalizedAgentId, scopeKey);

    beginMutation(mutationKey);
    setAgentError(null);

    try {
        const response = await timedFetch(
            `/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}/set-primary`,
            { method: 'POST' }
        );
        await expectOk(response);
        if (isStaleAgentLoad(generation, scopeKey)) {
            return;
        }

        // Update local state: clear is_primary on all agents, set it on the target.
        agentMap.update((map) => {
            const next = new Map(map);
            for (const [id, agent] of next) {
                if (agent.is_primary && id !== normalizedAgentId) {
                    next.set(id, { ...agent, is_primary: false });
                }
            }
            const target = next.get(normalizedAgentId);
            if (target) {
                next.set(normalizedAgentId, { ...target, is_primary: true });
            }
            return next;
        });

        agentMeta.update((state) => ({
            ...state,
            primaryAgentId: normalizedAgentId
        }));
	} catch (error) {
		if (isStaleAgentLoad(generation, scopeKey)) {
			throw error;
		}
		const message = error instanceof Error ? error.message : 'Failed to set primary agent';
		setAgentError(message);
		throw error;
	} finally {
		endMutation(mutationKey);
    }
}

/**
 * Manually trigger an agent cycle.
 */
export async function triggerAgent(
    agentId: string,
    request: TriggerAgentRequest = {}
): Promise<TriggerAgentResponse> {
    const normalizedAgentId = agentId.trim();
    if (!normalizedAgentId) {
        throw new Error('agentId is required');
    }
    const { generation, scopeKey } = nextAgentLoadToken();
    const mutationKey = scopedMutationKey(normalizedAgentId, scopeKey);

    beginMutation(mutationKey);
    setAgentError(null);

    const snapshot = get(agentMap).get(normalizedAgentId);
    updateAgentSummary(normalizedAgentId, (existing) => ({
        ...(existing || { agent_id: normalizedAgentId }),
        agent_id: normalizedAgentId,
        status: 'triggered',
        current_goal_id: request.goal_id || existing?.current_goal_id,
        current_cycle_id: undefined,
        current_execution_id: undefined,
        last_trigger: request.trigger || existing?.last_trigger || 'manual',
        updated_at: existing?.updated_at ?? 0
    }));

    try {
        const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}/trigger`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(request)
        });
        await expectOk(response);

        const payload = asRecord(await response.json() as unknown) || {};
        const result: TriggerAgentResponse = {
            agent_id: readString(payload, 'agent_id') || normalizedAgentId,
            goal_id: readString(payload, 'goal_id') || request.goal_id || '',
            trigger: readString(payload, 'trigger') || request.trigger || 'manual',
            trigger_seq: readNumber(payload, 'trigger_seq') ?? 0,
            cycle_id: readString(payload, 'cycle_id') || '',
            execution_id: readString(payload, 'execution_id') || undefined,
            definition_version: readNumber(payload, 'definition_version') ?? snapshot?.version ?? 0,
            status: readString(payload, 'status') || 'triggered',
            queue_position: readNumber(payload, 'queue_position')
        };
        if (isStaleAgentLoad(generation, scopeKey)) {
            return result;
        }

        updateAgentSummary(normalizedAgentId, (existing) => ({
            ...(existing || { agent_id: normalizedAgentId }),
            agent_id: normalizedAgentId,
            status: statusFromApiStatus(result.status, 'triggered'),
            current_goal_id: result.goal_id || existing?.current_goal_id,
            current_cycle_id: result.cycle_id || existing?.current_cycle_id,
            current_execution_id: result.execution_id || existing?.current_execution_id,
            last_trigger: result.trigger || existing?.last_trigger,
            updated_at: existing?.updated_at ?? 0
        }));

        return result;
    } catch (error) {
        if (!isStaleAgentLoad(generation, scopeKey)) {
            restoreAgentSnapshot(normalizedAgentId, snapshot);
            const message = error instanceof Error ? error.message : 'Failed to trigger agent';
            setAgentError(message);
        }
        throw error;
    } finally {
        endMutation(mutationKey);
    }
}

function parseHarnessRuntimeStatus(payload: unknown): HarnessRuntimeStatus {
    const record = asRecord(payload) || {};
    const enabled = readBoolean(record, 'enabled');
    const paused = readBoolean(record, 'paused');
    if (enabled == null || paused == null) {
        throw new Error('Malformed harness runtime status');
    }
    return {
        enabled,
        paused,
        configured_paused: readBoolean(record, 'configured_paused') ?? null,
        env_override: readBoolean(record, 'env_override') ?? null,
        effective_source: readString(record, 'effective_source') || 'config_error',
        config_error: readString(record, 'config_error')
    };
}

export async function loadHarnessRuntimeStatus(): Promise<HarnessRuntimeStatus> {
    const response = await timedFetch('/api/magician/v2/harness/runtime');
    await expectOk(response);
    return parseHarnessRuntimeStatus(await response.json() as unknown);
}

export async function setHarnessRuntimeEnabled(
    enabled: boolean
): Promise<HarnessRuntimeUpdateResponse> {
    const response = await timedFetch('/api/magician/v2/harness/runtime', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled })
    });
    await expectOk(response);
    const payload = await response.json() as unknown;
    const record = asRecord(payload) || {};
    return {
        ...parseHarnessRuntimeStatus(payload),
        changed: readBoolean(record, 'changed') ?? false,
        env_override_cleared: readBoolean(record, 'env_override_cleared') ?? false,
        harness_agents_seen: readNumber(record, 'harness_agents_seen') ?? 0,
        active_cycles_targeted: readNumber(record, 'active_cycles_targeted') ?? 0,
        pending_dispatches_cleared: readNumber(record, 'pending_dispatches_cleared') ?? 0,
        warnings: readStringArray(record, 'warnings')
    };
}

/**
 * Pause an agent.
 */
export async function pauseAgent(agentId: string): Promise<AgentPauseResumeResponse> {
    const normalizedAgentId = agentId.trim();
    if (!normalizedAgentId) {
        throw new Error('agentId is required');
    }
    const { generation, scopeKey } = nextAgentLoadToken();
    const mutationKey = scopedMutationKey(normalizedAgentId, scopeKey);

    beginMutation(mutationKey);
    setAgentError(null);

    const snapshot = get(agentMap).get(normalizedAgentId);
    updateAgentSummary(normalizedAgentId, (existing) => ({
        ...(existing || { agent_id: normalizedAgentId }),
        agent_id: normalizedAgentId,
        status: 'paused',
        current_goal_id: undefined,
        current_cycle_id: undefined,
        current_execution_id: undefined,
        updated_at: existing?.updated_at ?? 0
    }));

    try {
        const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}/pause`, {
            method: 'POST'
        });
        await expectOk(response);

        const payload = asRecord(await response.json() as unknown) || {};
        const result: AgentPauseResumeResponse = {
            agent_id: readString(payload, 'agent_id') || normalizedAgentId,
            status: readString(payload, 'status') || 'paused',
            changed: readBoolean(payload, 'changed') ?? true
        };
        if (isStaleAgentLoad(generation, scopeKey)) {
            return result;
        }

        updateAgentSummary(normalizedAgentId, (existing) => ({
            ...(existing || { agent_id: normalizedAgentId }),
            agent_id: normalizedAgentId,
            status: statusFromApiStatus(result.status, 'paused'),
            current_goal_id: undefined,
            current_cycle_id: undefined,
            current_execution_id: undefined,
            updated_at: existing?.updated_at ?? 0
        }));

        return result;
    } catch (error) {
        if (!isStaleAgentLoad(generation, scopeKey)) {
            restoreAgentSnapshot(normalizedAgentId, snapshot);
            const message = error instanceof Error ? error.message : 'Failed to pause agent';
            setAgentError(message);
        }
        throw error;
    } finally {
        endMutation(mutationKey);
    }
}

/**
 * Resume an agent.
 */
export async function resumeAgent(agentId: string): Promise<AgentPauseResumeResponse> {
    const normalizedAgentId = agentId.trim();
    if (!normalizedAgentId) {
        throw new Error('agentId is required');
    }
    const { generation, scopeKey } = nextAgentLoadToken();
    const mutationKey = scopedMutationKey(normalizedAgentId, scopeKey);

    beginMutation(mutationKey);
    setAgentError(null);

    const snapshot = get(agentMap).get(normalizedAgentId);
    updateAgentSummary(normalizedAgentId, (existing) => ({
        ...(existing || { agent_id: normalizedAgentId }),
        agent_id: normalizedAgentId,
        status: 'idle',
        current_goal_id: undefined,
        current_cycle_id: undefined,
        current_execution_id: undefined,
        updated_at: existing?.updated_at ?? 0
    }));

    try {
        const response = await timedFetch(`/api/magician/v2/agents/${encodeURIComponent(normalizedAgentId)}/resume`, {
            method: 'POST'
        });
        await expectOk(response);

        const payload = asRecord(await response.json() as unknown) || {};
        const result: AgentPauseResumeResponse = {
            agent_id: readString(payload, 'agent_id') || normalizedAgentId,
            status: readString(payload, 'status') || 'idle',
            changed: readBoolean(payload, 'changed') ?? true
        };
        if (isStaleAgentLoad(generation, scopeKey)) {
            return result;
        }

        updateAgentSummary(normalizedAgentId, (existing) => ({
            ...(existing || { agent_id: normalizedAgentId }),
            agent_id: normalizedAgentId,
            status: statusFromApiStatus(result.status, 'idle'),
            current_goal_id: undefined,
            current_cycle_id: undefined,
            current_execution_id: undefined,
            updated_at: existing?.updated_at ?? 0
        }));

        return result;
    } catch (error) {
        if (!isStaleAgentLoad(generation, scopeKey)) {
            restoreAgentSnapshot(normalizedAgentId, snapshot);
            const message = error instanceof Error ? error.message : 'Failed to resume agent';
            setAgentError(message);
        }
        throw error;
    } finally {
        endMutation(mutationKey);
    }
}

/**
 * Clear recoverable error state.
 */
export function clearAgentError(): void {
    setAgentError(null);
}

/**
 * Reset store state (e.g., disconnect, logout).
 */
export function clearAgents(): void {
    agentLoadGeneration += 1;
    clearAgentsForScopeChange();
}

if (browser) {
    startAgentScopeBridge();
}
