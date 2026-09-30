/**
 * MUIJ Store — Tracks per-agent MUIJ component state from WebSocket deltas.
 *
 * Subscribes to `agent.ui.delta` events via v2-websocket.ts, applies delta
 * operations (upsert / remove / reorder) to per-agent component state, and
 * handles snapshot recovery on reconnect.
 *
 * Types mirror Rust `gaui/muij.rs`.
 */

import { writable, derived } from 'svelte/store';
import type { AgentEventEnvelope } from '$lib/realtime/v2-websocket';

// =============================================================================
// Types (mirror Rust gaui/muij.rs)
// =============================================================================

export interface MuijComponent {
    id: string;
    component_type: string; // String, not enum — extensible
    label?: string;
    source?: string;
    query?: string;
    props: Record<string, unknown>;
    static_snapshot?: unknown;
    children?: MuijComponent[];
}

export interface MuijDocument {
    muij_version: string;
    agent_id: string;
    layout: MuijComponent[];
    generated_at: string;
}

export type MuijDelta =
    | { op: 'upsert'; component_id: string; data: Record<string, unknown> }
    | { op: 'remove'; component_id: string }
    | { op: 'reorder'; ids: string[] };

/** Interaction kinds emitted by MuijRenderer. */
export type MuijInteractionKind =
    | 'submit' | 'search' | 'change' | 'action'
    | 'confirm' | 'cancel' | 'dismiss' | 'close';

/** Event detail dispatched by MuijRenderer on component interactions. */
export interface MuijInteractionEventDetail {
    componentId: string;
    interaction: MuijInteractionKind;
    detail: Record<string, unknown>;
    sent: boolean;
}

export interface AgentMuijState {
    components: Map<string, MuijComponent>;
    order: string[]; // render order
}

interface SnapshotRequestDeltaTracker {
    upserts: Set<string>;
    removed: Set<string>;
    reorderSeen: boolean;
}

// =============================================================================
// Store
// =============================================================================

const muijMap = writable<Map<string, AgentMuijState>>(new Map());

/** Agent IDs that have received at least one delta. Used for reconnect. */
const trackedAgents: Set<string> = new Set();

/**
 * Agent IDs whose cycle just completed. Prevents late-arriving deltas
 * (from the emitter's async re-broadcast) from resurrecting cleared state.
 *
 * - Cleared for an agent when `onAgentCycleStarted()` fires for that agent.
 * - All entries cleared on `onDisconnect()` to prevent permanent silencing.
 * - Time-based eviction: entries older than 5 minutes are removed on each
 *   `clearAgentMuij()` call (R103/R323).
 */
// R103: Use a timestamp Map instead of a Set to allow age-based eviction
const recentlyCleared: Map<string, number> = new Map();
// R641: Track last clear timestamp separately from recentlyCleared, which gets
// reset by onAgentCycleStarted. This dedup guard prevents stale double-completion
// events (agent.cycle.completed + agent.cycle.paused for the same old cycle) from
// clearing a rapidly-started new cycle's state. 1s window — generous enough to
// catch stale duplicates, short enough not to block legitimate rapid cycles.
const lastClearAt: Map<string, number> = new Map();
const CLEAR_DEDUP_WINDOW_MS = 1000;

/**
 * Agent IDs that received a delta after the most recent snapshot request
 * was sent. When the snapshot response arrives, these agents use a merge
 * strategy (snapshot as base, delta-applied state overlaid) instead of
 * full replacement, preventing stale snapshots from overwriting fresher
 * delta-applied state.
 */
const freshSinceSnapshotRequest: Set<string> = new Set();
/** Per-agent deltas seen after snapshot request was sent. */
const deltasSinceSnapshotRequest: Map<string, SnapshotRequestDeltaTracker> = new Map();
/** Last known active cycle per agent, used to ignore stale completion events. */
const activeCycleByAgent: Map<string, string> = new Map();
/** R288: Agent IDs whose cycle started after the last snapshot request was sent.
 * Snapshots arriving for these agents are stale (from a prior cycle) and must be rejected. */
const snapshotInvalidatedByNewCycle: Set<string> = new Set();
/** R151: Timeout handle for snapshot request staleness guard (R142). */
let snapshotTimeoutId: ReturnType<typeof setTimeout> | null = null;

// =============================================================================
// Pure Functions
// =============================================================================

function asPropsObject(value: unknown): Record<string, unknown> {
    if (value != null && typeof value === 'object' && !Array.isArray(value)) {
        return value as Record<string, unknown>;
    }
    return {};
}

