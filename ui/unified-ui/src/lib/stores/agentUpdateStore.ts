// Agent update store — accumulates AgentUpdate events per (principal, workspace) scope.
//
// Fed by two paths:
//  1. Initial REST load via `GET /api/magician/v2/updates` at page mount.
//  2. Live WebSocket deltas via `handleAgentUpdateEnvelopeEvent`, called
//     from v2-websocket.ts whenever an AgentEventEnvelope with
//     `event_type === 'agent.update'` arrives.
//
// Both paths deduplicate by event `id`. Events stored newest-first.
// Memory cap per scope prevents unbounded growth during long-running feeds.

import { derived, writable, type Readable } from 'svelte/store';
import type { AgentEventEnvelope } from '$lib/realtime/v2-websocket';
import type { AgentUpdate } from '$lib/types/agentUpdate';

export const AGENT_UPDATE_EVENT_TYPE = 'agent.update';
const MAX_PER_SCOPE = 500;

type ScopeKey = string;

function scopeKey(principal: string, workspace: string): ScopeKey {
	return `${principal}/${workspace}`;
}

interface ScopeState {
	events: AgentUpdate[];
	seenIds: Set<string>;
}

const stateMap = writable<Map<ScopeKey, ScopeState>>(new Map());

// --------------------------------------------------------------------------
// Ingest paths
// --------------------------------------------------------------------------

/**
 * Handle a WebSocket envelope with `event_type === 'agent.update'`. Invoked
 * from v2-websocket.ts for every inbound AgentEventEnvelope; it short-circuits
 * on non-update event types, so callers can fire it unconditionally.
 */
export function handleAgentUpdateEnvelopeEvent(envelope: AgentEventEnvelope): void {
	if (envelope.event_type !== AGENT_UPDATE_EVENT_TYPE) return;
	const principal = envelope.principal ?? '';
	const workspace = envelope.workspace ?? '';
	if (!principal || !workspace) {
		// Unscoped updates cannot be routed to any feed view — drop.
		return;
	}
	const update = envelope.payload as AgentUpdate | null;
	if (!update || typeof update !== 'object' || typeof update.id !== 'string') {
		return;
	}
	ingest(principal, workspace, [update]);
}

/** Seed the store from an initial REST response. Idempotent on replay. */
export function seedScope(
	principal: string,
	workspace: string,
	events: AgentUpdate[]
): void {
	if (!principal || !workspace) return;
	ingest(principal, workspace, events);
}

/** Remove all events for a scope. Use on scope switch or explicit reset. */
export function clearScope(principal: string, workspace: string): void {
	if (!principal || !workspace) return;
	const key = scopeKey(principal, workspace);
	stateMap.update((map) => {
		if (!map.has(key)) return map;
		const next = new Map(map);
		next.delete(key);
		return next;
	});
}

function ingest(principal: string, workspace: string, newEvents: AgentUpdate[]): void {
	if (newEvents.length === 0) return;
	const key = scopeKey(principal, workspace);
	stateMap.update((map) => {
		const next = new Map(map);
		const current = next.get(key) ?? { events: [], seenIds: new Set<string>() };
		const seenIds = new Set(current.seenIds);
		const acceptedFresh: AgentUpdate[] = [];
		for (const event of newEvents) {
			if (!event || typeof event.id !== 'string') continue;
			if (seenIds.has(event.id)) continue;
			seenIds.add(event.id);
			acceptedFresh.push(event);
		}
		if (acceptedFresh.length === 0) return map;
		// Merge and sort newest first. The fresh set plus the existing list;
		// both are already roughly time-ordered but we resort to be safe.
		const merged = [...acceptedFresh, ...current.events];
		merged.sort((a, b) => b.ts - a.ts);
		const capped = merged.slice(0, MAX_PER_SCOPE);
		// Re-derive seenIds to match the capped window so evicted ids can
		// re-enter via a later WS replay.
		const cappedIds = new Set(capped.map((e) => e.id));
		next.set(key, { events: capped, seenIds: cappedIds });
		return next;
	});
}

// --------------------------------------------------------------------------
// Read paths
// --------------------------------------------------------------------------

/** Derived store of events for the given scope; empty array if unseeded. */
export function updatesForScope(
	principal: string,
	workspace: string
): Readable<AgentUpdate[]> {
	const key = scopeKey(principal, workspace);
	return derived(stateMap, (map) => map.get(key)?.events ?? []);
}

/**
 * Derived store of events for the given (principal, workspace) narrowed to a
 * specific `thread_id`. Returns events whose envelope `thread_id` matches
 * exactly; events without a thread_id are excluded.
 *
 * Storage is still keyed per-workspace; this is a read-side filter that
 * stays reactive when new thread-scoped events arrive.
 */
export function updatesForThread(
	principal: string,
	workspace: string,
	threadId: string
): Readable<AgentUpdate[]> {
	const key = scopeKey(principal, workspace);
	return derived(stateMap, (map) => {
		const all = map.get(key)?.events ?? [];
		if (!threadId) return all;
		return all.filter((event) => event.thread_id === threadId);
	});
}

/** Non-reactive read of current events for a scope. */
export function currentUpdatesForScope(
	principal: string,
	workspace: string,
	get: <T>(store: Readable<T>) => T
): AgentUpdate[] {
	return get(updatesForScope(principal, workspace));
}

// --------------------------------------------------------------------------
// Test helpers — not exported at package level.
// --------------------------------------------------------------------------

/** Test-only: clear all scopes. */
export function __resetForTests(): void {
	stateMap.set(new Map());
}
