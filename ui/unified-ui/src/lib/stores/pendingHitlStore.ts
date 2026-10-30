/**
 * Pending HITL count — single source of truth for "how many human-in-the-
 * loop responses is the system waiting on right now".
 *
 * Live events come from the app's event hub (`$lib/realtime/eventHub`,
 * filter `categories: ['hitl'], userRelevant: true`); history is one
 * `/api/magician/v3/events?category=hitl&user_relevant=true&backfill_only=true`
 * request per scope, repeated when the hub resyncs. Counts events of `attention.raised` / `clarification.queued` /
 * `approval.requested` family minus their resolved counterparts.
 *
 * Drop the derived `pendingHitlCount` store into any badge surface —
 * TopBar badge/cascade, Internals drawer header, Attention center —
 * instead of letting each surface re-implement its own counting.
 *
 * See `docs/archive/plans/2026-05-10-execution-panel-canvas-redesign.md` —
 * Deferred / HITL standardization section.
 */
import { browser } from '$app/environment';
import { derived, writable, type Readable } from 'svelte/store';
import { scopeIdentityStore, getCurrentScopeCredentialRevision } from '$lib/stores/scopeIdentityStore';
import { timedFetch } from '$lib/shared/fetch';
import { parseNdjsonFrames, subscribeWithBackfill } from '$lib/realtime/eventHub';
import { attentionStore } from '$lib/stores/attentionStore';
import { hitlRequestFromCanonicalEvent } from '$lib/hitl/adapters';

export interface HitlPendingEntry {
	pause_state_id?: string;
	approval_id?: string;
	correlation_id: string;
	at: number;
	/**
	 * Chat turn this HITL belongs to, when the event carries one. Set
	 * by the typing-bubble surface in `chat/+page.svelte` to flip the
	 * "Working…" dots into a "Waiting on you" pill for the in-flight
	 * turn. Missing for non-chat-anchored HITLs (autonomous loops,
	 * delegate execution paused outside a chat turn, etc.).
	 */
	chat_turn_id?: string;
	/**
	 * The task this ask blocks, from the parsed request's scope (its task id,
	 * or a plan question's workflow id). Missing for an ask raised outside a
	 * task. Read by task cards that say "needs you" only when an ask exists.
	 */
	task_id?: string;
	/**
	 * Raw canonical event payload. Kept so click handlers (typing-bubble
	 * pill, future surfaces) can rebuild a full `HitlRequest` via
	 * `hitlRequestFromCanonicalEvent` and hand it to `respondToHitl`
	 * — same modal + same POST as the `/attention` path. Without this
	 * each surface would have to fetch the request shape separately.
	 */
	raw?: Record<string, unknown>;
}

const pending = writable<Map<string, HitlPendingEntry>>(new Map());

/** Number of HITL items currently awaiting a human response. */
export const pendingHitlCount: Readable<number> = derived(pending, ($p) => $p.size);

/** Snapshot of all currently-pending entries; rarely needed, but exposed for debug. */
export const pendingHitlEntries: Readable<HitlPendingEntry[]> = derived(pending, ($p) =>
	Array.from($p.values())
);

/**
 * Optimistically drop a pending entry by its correlation id (the map's key).
 * Called from a resolve surface (the /attention respond handler) the instant
 * the POST succeeds, so the row disappears immediately instead of lingering
 * until the SSE `HitlResolved` or the next poll catches up. Idempotent — a
 * no-op if the id isn't pending (e.g. the SSE event already removed it).
 */
export function dropPendingHitl(correlationId: string): void {
	if (!correlationId) return;
	pending.update((current) => {
		if (!current.has(correlationId)) return current;
		const next = new Map(current);
		next.delete(correlationId);
		return next;
	});
}

/**
 * Lookup table keyed by `chat_turn_id` so the chat typing-bubble can
 * ask "is there a HITL waiting for THIS turn?" in O(1). A single turn
 * with multiple parallel HITLs surfaces the most recent one (the
 * common case is 1:1 anyway — multi-question batches share a chain
 * id and surface as a single chained prompt in the modal).
 */
export const pendingHitlByChatTurnId: Readable<Map<string, HitlPendingEntry>> = derived(
	pending,
	($p) => {
		const out = new Map<string, HitlPendingEntry>();
		for (const entry of $p.values()) {
			if (entry.chat_turn_id) {
				const existing = out.get(entry.chat_turn_id);
				if (!existing || entry.at >= existing.at) {
					out.set(entry.chat_turn_id, entry);
				}
			}
		}
		return out;
	}
);

