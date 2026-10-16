/**
 * Approval Store - REST + realtime state for approval queue and resolution actions.
 */

import { browser } from '$app/environment';
import { writable, derived, get } from 'svelte/store';
import type { AgentEventEnvelope } from '$lib/realtime/v2-websocket';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { timedFetch } from '$lib/shared/fetch';
import type { HitlOpenTarget } from '$lib/hitl/types';

// =============================================================================
// Types
// =============================================================================

export type ApprovalStatus =
    | 'pending'
    | 'validating'
    | 'approved'
    | 'rejected'
    | 'expired'
    | 'resolved';

export type ApprovalDecision = 'approve' | 'reject';

export interface ApprovalDeliverySummary {
    delivery_id: string;
    channel: string;
    status: string;
    delivered_at?: number;
}

export interface ApprovalSummary {
    approval_id: string;
    principal?: string;
    workspace?: string;
    agent_id: string;
    goal_id: string;
    cycle_id: string;
    execution_id?: string;
    trigger_seq: number;
    status: ApprovalStatus;
    created_at: number;
    expires_at: number;
    updated_at: number;
    resolved_at?: number;
    resolved_by?: string;
    plan_hash?: string;
    pending_action_count: number;
    pending_actions?: unknown[];
    deliveries?: ApprovalDeliverySummary[];
}

export interface ApprovalStoreState {
    isLoading: boolean;
    error: string | null;
    totalCount: number;
    lastLoadedAt: number | null;
    resolvingCount: number;
}

export interface LoadApprovalsOptions {
    status?: string;
    agent_id?: string;
    replace?: boolean;
    clearError?: boolean;
}

export interface ResolveApprovalResult {
    resolved: boolean;
    status: ApprovalStatus;
}

interface ApprovalMetaState {
    isLoading: boolean;
    error: string | null;
    totalCount: number;
    lastLoadedAt: number | null;
    resolving: Record<string, number>;
}

const APPROVAL_REQUESTED_EVENTS = new Set(['approval.requested', 'agent.approval.requested']);
const APPROVAL_RESOLVED_EVENTS = new Set(['approval.resolved', 'agent.approval.resolved']);
const APPROVAL_EXPIRED_EVENTS = new Set(['approval.expired', 'agent.approval.expired']);

// Phase H5.3 — canonical-event types the store handles in parallel
// with the legacy approval.requested / .resolved / .expired family.
// `source: "approval"` on the canonical envelope is the dedup key —
// approval_id == correlation_id by construction in the backend emit
// (web_api.rs::emit_approval_requested_event), so dual-write collapses
// onto the same Map entry. When H6.4 drops the legacy emits, this
// becomes the sole reader.
const CANONICAL_HITL_REQUESTED_TYPE = 'HitlRequested';
const CANONICAL_HITL_RESOLVED_TYPE = 'HitlResolved';

const APPROVAL_SORT_PRIORITY: Record<ApprovalStatus, number> = {
    pending: 0,
    validating: 1,
    approved: 2,
    rejected: 3,
    expired: 4,
    resolved: 5
};
const TERMINAL_APPROVAL_STATUSES = new Set<ApprovalStatus>([
    'approved',
    'rejected',
    'expired',
    'resolved'
]);

const defaultMetaState: ApprovalMetaState = {
    isLoading: false,
    error: null,
    totalCount: 0,
    lastLoadedAt: null,
    resolving: {}
};

let approvalLoadGeneration = 0;
let approvalScopeUnsubscribe: (() => void) | null = null;
let lastApprovalScopeKey = '';

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

function normalizeTimestampValue(value: unknown): number | undefined {
    if (typeof value === 'number' && Number.isFinite(value)) {
        return value < 1_000_000_000_000 ? value * 1000 : value;
    }
    if (typeof value === 'string' && value.trim().length > 0) {
        const parsed = Date.parse(value);
        return Number.isFinite(parsed) ? parsed : undefined;
    }
    return undefined;
}

function normalizeStatus(raw: string | undefined, fallback: ApprovalStatus): ApprovalStatus {
    if (!raw) return fallback;
    const normalized = raw.trim().toLowerCase().replace(/[\s-]/g, '_');
    if (normalized === 'pending') return 'pending';
    if (normalized === 'validating') return 'validating';
    if (normalized === 'approved' || normalized === 'approve') return 'approved';
    if (normalized === 'rejected' || normalized === 'reject') return 'rejected';
    if (normalized === 'expired') return 'expired';
    if (normalized === 'resolved') return 'resolved';
    return fallback;
}