function asString(value: unknown, fallback: string = ''): string {
    if (typeof value === 'string') return value;
    if (typeof value === 'number' || typeof value === 'boolean' || typeof value === 'bigint') {
        return String(value);
    }
    return fallback;
}

function createSnapshotRequestDeltaTracker(): SnapshotRequestDeltaTracker {
    return {
        upserts: new Set(),
        removed: new Set(),
        reorderSeen: false
    };
}

/**
 * R262/R263/R264/R267/R269: Unified normalization for optional string fields (source, query).
 * - undefined: keep fallback (existing value for merge, undefined for new/snapshot)
 * - null: explicit clear → undefined (R262)
 * - string: use it; empty string → undefined for consistency (R264)
 * - non-string (number, boolean, object): reject → fallback (R269: matches Rust as_str())
 */
function normalizeOptionalStringField(value: unknown, fallback?: string): string | undefined {
    if (value === undefined) return fallback;
    if (value === null) return undefined;
    if (typeof value === 'string') return value.length > 0 ? value : undefined;
    return fallback;
}

function normalizeComponentId(value: unknown): string | undefined {
    if (typeof value !== 'string') return undefined;
    const trimmed = value.trim();
    return trimmed.length > 0 ? trimmed : undefined;
}

function isValidSnapshotDocument(value: unknown): value is MuijDocument {
    if (value == null || typeof value !== 'object' || Array.isArray(value)) return false;
    const rec = value as { layout?: unknown; muij_version?: unknown };
    // R306: Validate muij_version presence to defend against future schema breaks
    return Array.isArray(rec.layout) && typeof rec.muij_version === 'string';
}

function isValidChildComponent(child: unknown): child is MuijComponent {
    if (child == null || typeof child !== 'object' || Array.isArray(child)) return false;
    const rec = child as { id?: unknown };
    return typeof rec.id === 'string' && rec.id.trim().length > 0;
}

function normalizeChildren(value: unknown, depth: number = 0): MuijComponent[] | undefined {
    if (!Array.isArray(value)) return undefined;
    const seen = new Set<string>();
    const normalized: MuijComponent[] = [];
    for (const child of value) {
        if (!isValidChildComponent(child)) continue;
        const component = normalizeComponent(child, depth);
        // R281: deduplicate child IDs (first-wins) to protect nested keyed rendering.
        if (seen.has(component.id)) continue;
        seen.add(component.id);
        normalized.push(component);
    }
    return normalized;
}

/** R235: Leaf-level normalization — normalizes immediate child fields without recursing further. */
function normalizeChildrenLeaf(value: unknown): MuijComponent[] | undefined {
    if (!Array.isArray(value)) return undefined;
    const seen = new Set<string>();
    const normalized: MuijComponent[] = [];
    for (const child of value) {
        if (!isValidChildComponent(child)) continue;
        const component: MuijComponent = {
            ...child,
            // R467: Trim child IDs at depth cap to match normalizeComponent's canonicalization
            id: asString(child.id).trim(),
            // R252: Fallback to 'Unknown' — aligns with applyDelta() new-component path
            component_type: (typeof child.component_type === 'string' ? child.component_type : 'Unknown').trim(),
            label: asString(child.label),
            // R267: Normalize source/query at leaf level too
            source: normalizeOptionalStringField(child.source),
            query: normalizeOptionalStringField(child.query),
            props: asPropsObject(child.props)
        };
        // R281: deduplicate child IDs (first-wins) to protect nested keyed rendering.
        if (seen.has(component.id)) continue;
        seen.add(component.id);
        normalized.push(component);
    }
    return normalized;
}

function normalizeComponent(component: MuijComponent, depth: number = 0): MuijComponent {
    // R209/R233: Trim component_type; typeof guard prevents TypeError on non-string payloads
    // R252: Fallback to 'Unknown' — aligns with applyDelta() new-component path
    const componentType = (typeof component.component_type === 'string' ? component.component_type : 'Unknown').trim();
    // R208: Bound recursion to match MuijRenderer.MAX_DEPTH = 32
    // R235: At depth cap, still normalize children's own fields (trim, asPropsObject)
    const normalizedChildren = depth >= 32
        ? normalizeChildrenLeaf(component.children)
        : normalizeChildren(component.children, depth + 1);
    // R291: Always set children explicitly to prevent non-array values leaking via ...component spread
    // R285: Trim ID to match delta-path normalizeComponentId() canonicalization
    return {
        ...component,
        id: asString(component.id).trim(),
        component_type: componentType,
        label: asString(component.label),
        // R263: Normalize source/query — rejects non-string types from snapshots
        source: normalizeOptionalStringField(component.source),
        query: normalizeOptionalStringField(component.query),
        props: asPropsObject(component.props),
        children: normalizedChildren
    };
}