/** Ids of the tasks at least one pending ask blocks. */
export const pendingHitlTaskIds: Readable<Set<string>> = derived(pending, ($p) => {
	const out = new Set<string>();
	for (const entry of $p.values()) {
		if (entry.task_id) out.add(entry.task_id);
	}
	return out;
});

let lastScopeKey = '';
// Module-level handle to the scope-store subscription so repeated
// `ensurePendingHitlBridge()` calls don't stack independent
// subscribers. Without this, every TopBar remount (layout swap, hot-
// reload, theme switch) would add a fresh `scopeIdentityStore.subscribe`
// callback — each callback then races to disconnect/reconnect on the
// next scope change, multiplying hub subscriptions.
let scopeUnsub: (() => void) | null = null;

/** Read the canonical timestamp (ms epoch) from any event shape. Mirrors
 *  the layered extractor in `EventStreamCard.svelte` and the backend
 *  `extract_event_timestamp_ms`: tries top-level → `data.timestamp` →
 *  `payload.timestamp_ms`. Returns `null` when none of them resolve. */
function readEventTimestampMs(raw: Record<string, unknown>): number | null {
	const data = (raw as { data?: unknown }).data;
	const payload = (raw as { payload?: unknown }).payload;
	const dataObj =
		data !== null && typeof data === 'object'
			? (data as Record<string, unknown>)
			: undefined;
	const payloadObj =
		payload !== null && typeof payload === 'object'
			? (payload as Record<string, unknown>)
			: undefined;
	const candidates: unknown[] = [
		raw.timestamp_ms,
		raw.timestamp,
		dataObj?.timestamp,
		dataObj?.timestamp_ms,
		payloadObj?.timestamp_ms,
		payloadObj?.timestamp
	];
	for (const candidate of candidates) {
		// Zero-skip matches `transport_log::has_structural_timestamp`
		// and EventStreamCard's extractor: legacy variants with
		// `#[serde(default)] i64` serialize 0 when unset, and accepting
		// that would anchor at epoch zero.
		if (typeof candidate === 'number' && Number.isFinite(candidate) && candidate !== 0) {
			return candidate;
		}
		if (typeof candidate === 'string') {
			const parsed = Date.parse(candidate);
			if (Number.isFinite(parsed) && parsed !== 0) return parsed;
		}
	}
	return null;
}

function correlationKey(raw: Record<string, unknown>): string | null {
	// Phase H2 — the canonical `HitlRequested` event puts the canonical
	// id at `correlation_id`. Tried first so the dual-emitted legacy
	// event (which carries the same id at `pause_state_id`) deduplicates
	// correctly into one Map entry.
	//
	// Walk three nesting levels because the wire shape differs per path:
	//   1. Live-tail typed `RuntimeTransportEvent` (e.g. `HitlRequested`):
	//      `{event_type: "HitlRequested", data: {correlation_id, ...}}`.
	//   2. Live-tail envelope (`AgentEvent`):
	//      `{event_type: "AgentEvent", data: {event: {payload: {correlation_id}}}}`.
	//   3. Cross-scope backfill (canonical event from events.jsonl):
	//      `{event_id, event_type, payload: {correlation_id, ...}}`.
	//
	// `event_id` is intentionally excluded — it's a per-emission uuid
	// (`evt_<random>`), so falling back to it makes every event its own
	// pending entry and resolutions never match a prior key, producing a
	// monotonically-climbing badge count (the bug that produced "393").
	const data = (raw as { data?: unknown }).data;
	const dataObj =
		data !== null && typeof data === 'object'
			? (data as Record<string, unknown>)
			: undefined;
	const dataEvent =
		dataObj !== undefined ? (dataObj as { event?: unknown }).event : undefined;
	const dataEventObj =
		dataEvent !== null && typeof dataEvent === 'object'
			? (dataEvent as Record<string, unknown>)
			: undefined;
	const dataEventPayload =
		dataEventObj !== undefined
			? (dataEventObj as { payload?: unknown }).payload
			: undefined;
	const dataEventPayloadObj =
		dataEventPayload !== null && typeof dataEventPayload === 'object'
			? (dataEventPayload as Record<string, unknown>)
			: undefined;
	const payload = (raw as { payload?: unknown }).payload;
	const payloadObj =
		payload !== null && typeof payload === 'object'
			? (payload as Record<string, unknown>)
			: undefined;
	const layers: Array<Record<string, unknown> | undefined> = [
		raw,
		dataObj,
		dataEventObj,
		dataEventPayloadObj,
		payloadObj
	];
	const keys = [
		'correlation_id',
		'pause_state_id',
		'approval_id',
		'clarification_id',
		'request_id'
	];
	// Key-first / layer-second. `correlation_id` is the dedup contract
	// — try it across every nesting layer before falling back to
	// `pause_state_id` (and so on). The previous layer-first order
	// meant a request emitting `correlation_id` at `payload.*` and its
	// resolution emitting `pause_state_id` at top-level would mint
	// different keys, leaving the pending entry unresolved and the
	// badge climbing monotonically — the original bug class this
	// helper was added to prevent.
	for (const key of keys) {
		for (const layer of layers) {
			if (!layer) continue;
			const value = layer[key];
			if (typeof value === 'string' && value.length > 0) return value;
		}
	}
	return null;
}