function parseDeliveries(raw: unknown): ApprovalDeliverySummary[] {
    if (!Array.isArray(raw)) return [];

    const deliveries: ApprovalDeliverySummary[] = [];
    for (const entry of raw) {
        const record = asRecord(entry);
        if (!record) continue;
        const deliveryId = readString(record, 'delivery_id');
        const channel = readString(record, 'channel');
        if (!deliveryId || !channel) continue;

        deliveries.push({
            delivery_id: deliveryId,
            channel,
            status: readString(record, 'status') || 'unknown',
            delivered_at: normalizeTimestampValue(record.delivered_at)
        });
    }

    return deliveries;
}

function readStringArray(record: Record<string, unknown>, key: string): string[] {
    const value = record[key];
    if (!Array.isArray(value)) return [];
    return value
        .map((entry) => (typeof entry === 'string' ? entry.trim() : ''))
        .filter((entry) => entry.length > 0);
}

function parseApprovalRequest(raw: unknown): ApprovalSummary | null {
    const record = asRecord(raw);
    if (!record) return null;

    const approvalId = readString(record, 'approval_id');
    const agentId = readString(record, 'agent_id');
    const goalId = readString(record, 'goal_id');
    const cycleId = readString(record, 'cycle_id');
    if (!approvalId || !agentId || !goalId || !cycleId) return null;

    const pendingActions = Array.isArray(record.pending_actions) ? record.pending_actions : [];
    const createdAt = normalizeTimestampValue(record.created_at) ?? Date.now();
    const resolvedAt = normalizeTimestampValue(record.resolved_at);

    return {
        approval_id: approvalId,
        principal: readString(record, 'principal'),
        workspace: readString(record, 'workspace'),
        agent_id: agentId,
        goal_id: goalId,
        cycle_id: cycleId,
        execution_id: readString(record, 'execution_id'),
        trigger_seq: readNumber(record, 'trigger_seq') ?? 0,
        status: normalizeStatus(readString(record, 'status'), 'pending'),
        created_at: createdAt,
        expires_at: normalizeTimestampValue(record.expires_at) ?? createdAt,
        updated_at: resolvedAt ?? createdAt,
        resolved_at: resolvedAt,
        resolved_by: readString(record, 'resolved_by'),
        plan_hash: readString(record, 'plan_hash'),
        pending_action_count: pendingActions.length,
        pending_actions: pendingActions
    };
}

/** Build the complete direct-open contract from an approval REST record. */
export function approvalHitlOpenTarget(approval: ApprovalSummary): HitlOpenTarget {
    const count = approval.pending_action_count;
    const activeScope = get(scopeIdentityStore);
    const actionDescriptions = (approval.pending_actions ?? [])
        .map((action) => {
            if (typeof action === 'string') return action.trim();
            const record = asRecord(action);
            if (!record) return '';
            return readString(record, 'action_description') ?? readString(record, 'step_id') ?? '';
        })
        .filter((description) => description.length > 0)
        .slice(0, 5)
        .map((description) => description.slice(0, 300));
    const summary = count > 0
        ? `Approval required for ${count} pending action${count === 1 ? '' : 's'}`
        : 'Approval required before the agent can continue';
    return {
        id: approval.approval_id,
        source: 'approval',
        input_type: 'confirmation',
        prompt: actionDescriptions.length > 0
            ? `${summary}\n\nPending actions:\n- ${actionDescriptions.join('\n- ')}`
            : summary,
        input_schema: {
            confirm_label: 'Approve',
            deny_label: 'Reject'
        },
        identifiers: {
            approval_id: approval.approval_id,
            correlation_id: approval.approval_id
        },
        scope: {
            principal: approval.principal ?? activeScope.principal,
            workspace: approval.workspace ?? activeScope.workspace,
            execution_id: approval.execution_id,
            agent_id: approval.agent_id
        },
        at: approval.updated_at
    };
}