/**
 * Apply a single delta operation to the store state for a given agent.
 * Returns a new Map to trigger Svelte reactivity.
 */
export function applyDelta(
    state: Map<string, AgentMuijState>,
    agentId: string,
    delta: MuijDelta
): Map<string, AgentMuijState> {
    const agentState = state.get(agentId);
    // R277: Ignore remove/reorder for unseen agents to avoid ghost empty states.
    // R508: Return same reference to avoid triggering spurious Svelte derived-store notifications.
    if (!agentState && delta.op !== 'upsert') {
        return state;
    }
    // R121: Defensive copy — never mutate the live agentState reference
    const copied: AgentMuijState = agentState
        ? { components: new Map(agentState.components), order: [...agentState.order] }
        : { components: new Map<string, MuijComponent>(), order: [] };

    switch (delta.op) {
        case 'upsert': {
            const componentId = normalizeComponentId(delta.component_id);
            // R508: Return same reference on invalid component_id — no state change.
            if (!componentId) return state;
            // Backend sends delta.data as a partial MuijComponent shape:
            //   { component_type, label, props: { fill, ... }, source?, query?, static_snapshot? }
            // Extract the nested props object; fall back to empty if absent.
            const deltaProps = asPropsObject(delta.data.props);
            const normalizedChildren = normalizeChildren(delta.data.children);
            const existing = copied.components.get(componentId);
            if (existing) {
                // Merge: update top-level fields if present in data, merge nested props
                // R123: component_type mutation is intentionally excluded — type is immutable after creation
                copied.components.set(componentId, {
                    ...existing,
                    ...(delta.data.label !== undefined && { label: asString(delta.data.label, existing.label) }),
                    // R262/R264/R269: Use normalizeOptionalStringField — handles null-clear,
                    // empty-string consistency, and strict string-only acceptance.
                    ...(delta.data.source !== undefined && { source: normalizeOptionalStringField(delta.data.source, existing.source) }),
                    ...(delta.data.query !== undefined && { query: normalizeOptionalStringField(delta.data.query, existing.query) }),
                    // R398: Normalize null → undefined for static_snapshot (matches Rust Option<Value> semantics)
                    ...(delta.data.static_snapshot !== undefined && {
                        static_snapshot: delta.data.static_snapshot === null ? undefined : delta.data.static_snapshot
                    }),
                    // R61: propagate children from delta data when present
                    ...(normalizedChildren !== undefined && { children: normalizedChildren }),
                    props: { ...existing.props, ...deltaProps }
                });
            } else {
                // New component — construct from delta data
                // R234/R240: typeof guard + trim — matches normalizeComponent's pattern
                // R262/R264/R269: Use normalizeOptionalStringField for source/query
                copied.components.set(componentId, {
                    id: componentId,
                    component_type: (typeof delta.data.component_type === 'string' ? delta.data.component_type : 'Unknown').trim(),
                    label: asString(delta.data.label),
                    source: normalizeOptionalStringField(delta.data.source),
                    query: normalizeOptionalStringField(delta.data.query),
                    props: deltaProps,
                    // R398: Normalize null → undefined for static_snapshot
                    static_snapshot: delta.data.static_snapshot === null ? undefined : delta.data.static_snapshot,
                    // R61: propagate children from delta data when present
                    children: normalizedChildren
                });
                // Append to order if not already present
                if (!copied.order.includes(componentId)) {
                    copied.order = [...copied.order, componentId];
                }
            }
            break;
        }
        case 'remove': {
            const componentId = normalizeComponentId(delta.component_id);
            // R508: Return same reference on invalid component_id — no state change.
            if (!componentId) return state;
            copied.components.delete(componentId);
            copied.order = copied.order.filter(id => id !== componentId);
            break;
        }
        case 'reorder': {
            // R140: Reconcile order with components Map
            // R254: Deduplicate reorder IDs to avoid duplicate keyed renders.
            const seen = new Set<string>();
            const validIds: string[] = [];
            for (const id of delta.ids) {
                const normalizedId = normalizeComponentId(id);
                if (!normalizedId) continue;
                if (!copied.components.has(normalizedId) || seen.has(normalizedId)) continue;
                seen.add(normalizedId);
                validIds.push(normalizedId);
            }
            // Append any component IDs not in the new order
            for (const id of copied.components.keys()) {
                if (!seen.has(id)) {
                    seen.add(id);
                    validIds.push(id);
                }
            }
            copied.order = validIds;
            break;
        }
    }

    // R326: Clone state before mutating to avoid in-place mutation of the store's
    // current Map reference inside the writable.update() callback.
    const next = new Map(state);
    next.set(agentId, copied);
    return next;
}