function isResolutionEvent(eventType: string): boolean {
	// Phase H2 — `HitlResolved` is the canonical resolution event (the
	// per-source `UserRequestResolved` was retired backend-side in H7.1).
	// The legacy `.resolved` / `.responded` / `.expired` / `.cancelled` /
	// `.dismissed` suffixes still apply.
	if (eventType === 'HitlResolved') return true;
	return (
		eventType.endsWith('.resolved') ||
		eventType.endsWith('.responded') ||
		eventType.endsWith('.expired') ||
		eventType.endsWith('.cancelled') ||
		eventType.endsWith('.dismissed')
	);
}

/** Walk the same nesting layers `correlationKey` does to pull a named
 *  string field out of a possibly-nested event payload. Returns
 *  `undefined` when the key is absent at every layer. */
function pickStringAcrossLayers(
	raw: Record<string, unknown>,
	key: string
): string | undefined {
	const data = (raw as { data?: unknown }).data;
	const dataObj =
		data !== null && typeof data === 'object'
			? (data as Record<string, unknown>)
			: undefined;
	const dataEvent =
		dataObj !== undefined ? (dataObj as { event?: unknown }).event : undefined;
	const dataEventObj =
		dataEvent !== null && typeof dataEvent === 'object'
			? (dataEvent as Record<string, unknown>)
			: undefined;
	const dataEventPayload =
		dataEventObj !== undefined
			? (dataEventObj as { payload?: unknown }).payload
			: undefined;
	const dataEventPayloadObj =
		dataEventPayload !== null && typeof dataEventPayload === 'object'
			? (dataEventPayload as Record<string, unknown>)
			: undefined;
	const payload = (raw as { payload?: unknown }).payload;
	const payloadObj =
		payload !== null && typeof payload === 'object'
			? (payload as Record<string, unknown>)
			: undefined;
	for (const layer of [raw, dataObj, dataEventObj, dataEventPayloadObj, payloadObj]) {
		if (!layer) continue;
		const value = layer[key];
		if (typeof value === 'string' && value.length > 0) return value;
	}
	return undefined;
}

/**
 * The form of a `HitlRequested` line that `hitlRequestFromCanonicalEvent`
 * accepts, or `null` when no surface could render it. The live tail carries
 * the request under `data`; the events backfill carries it under `payload`,
 * which is re-wrapped under `data` so both parse through the one adapter.
 */
function renderableCanonicalEvent(
	parsed: Record<string, unknown>
): Record<string, unknown> | null {
	if (hitlRequestFromCanonicalEvent(parsed)) return parsed;
	const payload = (parsed as { payload?: unknown }).payload;
	if (payload === null || typeof payload !== 'object' || Array.isArray(payload)) return null;
	const wrapped: Record<string, unknown> = {
		...parsed,
		event_type: 'HitlRequested',
		data: payload as Record<string, unknown>
	};
	return hitlRequestFromCanonicalEvent(wrapped) ? wrapped : null;
}

function ingestLine(line: string): void {
	let parsed: Record<string, unknown>;
	try {
		parsed = JSON.parse(line);
	} catch {
		return;
	}
	ingestFrame(parsed);
}