function shouldApplyApprovalUpdate(
    existing: ApprovalSummary | undefined,
    nextStatus: ApprovalStatus,
    updatedAt: number
): boolean {
    if (!existing) return true;
    if (updatedAt < existing.updated_at) return false;

    // Approval IDs are immutable one-shot workflows. Once terminal, do not
    // allow realtime events to rewrite status into a different state.
    if (TERMINAL_APPROVAL_STATUSES.has(existing.status) && existing.status !== nextStatus) {
        return false;
    }

    return true;
}

function shouldApplyApprovalEnvelopeUpdate(
    existing: ApprovalSummary | undefined,
    nextStatus: ApprovalStatus,
    updatedAt: number
): boolean {
    return shouldApplyApprovalUpdate(existing, nextStatus, updatedAt);
}

function defaultApprovalSummary(
    approvalId: string,
    agentId: string,
    updatedAt: number
): ApprovalSummary {
    return {
        approval_id: approvalId,
        agent_id: agentId,
        goal_id: '',
        cycle_id: '',
        trigger_seq: 0,
        status: 'pending',
        created_at: updatedAt,
        expires_at: updatedAt,
        updated_at: updatedAt,
        pending_action_count: 0
    };
}

function mergeApprovalSummary(nextSummary: ApprovalSummary): void {
    approvalMap.update((map) => {
        const next = new Map(map);
        const existing = next.get(nextSummary.approval_id);
        if (!shouldApplyApprovalUpdate(existing, nextSummary.status, nextSummary.updated_at)) {
            return map;
        }

        next.set(nextSummary.approval_id, {
            ...(existing || defaultApprovalSummary(nextSummary.approval_id, nextSummary.agent_id, nextSummary.updated_at)),
            ...nextSummary,
            deliveries: nextSummary.deliveries ?? existing?.deliveries,
            pending_actions: nextSummary.pending_actions ?? existing?.pending_actions,
            updated_at: Math.max(existing?.updated_at ?? 0, nextSummary.updated_at)
        });

        return next;
    });
}

function setError(error: string | null): void {
    approvalMeta.update((state) => ({
        ...state,
        error
    }));
}

function beginResolve(approvalId: string): void {
    approvalMeta.update((state) => {
        const next = { ...state.resolving };
        next[approvalId] = (next[approvalId] || 0) + 1;
        return {
            ...state,
            resolving: next
        };
    });
}

function endResolve(approvalId: string): void {
    approvalMeta.update((state) => {
        const next = { ...state.resolving };
        if (!next[approvalId]) return state;

        if (next[approvalId] <= 1) {
            delete next[approvalId];
        } else {
            next[approvalId] -= 1;
        }

        return {
            ...state,
            resolving: next
        };
    });
}