/**
 * Replace full agent state from a MuijDocument snapshot.
 * Returns a new Map to trigger Svelte reactivity.
 */
export function applySnapshot(
    state: Map<string, AgentMuijState>,
    agentId: string,
    document: MuijDocument
): Map<string, AgentMuijState> {
    const components = new Map<string, MuijComponent>();
    const order: string[] = [];
    // R210: Deduplicate order — skip components with IDs already seen
    const seen = new Set<string>();

    for (const component of document.layout) {
        if (!isValidChildComponent(component)) continue;
        const normalized = normalizeComponent(component);
        // R239: First-wins for both Map and order — consistent dedup semantics
        if (!seen.has(normalized.id)) {
            seen.add(normalized.id);
            components.set(normalized.id, normalized);
            order.push(normalized.id);
        }
    }

    // R326: Clone before mutating
    const next = new Map(state);
    next.set(agentId, { components, order });
    return next;
}

/**
 * Merge a snapshot under existing delta-applied state. The snapshot provides
 * the canonical component list and ordering, but individual components that
 * already exist in the store (from fresher deltas) are preserved.
 */
export function mergeSnapshot(
    state: Map<string, AgentMuijState>,
    agentId: string,
    document: MuijDocument,
    touchedSinceRequest?: SnapshotRequestDeltaTracker
): Map<string, AgentMuijState> {
    const existing = state.get(agentId);
    // R138: If no existing state (e.g., after cycle clear), treat as full replacement
    // to prevent stale snapshot data from contaminating a fresh cycle.
    if (!existing || existing.components.size === 0) {
        return applySnapshot(state, agentId, document);
    }
    const components = new Map<string, MuijComponent>();
    let order: string[] = [];
    const upsertsSinceRequest = touchedSinceRequest?.upserts;
    const removedSinceRequest = touchedSinceRequest?.removed;
    // R210: Deduplicate order — skip components with IDs already seen
    const seen = new Set<string>();

    for (const component of document.layout) {
        if (!isValidChildComponent(component)) continue;
        const normalized = normalizeComponent(component);
        // R257: Remove deltas received after snapshot request must win over stale snapshot rows.
        if (removedSinceRequest?.has(normalized.id)) continue;
        // R239: First-wins — consistent dedup semantics for both Map and order
        if (!seen.has(normalized.id)) {
            seen.add(normalized.id);
            // Prefer existing only for IDs upserted after snapshot request.
            const fresher = upsertsSinceRequest?.has(normalized.id)
                ? existing.components.get(normalized.id)
                : undefined;
            components.set(normalized.id, fresher || normalized);
            order.push(normalized.id);
        }
    }

    // Preserve only components that were upserted after the snapshot request.
    // This avoids carrying stale leftovers from a prior cycle into a new snapshot.
    if (existing) {
        for (const [id, comp] of existing.components) {
            if (removedSinceRequest?.has(id)) continue;
            if (!components.has(id) && upsertsSinceRequest?.has(id)) {
                components.set(id, normalizeComponent(comp));
                if (!seen.has(id)) {
                    seen.add(id);
                    order.push(id);
                }
            }
        }
    }

    // R258: Reorder deltas received after snapshot request must win over stale snapshot order.
    if (touchedSinceRequest?.reorderSeen) {
        const mergedOrder: string[] = [];
        const orderSeen = new Set<string>();
        for (const id of existing.order) {
            if (!components.has(id) || orderSeen.has(id)) continue;
            orderSeen.add(id);
            mergedOrder.push(id);
        }
        for (const id of order) {
            if (!components.has(id) || orderSeen.has(id)) continue;
            orderSeen.add(id);
            mergedOrder.push(id);
        }
        order = mergedOrder;
    }

    // R326: Clone before mutating
    const next = new Map(state);
    next.set(agentId, { components, order });
    return next;
}

// =============================================================================
// Event Handlers (called from v2-websocket.ts)
// =============================================================================

/**
 * Handle a MUIJ delta event from `agent.ui.delta` AgentEvent envelope.
 */