function ingestFrame(parsed: Record<string, unknown>): void {
	const eventType = String(parsed.event_type ?? '');
	if (!eventType || eventType.startsWith('__events_')) return;
	const key = correlationKey(parsed);
	if (!key) return;
	if (isResolutionEvent(eventType)) {
		// Mirror the resolution onto the attention feed store so a feed-sourced
		// (or cross-surface, e.g. resolved on desktop) row drops on this same SSE
		// event rather than waiting on the v2 WS relay or the 15s
		// /feed/attention poll. Unconditional (not gated on the bus holding
		// `key`) so a feed-only pause still clears; idempotent if it didn't.
		attentionStore.dropResolved(key);
		pending.update((current) => {
			if (!current.has(key)) return current;
			const next = new Map(current);
			next.delete(key);
			return next;
		});
		return;
	}
	// Count ONLY the canonical top-level `HitlRequested` — the same predicate the
	// attention page's `hitlRequestFromCanonicalEvent` renders. Legacy dual-emit
	// twins (`AgenticWaitingForUser` / `AgenticWaitingForConfirmation`) are
	// co-emitted for the SAME pause but keyed on
	// `pause_state_id` (vs the canonical `correlation_id`), so they double-counted
	// the badge AND left an orphan no `HitlResolved` ever clears + the page can't
	// render. Skipping non-canonical events keeps the badge's key set identical to
	// the page's renderable set, so the badge can never exceed the pending list.
	if (eventType !== 'HitlRequested') return;
	// Count only what a surface can open. A request the canonical parser rejects
	// (unknown source, a choice with no options, missing scope) would raise the
	// badge and offer a pill whose click can only fail, so it never enters the
	// map. The stored `raw` is the shape that parsed, so every click surface
	// rebuilds the same request this check accepted.
	const renderable = renderableCanonicalEvent(parsed);
	if (!renderable) return;
	const request = hitlRequestFromCanonicalEvent(renderable);
	const taskId = request?.scope.task_id ?? request?.scope.workflow_id;
	const ts = readEventTimestampMs(parsed) ?? Date.now();
	pending.update((current) => {
		const next = new Map(current);
		next.set(key, {
			pause_state_id: pickStringAcrossLayers(parsed, 'pause_state_id'),
			approval_id: pickStringAcrossLayers(parsed, 'approval_id'),
			correlation_id: key,
			at: ts,
			chat_turn_id: pickStringAcrossLayers(parsed, 'chat_turn_id'),
			task_id: taskId,
			// Stash the raw event so click surfaces can rebuild a full
			// `HitlRequest` without re-fetching. JSON-serializable, so the
			// Map stays cheap to clone on update.
			raw: renderable
		});
		return next;
	});
}

let liveUnsub: (() => void) | null = null;

function disconnect(): void {
	if (liveUnsub) {
		liveUnsub();
		liveUnsub = null;
	}
}

/**
 * History for the pending map: one `backfill_only` request. Filter to the
 * HITL category but DO NOT pin severity — requests ride at `attention`,
 * their `*Resolved` twins at `info`, and pinning would drop every resolution.
 * The map is reset first so an entry whose resolution was missed while the
 * hub was disconnected cannot linger; the backfill carries the resolution in
 * order, so a fresh replay converges.
 */
async function backfill(signal: AbortSignal): Promise<void> {
	const params = new URLSearchParams();
	params.set('category', 'hitl');
	params.set('user_relevant', 'true');
	params.set('backfill_only', 'true');
	const response = await timedFetch(`/api/magician/v3/events?${params.toString()}`, { signal });
	if (signal.aborted || !response.ok) return;
	const body = await response.text();
	if (signal.aborted) return;
	pending.set(new Map());
	for (const frame of parseNdjsonFrames(body)) ingestFrame(frame);
}

function connect(): void {
	disconnect();
	pending.set(new Map());
	liveUnsub = subscribeWithBackfill({
		filter: { categories: ['hitl'], userRelevant: true },
		backfill: ({ signal }) => backfill(signal),
		onEvent: (event) => ingestFrame(event.raw)
	});
}

/** Mounts the subscription and re-binds when scope changes. Idempotent —
 *  subsequent calls reuse the single module-level subscriber, so multiple
 *  TopBar mounts (layout swaps, hot-reloads, theme cycles) don't stack
 *  duplicate scope listeners. */
export function ensurePendingHitlBridge(): void {
	if (!browser) return;
	if (scopeUnsub) return;
	scopeUnsub = scopeIdentityStore.subscribe((scope) => {
		const nextKey = `${scope.principal}::${scope.workspace}::${getCurrentScopeCredentialRevision()}`;
		if (nextKey === lastScopeKey) return;
		lastScopeKey = nextKey;
		connect();
	});
}

/** Feed one NDJSON line through the same ingest the live tail uses. Exposed
 *  for unit tests, which cannot open the events stream. */
export function ingestPendingHitlLineForTest(line: string): void {
	ingestLine(line);
}

export function teardownPendingHitlBridge(): void {
	disconnect();
	pending.set(new Map());
	lastScopeKey = '';
	if (scopeUnsub) {
		scopeUnsub();
		scopeUnsub = null;
	}
}