function syncTotalCountWithMap(): void {
    approvalMeta.update((state) => ({
        ...state,
        totalCount: get(approvalMap).size
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

// =============================================================================
// Store
// =============================================================================

const approvalMap = writable<Map<string, ApprovalSummary>>(new Map());
const approvalMeta = writable<ApprovalMetaState>(defaultMetaState);

function currentApprovalScopeKey(): string {
    const scope = get(scopeIdentityStore);
    return `${scope.principal}:${scope.workspace}`;
}

function nextApprovalLoadToken(): { generation: number; scopeKey: string } {
    return {
        generation: approvalLoadGeneration,
        scopeKey: currentApprovalScopeKey()
    };
}

function isStaleApprovalLoad(generation: number, scopeKey: string): boolean {
    return generation !== approvalLoadGeneration || currentApprovalScopeKey() !== scopeKey;
}

function clearApprovalsForScopeChange(): void {
    approvalMap.set(new Map());
    approvalMeta.set({
        ...defaultMetaState,
        totalCount: 0
    });
}

function startApprovalScopeBridge(): void {
    if (approvalScopeUnsubscribe || !browser) return;
    const currentScope = get(scopeIdentityStore);
    lastApprovalScopeKey = `${currentScope.principal}:${currentScope.workspace}`;
    approvalScopeUnsubscribe = scopeIdentityStore.subscribe((scope) => {
        const scopeKey = `${scope.principal}:${scope.workspace}`;
        if (scopeKey === lastApprovalScopeKey) return;
        lastApprovalScopeKey = scopeKey;
        approvalLoadGeneration += 1;
        clearApprovalsForScopeChange();
    });
}

if (browser) {
    startApprovalScopeBridge();
}

export const approvalList = derived(approvalMap, ($map) =>
    Array.from($map.values()).sort((a, b) => {
        const priorityDelta = APPROVAL_SORT_PRIORITY[a.status] - APPROVAL_SORT_PRIORITY[b.status];
        if (priorityDelta !== 0) return priorityDelta;
        return b.updated_at - a.updated_at;
    })
);

export const pendingApprovals = derived(approvalList, ($approvals) =>
    $approvals.filter((approval) => approval.status === 'pending' || approval.status === 'validating')
);

export const approvalsByAgent = derived(approvalList, ($approvals) => {
    const grouped: Record<string, ApprovalSummary[]> = {};
    for (const approval of $approvals) {
        if (!grouped[approval.agent_id]) {
            grouped[approval.agent_id] = [];
        }
        grouped[approval.agent_id].push(approval);
    }
    return grouped;
});

export const approvalCountsByStatus = derived(approvalMap, ($map) => {
    const counts: Record<ApprovalStatus, number> = {
        pending: 0,
        validating: 0,
        approved: 0,
        rejected: 0,
        expired: 0,
        resolved: 0
    };

    for (const approval of $map.values()) {
        counts[approval.status] += 1;
    }

    return counts;
});

export const approvalStoreState = derived(approvalMeta, ($meta): ApprovalStoreState => ({
    isLoading: $meta.isLoading,
    error: $meta.error,
    totalCount: $meta.totalCount,
    lastLoadedAt: $meta.lastLoadedAt,
    resolvingCount: Object.values($meta.resolving).reduce((sum, count) => sum + count, 0)
}));

export function getApprovalSnapshot(approvalId: string): ApprovalSummary | undefined {
    return get(approvalMap).get(approvalId);
}

export function isApprovalResolving(approvalId: string): boolean {
    return Boolean(get(approvalMeta).resolving[approvalId]);
}

// =============================================================================
// Realtime Event Merge
// =============================================================================

/**
 * Merge approval lifecycle events emitted through the generic AgentEvent envelope.
 */
export function handleApprovalEnvelopeEvent(envelope: AgentEventEnvelope): void {
    if (browser) startApprovalScopeBridge();
    if (!APPROVAL_REQUESTED_EVENTS.has(envelope.event_type)
        && !APPROVAL_RESOLVED_EVENTS.has(envelope.event_type)
        && !APPROVAL_EXPIRED_EVENTS.has(envelope.event_type)) {
        return;
    }
    if (envelope.principal && envelope.workspace) {
        const scope = get(scopeIdentityStore);
        if (envelope.principal !== scope.principal || envelope.workspace !== scope.workspace) {
            return;
        }
    }

    const payload = asRecord(envelope.payload) || {};
    const approvalId = readString(payload, 'approval_id');
    if (!approvalId) return;

    const updatedAt = normalizeTimestampValue(envelope.timestamp) ?? Date.now();

    approvalMap.update((map) => {
        const next = new Map(map);
        const existing = next.get(approvalId)
            || defaultApprovalSummary(approvalId, envelope.agent_id, updatedAt);

        if (APPROVAL_REQUESTED_EVENTS.has(envelope.event_type)) {
            if (!shouldApplyApprovalEnvelopeUpdate(existing, 'pending', updatedAt)) {
                return next;
            }
            const pendingActionCount = readNumber(payload, 'pending_action_count')
                ?? existing.pending_action_count;
            next.set(approvalId, {
                ...existing,
                approval_id: approvalId,
                principal: envelope.principal || existing.principal,
                workspace: envelope.workspace || existing.workspace,
                agent_id: envelope.agent_id,
                execution_id: readString(payload, 'execution_id') || existing.execution_id,
                goal_id: readString(payload, 'goal_id') || existing.goal_id,
                cycle_id: readString(payload, 'cycle_id') || existing.cycle_id,
                trigger_seq: readNumber(payload, 'trigger_seq') ?? existing.trigger_seq,
                status: 'pending',
                expires_at: normalizeTimestampValue(payload.expires_at) ?? existing.expires_at,
                pending_action_count: pendingActionCount,
                updated_at: updatedAt
            });
            return next;
        }

        if (APPROVAL_RESOLVED_EVENTS.has(envelope.event_type)) {
            const decision = readString(payload, 'decision');
            const resolvedStatusBase: ApprovalStatus =
                decision === 'approve' || decision === 'approved'
                    ? 'approved'
                    : decision === 'reject' || decision === 'rejected'
                        ? 'rejected'
                        : 'resolved';
            const resolvedStatus: ApprovalStatus =
                resolvedStatusBase === 'resolved'
                    && TERMINAL_APPROVAL_STATUSES.has(existing.status)
                    ? existing.status
                    : resolvedStatusBase;

            const resolvedAt = normalizeTimestampValue(payload.resolved_at) ?? updatedAt;
            if (!shouldApplyApprovalEnvelopeUpdate(existing, resolvedStatus, resolvedAt)) {
                return next;
            }
            next.set(approvalId, {
                ...existing,
                approval_id: approvalId,
                agent_id: envelope.agent_id,
                goal_id: readString(payload, 'goal_id') || existing.goal_id,
                cycle_id: readString(payload, 'cycle_id') || existing.cycle_id,
                trigger_seq: readNumber(payload, 'trigger_seq') ?? existing.trigger_seq,
                status: resolvedStatus,
                resolved_by: readString(payload, 'resolved_by')
                    || readString(payload, 'source')
                    || existing.resolved_by,
                resolved_at: resolvedAt,
                updated_at: resolvedAt
            });
            return next;
        }

        const resolvedAt = normalizeTimestampValue(payload.resolved_at) ?? existing.resolved_at;
        if (!shouldApplyApprovalEnvelopeUpdate(existing, 'expired', updatedAt)) {
            return next;
        }
        next.set(approvalId, {
            ...existing,
            approval_id: approvalId,
            agent_id: envelope.agent_id,
            goal_id: readString(payload, 'goal_id') || existing.goal_id,
            cycle_id: readString(payload, 'cycle_id') || existing.cycle_id,
            trigger_seq: readNumber(payload, 'trigger_seq') ?? existing.trigger_seq,
            status: 'expired',
            resolved_by: readString(payload, 'resolved_by') || existing.resolved_by,
            resolved_at: resolvedAt,
            updated_at: updatedAt
        });
        return next;
    });

    syncTotalCountWithMap();
}

/**
 * Phase H5.3 — canonical-envelope handler.
 *
 * Consumes `HitlRequested` / `HitlResolved` typed events emitted as
 * top-level `RuntimeTransportEvent` variants (NOT the AgentEvent
 * envelope path that `handleApprovalEnvelopeEvent` handles). Filters
 * to `source: "approval"` and updates the same `approvalMap`. Keyed
 * on `correlation_id` (= `approval_id` by backend construction in
 * `web_api.rs::emit_approval_requested_event`), so dual-emit with
 * the legacy `approval.requested` event collapses onto a single
 * approvalMap entry.
 *
 * Source-specific ride-along data (`goal_id`, `cycle_id`,
 * `trigger_seq`, `pending_action_count`, `expires_at`) arrives on
 * `data.input_schema` per the H5.3 backend extension. When H6.4
 * drops the legacy `emit_agent_event("approval.requested", …)` call,
 * this handler becomes the sole live updater and the `APPROVAL_*_EVENTS`
 * legacy reader can be removed.
 */
export function handleCanonicalHitlApprovalEvent(
    eventType: string,
    data: Record<string, unknown>
): void {
    if (eventType !== CANONICAL_HITL_REQUESTED_TYPE
        && eventType !== CANONICAL_HITL_RESOLVED_TYPE) {
        return;
    }
    const source = readString(data, 'source');
    if (source !== 'approval') return;

    if (browser) startApprovalScopeBridge();

    // Scope guard — drop events outside the active scope.
    const envelopePrincipal = readString(data, 'principal');
    const envelopeWorkspace = readString(data, 'workspace');
    if (envelopePrincipal && envelopeWorkspace) {
        const scope = get(scopeIdentityStore);
        if (envelopePrincipal !== scope.principal || envelopeWorkspace !== scope.workspace) {
            return;
        }
    }

    const correlationId = readString(data, 'correlation_id');
    if (!correlationId) return;
    const agentId = readString(data, 'agent_id') ?? '';
    const updatedAt = normalizeTimestampValue(data.timestamp) ?? Date.now();
    const schema = asRecord(data.input_schema) ?? {};

    approvalMap.update((map) => {
        const next = new Map(map);
        const existing = next.get(correlationId)
            || defaultApprovalSummary(correlationId, agentId, updatedAt);

        if (eventType === CANONICAL_HITL_REQUESTED_TYPE) {
            if (!shouldApplyApprovalEnvelopeUpdate(existing, 'pending', updatedAt)) {
                return next;
            }
            const pendingActionCount = readNumber(schema, 'pending_action_count')
                ?? existing.pending_action_count;
            const pendingActionDescriptions = readStringArray(
                schema,
                'pending_action_descriptions'
            );
            next.set(correlationId, {
                ...existing,
                approval_id: correlationId,
                principal: envelopePrincipal ?? existing.principal,
                workspace: envelopeWorkspace ?? existing.workspace,
                agent_id: agentId || existing.agent_id,
                execution_id: readString(data, 'execution_id') || existing.execution_id,
                goal_id: readString(schema, 'goal_id') || existing.goal_id,
                cycle_id: readString(schema, 'cycle_id') || existing.cycle_id,
                trigger_seq: readNumber(schema, 'trigger_seq') ?? existing.trigger_seq,
                status: 'pending',
                expires_at: normalizeTimestampValue(schema.expires_at) ?? existing.expires_at,
                pending_action_count: pendingActionCount,
                pending_actions:
                    pendingActionDescriptions.length > 0
                        ? pendingActionDescriptions
                        : existing.pending_actions,
                updated_at: updatedAt
            });
            return next;
        }

        // Resolution — outcome maps to status:
        //   outcome: "responded" + decision: "approve" → approved
        //   outcome: "responded" + decision: "reject"  → rejected
        //   outcome: "expired"                          → expired
        //   anything else                               → resolved (terminal)
        const outcome = readString(data, 'outcome') ?? 'responded';
        const decision = readString(data, 'decision');
        const resolvedStatusBase: ApprovalStatus =
            outcome === 'expired'
                ? 'expired'
                : decision === 'approve' || decision === 'approved'
                    ? 'approved'
                    : decision === 'reject' || decision === 'rejected'
                        ? 'rejected'
                        : 'resolved';
        const resolvedStatus: ApprovalStatus =
            resolvedStatusBase === 'resolved'
                && TERMINAL_APPROVAL_STATUSES.has(existing.status)
                ? existing.status
                : resolvedStatusBase;

        if (!shouldApplyApprovalEnvelopeUpdate(existing, resolvedStatus, updatedAt)) {
            return next;
        }
        next.set(correlationId, {
            ...existing,
            approval_id: correlationId,
            agent_id: agentId || existing.agent_id,
            status: resolvedStatus,
            // `resolved_by` doesn't ride on canonical HitlResolved today —
            // fall back to the legacy value if present, else mark the
            // source so the UI shows "approval channel" rather than
            // blank. The REST `/api/v2/approvals/{id}` refetch on
            // page-view fills in the precise resolver.
            resolved_by: existing.resolved_by || source,
            resolved_at: updatedAt,
            updated_at: updatedAt
        });
        return next;
    });

    syncTotalCountWithMap();
}

// =============================================================================
// REST Hydration + Mutations
// =============================================================================

export async function loadApprovals(options: LoadApprovalsOptions = {}): Promise<ApprovalSummary[]> {
    if (browser) startApprovalScopeBridge();
    const { generation, scopeKey } = nextApprovalLoadToken();
    const replace = options.replace ?? (!options.status && !options.agent_id);
    const shouldClearError = options.clearError ?? true;

    if (shouldClearError) {
        setError(null);
    }

    approvalMeta.update((state) => ({
        ...state,
        isLoading: true
    }));

    try {
        const params = new URLSearchParams();
        if (options.status) params.set('status', options.status);
        if (options.agent_id) params.set('agent_id', options.agent_id);

        const query = params.toString();
        const url = query.length > 0
            ? `/api/magician/v2/approvals?${query}`
            : '/api/magician/v2/approvals';

        const response = await timedFetch(url);
        await expectOk(response);

        const payload = await response.json() as unknown;
        const root = asRecord(payload) || {};
        const rawApprovals = Array.isArray(root.approvals) ? root.approvals : [];
        const parsedApprovals = rawApprovals
            .map((entry) => parseApprovalRequest(entry))
            .filter((entry): entry is ApprovalSummary => Boolean(entry));
        if (isStaleApprovalLoad(generation, scopeKey)) {
            return parsedApprovals;
        }

        approvalMap.update((map) => {
            const next = replace ? new Map<string, ApprovalSummary>() : new Map(map);
            for (const approval of parsedApprovals) {
                const existing = next.get(approval.approval_id) || map.get(approval.approval_id);
                if (!shouldApplyApprovalUpdate(existing, approval.status, approval.updated_at)) {
                    if (replace && existing) {
                        next.set(approval.approval_id, existing);
                    }
                    continue;
                }
                next.set(approval.approval_id, {
                    ...(existing || defaultApprovalSummary(approval.approval_id, approval.agent_id, approval.updated_at)),
                    ...approval,
                    deliveries: existing?.deliveries,
                    updated_at: Math.max(existing?.updated_at ?? 0, approval.updated_at)
                });
            }
            return next;
        });

        approvalMeta.update((state) => ({
            ...state,
            isLoading: false,
            error: null,
            lastLoadedAt: Date.now(),
            totalCount: readNumber(root, 'total_count') ?? get(approvalMap).size
        }));

        return parsedApprovals;
    } catch (error) {
        if (isStaleApprovalLoad(generation, scopeKey)) {
            return [];
        }
        const message = error instanceof Error ? error.message : 'Failed to load approvals';
        approvalMeta.update((state) => ({
            ...state,
            isLoading: false,
            error: message
        }));
        throw error;
    }
}

export async function refreshApprovals(): Promise<ApprovalSummary[]> {
    return loadApprovals({ replace: true, clearError: true });
}

export async function loadApprovalDetails(approvalId: string): Promise<ApprovalSummary | null> {
    if (browser) startApprovalScopeBridge();
    const { generation, scopeKey } = nextApprovalLoadToken();
    const normalizedApprovalId = approvalId.trim();
    if (!normalizedApprovalId) {
        throw new Error('approvalId is required');
    }

    setError(null);
    approvalMeta.update((state) => ({
        ...state,
        isLoading: true
    }));

	try {
		const response = await timedFetch(`/api/magician/v2/approvals/${encodeURIComponent(normalizedApprovalId)}`);
		if (isStaleApprovalLoad(generation, scopeKey)) {
			return null;
		}

		if (response.status === 404) {
			approvalMap.update((map) => {
				const next = new Map(map);
				next.delete(normalizedApprovalId);
                return next;
            });
            syncTotalCountWithMap();
            approvalMeta.update((state) => ({
                ...state,
                isLoading: false,
                lastLoadedAt: Date.now()
            }));
            return null;
        }

        await expectOk(response);
        const payload = await response.json() as unknown;
        const root = asRecord(payload) || {};

        const parsed = parseApprovalRequest(root.request);
        if (isStaleApprovalLoad(generation, scopeKey)) {
            return null;
        }
        if (!parsed) {
            throw new Error('Malformed approval details payload');
        }

        const deliveries = parseDeliveries(root.deliveries);
        mergeApprovalSummary({
            ...parsed,
            deliveries,
            updated_at: parsed.updated_at
        });

        approvalMeta.update((state) => ({
            ...state,
            isLoading: false,
            error: null,
            lastLoadedAt: Date.now(),
            totalCount: get(approvalMap).size
        }));

        return get(approvalMap).get(parsed.approval_id) || null;
    } catch (error) {
        if (isStaleApprovalLoad(generation, scopeKey)) {
            return null;
        }
        const message = error instanceof Error ? error.message : 'Failed to load approval details';
        approvalMeta.update((state) => ({
            ...state,
            isLoading: false,
            error: message
        }));
        throw error;
    }
}

export async function resolveApproval(
    approvalId: string,
    decision: ApprovalDecision,
    options?: { channel?: string }
): Promise<ResolveApprovalResult> {
    if (browser) startApprovalScopeBridge();
    const { generation, scopeKey } = nextApprovalLoadToken();
    const normalizedApprovalId = approvalId.trim();
    if (!normalizedApprovalId) {
        throw new Error('approvalId is required');
    }

    const channel = options?.channel?.trim() || 'ui';

    beginResolve(normalizedApprovalId);
    setError(null);

    const snapshot = get(approvalMap).get(normalizedApprovalId);
    const now = Date.now();
    const resolvedStatus: ApprovalStatus = decision === 'approve' ? 'approved' : 'rejected';
    const optimisticStatus: ApprovalStatus = 'validating';
    let optimisticUpdatedAt: number | null = null;

    if (snapshot && (snapshot.status === 'pending' || snapshot.status === 'validating')) {
        optimisticUpdatedAt = snapshot.updated_at;
        mergeApprovalSummary({
            ...snapshot,
            status: optimisticStatus,
            resolved_by: channel,
            resolved_at: undefined,
            updated_at: snapshot.updated_at
        });
    }

	try {
		// Phase H7.x — legacy `/approvals/{id}/resolve` is 410 Gone. Use
		// canonical `/api/magician/v2/hitl/{correlation_id}/respond` with
		// `source: "approval"` and a Choice value matching the backend's
		// approve/reject id set.
		const response = await timedFetch(
			`/api/magician/v2/hitl/${encodeURIComponent(normalizedApprovalId)}/respond`,
			{
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({
					source: 'approval',
					value: {
						type: 'choice',
						selected_id: decision === 'approve' ? 'approve' : 'reject'
					},
					channel
				})
			}
		);
		if (isStaleApprovalLoad(generation, scopeKey)) {
			return {
				resolved: false,
				status: get(approvalMap).get(normalizedApprovalId)?.status || optimisticStatus
			};
		}

		// `/hitl/{id}/respond` returns 404/409 when the approval is no
		// longer pending — surface as "already resolved elsewhere" instead
		// of clobbering local state with `expired`.
		if (response.status === 404 || response.status === 409) {
			const message = await readApiError(response);
			setError(message || 'Approval is no longer pending');
			try {
				await loadApprovalDetails(normalizedApprovalId);
			} catch {
				if (snapshot && optimisticUpdatedAt !== null) {
					mergeApprovalSummary({ ...snapshot, updated_at: optimisticUpdatedAt });
				}
			}
			return {
				resolved: false,
				status: get(approvalMap).get(normalizedApprovalId)?.status || 'resolved'
			};
		}

		await expectOk(response);
        if (isStaleApprovalLoad(generation, scopeKey)) {
            return {
                resolved: false,
                status: get(approvalMap).get(normalizedApprovalId)?.status || optimisticStatus
            };
        }

        const payload = asRecord(await response.json() as unknown) || {};
        const resolved = readBoolean(payload, 'accepted') ?? readBoolean(payload, 'resolved') ?? false;

        if (!resolved) {
            try {
                await loadApprovalDetails(normalizedApprovalId);
            } catch {
                // Best-effort reconciliation only.
            }
            return {
                resolved: false,
                status: get(approvalMap).get(normalizedApprovalId)?.status || 'resolved'
            };
        }

        try {
            const refreshed = await loadApprovalDetails(normalizedApprovalId);
            if (refreshed && TERMINAL_APPROVAL_STATUSES.has(refreshed.status)) {
                return {
                    resolved: true,
                    status: refreshed.status
                };
            }
        } catch {
            // Best-effort reconciliation only.
        }

        const latest = get(approvalMap).get(normalizedApprovalId) || snapshot;
        if (latest) {
            mergeApprovalSummary({
                ...latest,
                approval_id: normalizedApprovalId,
                status: resolvedStatus,
                resolved_by: channel,
                resolved_at: now,
                updated_at: latest.updated_at
            });
        }

        return {
            resolved: true,
            status: resolvedStatus
        };
    } catch (error) {
        if (isStaleApprovalLoad(generation, scopeKey)) {
            return {
                resolved: false,
                status: get(approvalMap).get(normalizedApprovalId)?.status || optimisticStatus
            };
        }
        if (snapshot && optimisticUpdatedAt !== null) {
            approvalMap.update((map) => {
                const current = map.get(normalizedApprovalId);
                if (!current) {
                    return map;
                }

                // Only rollback if our local optimistic shadow is still current.
                if (current.updated_at !== optimisticUpdatedAt
                    || current.status !== optimisticStatus
                    || current.resolved_by !== channel) {
                    return map;
                }

                const next = new Map(map);
                next.set(normalizedApprovalId, snapshot);
                return next;
            });
        }

        const message = error instanceof Error ? error.message : 'Failed to resolve approval';
        setError(message);
        throw error;
    } finally {
        endResolve(normalizedApprovalId);
    }
}

export function clearApprovalError(): void {
    setError(null);
}

export function clearApprovals(): void {
    approvalMap.set(new Map());
    approvalMeta.set({
        ...defaultMetaState,
        totalCount: 0
    });
}