export function handleMuijEvent(envelope: AgentEventEnvelope): void {
    if (envelope.event_type !== 'agent.ui.delta') return;

    const agentId = envelope.agent_id;

    // Reject late deltas from a cycle that already completed.
    // The emitter's async re-broadcast can deliver a delta after
    // AgentCycleCompleted has already cleared this agent's state.
    if (recentlyCleared.has(agentId)) return;

    // R698: Validate shape BEFORE casting to prevent typed access on unvalidated data
    const rawDelta = envelope.payload;
    if (!rawDelta || typeof rawDelta !== 'object' || typeof (rawDelta as Record<string, unknown>).op !== 'string') return;
    const delta = rawDelta as MuijDelta;
    // R275: Strict op validation — reject unknown ops before tracking/state updates.
    if (delta.op !== 'upsert' && delta.op !== 'remove' && delta.op !== 'reorder') return;
    // R204/R308: Guard against non-record delta.data (null/undefined/arrays).
    if (delta.op === 'upsert' && (delta.data == null || typeof delta.data !== 'object' || Array.isArray(delta.data))) return;
    if (delta.op === 'reorder' && !Array.isArray(delta.ids)) return;
    // R651: Cap reorder delta IDs to prevent UI thread blocking on pathological payloads
    if (delta.op === 'reorder' && delta.ids.length > 10_000) {
        console.warn(`[muijStore] reorder delta for agent ${agentId} has ${delta.ids.length} IDs — exceeds cap, dropping`);
        return;
    }
    // R283: reject/normalize whitespace-only component IDs on delta paths.
    let normalizedDelta: MuijDelta;
    if (delta.op === 'upsert') {
        const componentId = normalizeComponentId(delta.component_id);
        if (!componentId) return;
        normalizedDelta = { ...delta, component_id: componentId };
    } else if (delta.op === 'remove') {
        const componentId = normalizeComponentId(delta.component_id);
        if (!componentId) return;
        normalizedDelta = { ...delta, component_id: componentId };
    } else {
        normalizedDelta = delta;
    }

    const pendingDeltas = deltasSinceSnapshotRequest.get(agentId);
    if (pendingDeltas) {
        freshSinceSnapshotRequest.add(agentId);
        if (normalizedDelta.op === 'upsert') {
            pendingDeltas.upserts.add(normalizedDelta.component_id);
            pendingDeltas.removed.delete(normalizedDelta.component_id);
        } else if (normalizedDelta.op === 'remove') {
            pendingDeltas.removed.add(normalizedDelta.component_id);
            pendingDeltas.upserts.delete(normalizedDelta.component_id);
        } else if (normalizedDelta.op === 'reorder') {
            pendingDeltas.reorderSeen = true;
        }
    }

    muijMap.update((state) => {
        const nextState = applyDelta(state, agentId, normalizedDelta);
        // R279: Track agents only when they have renderable state or receive upserts.
        // Prevent remove/reorder-only unseen agents from lingering in trackedAgents.
        if (normalizedDelta.op === 'upsert' || nextState.has(agentId)) {
            trackedAgents.add(agentId);
        } else {
            trackedAgents.delete(agentId);
        }
        // R141: Cap trackedAgents to prevent unbounded growth
        if (trackedAgents.size > 1000) {
            const toKeep = new Set(nextState.keys());
            for (const id of trackedAgents) {
                if (!toKeep.has(id)) trackedAgents.delete(id);
            }
        }
        return nextState;
    });
}

/**
 * Handle a MUIJ snapshot response (direct server→client frame).
 * Uses merge strategy if deltas arrived since the snapshot was requested,
 * full replacement otherwise.
 */
export function handleMuijSnapshot(agentId: string, document: MuijDocument): void {
    if (!isValidSnapshotDocument(document)) {
        console.warn(`[muijStore] invalid snapshot document for agent ${agentId}`);
        handleMuijSnapshotError(agentId);
        return;
    }
    // R328: Cross-validate agent_id — reject misrouted/corrupt snapshots
    if (typeof document.agent_id === 'string' && document.agent_id !== agentId) {
        console.warn(`[muijStore] snapshot agent_id mismatch: expected ${agentId}, got ${document.agent_id}`);
        handleMuijSnapshotError(agentId);
        return;
    }
    // Reject late-arriving snapshots for agents whose cycle already completed (R60).
    if (recentlyCleared.has(agentId)) return;
    // R288: Reject stale snapshots from a prior cycle — a new cycle boundary
    // invalidated the pending request, so this snapshot is from the old cycle.
    if (snapshotInvalidatedByNewCycle.has(agentId)) {
        snapshotInvalidatedByNewCycle.delete(agentId);
        deltasSinceSnapshotRequest.delete(agentId);
        freshSinceSnapshotRequest.delete(agentId);
        return;
    }

    trackedAgents.add(agentId);
    const touchedSinceRequest = deltasSinceSnapshotRequest.get(agentId);
    deltasSinceSnapshotRequest.delete(agentId);

    if (freshSinceSnapshotRequest.has(agentId)) {
        // Deltas arrived after we requested this snapshot — they're fresher.
        // Merge: snapshot provides the component list, deltas preserve state.
        freshSinceSnapshotRequest.delete(agentId);
        muijMap.update((state) => mergeSnapshot(state, agentId, document, touchedSinceRequest));
    } else {
        // No deltas since request — snapshot is authoritative, full replace.
        muijMap.update((state) => applySnapshot(state, agentId, document));
    }
}

export function handleMuijSnapshotError(agentId: string): void {
    freshSinceSnapshotRequest.delete(agentId);
    deltasSinceSnapshotRequest.delete(agentId);
    // R316: Clear invalidation flag so future snapshots for this agent are accepted.
    snapshotInvalidatedByNewCycle.delete(agentId);
}

// R332: Throttle snapshot requests — minimum 2s between calls to prevent
// reconnect-loop storms from overwhelming the backend.
let lastSnapshotRequestTime = 0;
const SNAPSHOT_REQUEST_COOLDOWN_MS = 2000;

/**
 * Request MUIJ snapshots for all tracked agents on reconnect.
 * Called from `ws.onopen` in v2-websocket.ts.
 */
export function requestMuijSnapshots(ws: { send: (message: Record<string, unknown>) => boolean }): void {
    const now = Date.now();
    if (now - lastSnapshotRequestTime < SNAPSHOT_REQUEST_COOLDOWN_MS) {
        console.warn('[muijStore] snapshot request throttled — cooldown active');
        return;
    }
    lastSnapshotRequestTime = now;
    // R151: Cancel any stale timeout from a prior call (rapid reconnect safety)
    if (snapshotTimeoutId !== null) {
        clearTimeout(snapshotTimeoutId);
        snapshotTimeoutId = null;
    }
    freshSinceSnapshotRequest.clear();
    deltasSinceSnapshotRequest.clear();
    snapshotInvalidatedByNewCycle.clear(); // R288: New request cycle resets invalidation
    // R507/R141: Re-populate trackedAgents from muijMap keys — onDisconnect()
    // clears trackedAgents (R322) but muijMap retains rendered state. Without
    // this, reconnect would send zero snapshot requests.
    let currentAgentIds: string[] = [];
    muijMap.subscribe((state) => {
        currentAgentIds = [...state.keys()];
    })();
    for (const id of currentAgentIds) {
        trackedAgents.add(id);
    }
    const agentsToRequest = [...trackedAgents].slice(0, 200);
    // R646: Log when snapshot cap truncates agents
    if (trackedAgents.size > 200) {
        console.warn(`[muijStore] snapshot request truncated: ${trackedAgents.size} tracked agents, requesting first 200`);
    }
    for (const agentId of agentsToRequest) {
        deltasSinceSnapshotRequest.set(agentId, createSnapshotRequestDeltaTracker());
        // R212: Check send() return value and log on failure
        const sent = ws.send({
            type: 'agent.ui.snapshot_request',
            agent_id: agentId
        });
        if (!sent) {
            console.warn(`[muijStore] snapshot request send failed for agent ${agentId}`);
            // R238: Clean up tracking to allow future snapshot requests for this agent
            deltasSinceSnapshotRequest.delete(agentId);
        }
    }
    // R142/R455: Timeout stale snapshot requests after 30 seconds.
    // After timeout, mark all pending agents as invalidated so that very-late
    // snapshot responses are rejected (not applied as full-replacement which
    // would overwrite 30+ seconds of fresher delta-applied state).
    snapshotTimeoutId = setTimeout(() => {
        // R455: Invalidate rather than just clearing — prevents late snapshots
        // from overwriting fresher state via the full-replacement path.
        const pendingCount = deltasSinceSnapshotRequest.size;
        if (pendingCount > 0) {
            // R726: Log timeout so operators know snapshot round-trip failed
            console.warn(`[muijStore] snapshot request timed out — ${pendingCount} agents still pending, invalidating`);
        }
        for (const agentId of deltasSinceSnapshotRequest.keys()) {
            snapshotInvalidatedByNewCycle.add(agentId);
        }
        freshSinceSnapshotRequest.clear();
        deltasSinceSnapshotRequest.clear();
        snapshotTimeoutId = null;
    }, 30_000);
}

/**
 * Request a MUIJ snapshot for a single agent if it's not already tracked.
 *
 * Used by the ExecutionPanel to load persisted task plan MUIJ components
 * on page refresh — the agent isn't in `trackedAgents` yet because no
 * deltas have arrived, so `requestMuijSnapshots()` wouldn't request it.
 */
export function requestAgentSnapshotIfNeeded(
	agentId: string,
	ws: { send: (message: Record<string, unknown>) => boolean }
): void {
	if (trackedAgents.has(agentId)) return; // Already tracking — snapshot will come via reconnect
	let hasState = false;
	muijMap.subscribe((state) => { hasState = state.has(agentId); })();
	if (hasState) return; // Already have components in memory
	trackedAgents.add(agentId);
	deltasSinceSnapshotRequest.set(agentId, createSnapshotRequestDeltaTracker());
	ws.send({ type: 'agent.ui.snapshot_request', agent_id: agentId });
}

/**
 * Clear MUIJ state for an agent (called on AgentCycleCompleted).
 */
export function clearAgentMuij(agentId: string, cycleId?: string): void {
    // R641: Time-based dedup guard — suppress rapid double-completion events
    // (e.g., agent.cycle.completed + agent.cycle.paused for the same old cycle)
    // that arrive after a new cycle has already started and cleared recentlyCleared.
    const lastClear = lastClearAt.get(agentId);
    if (lastClear && Date.now() - lastClear < CLEAR_DEDUP_WINDOW_MS) {
        return;
    }
    const activeCycle = activeCycleByAgent.get(agentId);
    // R689: When cycleId is undefined but an active cycle is tracked, fall back
    // to the active cycle ID. The envelope extraction may not always provide
    // cycle_id (e.g., malformed payloads), but the completion event still
    // semantically terminates the active cycle.
    const effectiveCycleId = cycleId ?? activeCycle;
    if (effectiveCycleId && activeCycle && effectiveCycleId !== activeCycle) {
        console.debug(`[muijStore] clearAgentMuij(${agentId}) rejected: cycleId=${effectiveCycleId} !== activeCycle=${activeCycle} (R399)`);
        return;
    }
    // R122: Idempotency guard — skip if already cleared and no new state accumulated
    if (recentlyCleared.has(agentId)) return;
    trackedAgents.delete(agentId);
    freshSinceSnapshotRequest.delete(agentId);
    deltasSinceSnapshotRequest.delete(agentId);
    if (effectiveCycleId) activeCycleByAgent.delete(agentId);
    recentlyCleared.set(agentId, Date.now());
    lastClearAt.set(agentId, Date.now());
    // R103/R323: Evict stale entries older than 5 minutes on every clear call
    // to prevent unbounded growth even below the cap.
    const cutoff = Date.now() - 5 * 60 * 1000;
    for (const [id, ts] of recentlyCleared) {
        if (ts < cutoff) recentlyCleared.delete(id);
    }
    // R644: Hard cap to prevent unbounded growth from rapid cycle churn
    const MAX_RECENTLY_CLEARED = 500;
    if (recentlyCleared.size > MAX_RECENTLY_CLEARED) {
        // Evict oldest entries (Map preserves insertion order)
        const toEvict = recentlyCleared.size - MAX_RECENTLY_CLEARED;
        let evicted = 0;
        for (const id of recentlyCleared.keys()) {
            if (evicted >= toEvict) break;
            recentlyCleared.delete(id);
            evicted++;
        }
    }
    muijMap.update((state) => {
        // R326: Clone before mutating
        const next = new Map(state);
        next.delete(agentId);
        return next;
    });
}

/**
 * R322: Clear all tracking state on navigation disconnect.
 * Prevents stale `trackedAgents` entries from accumulating across page navigations.
 * Called from v2-websocket.ts `disconnect()`.
 */
export function onDisconnect(): void {
    trackedAgents.clear();
    // R362: Clear recentlyCleared on disconnect — without this, agents whose cycle
    // completed before navigation are permanently silenced after reconnect because
    // handleMuijEvent and handleMuijSnapshot both early-return on recentlyCleared.has().
    recentlyCleared.clear();
    lastClearAt.clear();
    freshSinceSnapshotRequest.clear();
    deltasSinceSnapshotRequest.clear();
    snapshotInvalidatedByNewCycle.clear();
    // R376: Clear activeCycleByAgent — stale cycle IDs from before disconnect would cause
    // clearAgentMuij to reject valid completions for re-registered agents after reconnect.
    activeCycleByAgent.clear();
    lastSnapshotRequestTime = 0; // R332: Reset cooldown on disconnect
    if (snapshotTimeoutId !== null) {
        clearTimeout(snapshotTimeoutId);
        snapshotTimeoutId = null;
    }
}

/**
 * Signal that a new agent cycle has started. Clears the recently-cleared
 * flag so deltas for the new cycle are accepted.
 * Called from v2-websocket.ts on AgentCycleStarted.
 */
export function onAgentCycleStarted(agentId: string, cycleId?: string): void {
    recentlyCleared.delete(agentId);
    // R641: Reset dedup guard so the new cycle's completion event is not suppressed
    lastClearAt.delete(agentId);
    freshSinceSnapshotRequest.delete(agentId);
    // R288: If a snapshot request is pending for this agent, mark it stale
    if (deltasSinceSnapshotRequest.has(agentId)) {
        snapshotInvalidatedByNewCycle.add(agentId);
        // R695: Cap snapshotInvalidatedByNewCycle to prevent unbounded growth
        if (snapshotInvalidatedByNewCycle.size > 1000) {
            snapshotInvalidatedByNewCycle.clear();
        }
    }
    deltasSinceSnapshotRequest.delete(agentId);
    if (cycleId) activeCycleByAgent.set(agentId, cycleId);
    else activeCycleByAgent.delete(agentId);
    // R293: Cap activeCycleByAgent to prevent unbounded growth
    if (activeCycleByAgent.size > 1000) {
        for (const [id] of activeCycleByAgent) {
            if (!trackedAgents.has(id) && id !== agentId) activeCycleByAgent.delete(id);
        }
    }
    // R629/R740: Keep agent in trackedAgents so reconnect can request a snapshot.
    // Previously, deleting here meant reconnect between cycle start and first delta
    // would skip this agent entirely (requestMuijSnapshots finds no tracked agent).
    trackedAgents.add(agentId);
    // Force a clean boundary between cycles so reconnect merge cannot create chimera state.
    // R327: Stale coalescer deltas from the prior cycle may arrive after this wipe.
    // Since onAgentCycleStarted deletes the agent's state, stale deltas re-create it
    // briefly until new-cycle deltas replace them. Accepted: brief visual flicker only.
    muijMap.update((state) => {
        // R326: Clone before mutating
        const next = new Map(state);
        next.delete(agentId);
        return next;
    });
}

// =============================================================================
// Derived Stores
// =============================================================================

/**
 * Get ordered MuijComponents for a specific agent.
 */
export function getComponentsByAgent(agentId: string) {
    return derived(muijMap, ($map) => {
        const agentState = $map.get(agentId);
        if (!agentState) return [];
        return agentState.order
            .map(id => agentState.components.get(id))
            .filter((c): c is MuijComponent => c !== undefined);
    });
}

/**
 * Set of agent IDs that currently have MUIJ state.
 */
export const activeAgentIds = derived(muijMap, ($map) =>
    Array.from($map.keys())
);

/**
 * Clear all MUIJ state and tracking metadata.
 * Intended for route lifecycle boundaries where stale state must not survive
 * (e.g., leaving the app layout scope).
 */
export function clearAllMuijState(): void {
    trackedAgents.clear();
    recentlyCleared.clear();
    lastClearAt.clear();
    freshSinceSnapshotRequest.clear();
    deltasSinceSnapshotRequest.clear();
    activeCycleByAgent.clear();
    snapshotInvalidatedByNewCycle.clear();
    lastSnapshotRequestTime = 0;
    if (snapshotTimeoutId !== null) {
        clearTimeout(snapshotTimeoutId);
        snapshotTimeoutId = null;
    }
    muijMap.set(new Map());
}

// =============================================================================
// Test Utilities
// =============================================================================

/** @internal Reset all module-level state. Only for use in test suites (R307). */
export function _resetForTesting(): void {
    trackedAgents.clear();
    recentlyCleared.clear();
    lastClearAt.clear();
    freshSinceSnapshotRequest.clear();
    deltasSinceSnapshotRequest.clear();
    activeCycleByAgent.clear();
    snapshotInvalidatedByNewCycle.clear();
    lastSnapshotRequestTime = 0; // R332: Reset cooldown for test isolation
    if (snapshotTimeoutId !== null) {
        clearTimeout(snapshotTimeoutId);
        snapshotTimeoutId = null;
    }
    muijMap.set(new Map());
}
