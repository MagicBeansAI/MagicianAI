/**
 * Pending HITL count — single source of truth for "how many human-in-the-
 * loop responses is the system waiting on right now".
 *
 * Subscribes to a dedicated NDJSON tail of `/api/magician/v3/events?
 * category=hitl&severity=attention&user_relevant=true` for the active
 * scope. Counts events of `attention.raised` / `clarification.queued` /
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
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { timedFetch, LONG_FETCH_TIMEOUT_MS } from '$lib/shared/fetch';
import { createBackoff } from '$lib/realtime/backoff';
import { attentionStore } from '$lib/stores/attentionStore';

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

let connection: AbortController | null = null;
let lastScopeKey = '';
// Module-level handle to the scope-store subscription so repeated
// `ensurePendingHitlBridge()` calls don't stack independent
// subscribers. Without this, every TopBar remount (layout swap, hot-
// reload, theme switch) would add a fresh `scopeIdentityStore.subscribe`
// callback — each callback then races to disconnect/reconnect on the
// next scope change, multiplying open SSE-style fetch streams.
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
	// Legacy `UserRequestPending` shape carries the request id nested at
	// `data.request.id` (the inner UserRequest serialized as JSON). We
	// surface it as a synthetic layer here so the same `correlationKey`
	// helper resolves both legacy and canonical shapes consistently.
	const dataRequest =
		dataObj !== undefined ? (dataObj as { request?: unknown }).request : undefined;
	const dataRequestObj =
		dataRequest !== null && typeof dataRequest === 'object'
			? (dataRequest as Record<string, unknown>)
			: undefined;
	const layers: Array<Record<string, unknown> | undefined> = [
		raw,
		dataObj,
		dataEventObj,
		dataEventPayloadObj,
		payloadObj,
		dataRequestObj
	];
	const keys = [
		'correlation_id',
		'pause_state_id',
		'approval_id',
		'clarification_id',
		'request_id',
		'id'
	];
	// Key-first / layer-second. `correlation_id` is the dedup contract
	// — try it across every nesting layer before falling back to
	// `pause_state_id` (and so on). The previous layer-first order
	// meant a request emitting `correlation_id` at `payload.*` and its
	// resolution emitting `pause_state_id` at top-level would mint
	// different keys, leaving the pending entry unresolved and the
	// badge climbing monotonically — the original bug class this
	// helper was added to prevent. `id` is last and only reached via
	// `data.request` (the legacy `UserRequest` payload) so it doesn't
	// accidentally match unrelated top-level `id` fields.
	for (const key of keys) {
		for (const layer of layers) {
			if (!layer) continue;
			if (key === 'id' && layer !== dataRequestObj) continue;
			const value = layer[key];
			if (typeof value === 'string' && value.length > 0) return value;
		}
	}
	return null;
}

function isResolutionEvent(eventType: string): boolean {
	// Phase H2 — `HitlResolved` is the canonical resolution event.
	// Existing per-source resolutions (UserRequestResolved, etc.) and
	// the legacy `.resolved` / `.responded` / `.expired` / `.cancelled` /
	// `.dismissed` suffixes still apply.
	if (eventType === 'HitlResolved' || eventType === 'UserRequestResolved') return true;
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

function ingestLine(line: string): void {
	let parsed: Record<string, unknown>;
	try {
		parsed = JSON.parse(line);
	} catch {
		return;
	}
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
	// twins (`AgenticWaitingForUser` / `AgenticWaitingForConfirmation` /
	// `UserRequestPending`) are co-emitted for the SAME pause but keyed on
	// `pause_state_id` (vs the canonical `correlation_id`), so they double-counted
	// the badge AND left an orphan no `HitlResolved` ever clears + the page can't
	// render. Skipping non-canonical events keeps the badge's key set identical to
	// the page's renderable set, so the badge can never exceed the pending list.
	if (eventType !== 'HitlRequested') return;
	const ts = readEventTimestampMs(parsed) ?? Date.now();
	pending.update((current) => {
		const next = new Map(current);
		next.set(key, {
			pause_state_id: pickStringAcrossLayers(parsed, 'pause_state_id'),
			approval_id: pickStringAcrossLayers(parsed, 'approval_id'),
			correlation_id: key,
			at: ts,
			chat_turn_id: pickStringAcrossLayers(parsed, 'chat_turn_id'),
			// Stash the raw event so click surfaces can rebuild a full
			// `HitlRequest` without re-fetching. JSON-serializable, so the
			// Map stays cheap to clone on update.
			raw: parsed
		});
		return next;
	});
}

async function disconnect(): Promise<void> {
	if (reconnectTimer) {
		clearTimeout(reconnectTimer);
		reconnectTimer = null;
	}
	if (connection) {
		connection.abort();
		connection = null;
	}
}

/** Reconnect backoff after the SSE ends (cleanly OR via timeout).
 *  Exponential with jitter: a healthy stream that hits its 10-min
 *  long-fetch timeout reconnects in ~1–2s, but a DOWN backend backs
 *  off toward the 60s cap instead of retrying every 2s flat — a dead
 *  backend used to cost ~19k reconnects in 90 minutes across the
 *  event-stream clients. Reset when a stream actually delivers data. */
const reconnectBackoff = createBackoff({ initialMs: 2_000, maxMs: 60_000 });
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

function scheduleReconnect(principal: string, workspace: string): void {
	if (reconnectTimer) return; // already pending
	reconnectTimer = setTimeout(() => {
		reconnectTimer = null;
		// Scope may have changed while we were waiting — bail if the
		// active scope no longer matches the one this reconnect was
		// queued for. The scope-store subscription will already have
		// kicked off a fresh `connect()` for the new scope.
		const expectedKey = `${principal}::${workspace}`;
		if (lastScopeKey !== expectedKey) return;
		void connect(principal, workspace);
	}, reconnectBackoff.nextMs());
}

async function connect(principal: string, workspace: string): Promise<void> {
	await disconnect();
	// Reset the pending Map before replaying backfill so old entries
	// (e.g. a HitlRequested whose matching HitlResolved fired while the
	// previous SSE was timed-out / disconnected) don't linger. The
	// /events backfill includes the resolution in chronological order,
	// so a fresh replay always converges on the correct count even if
	// the previous live stream missed events.
	pending.set(new Map());
	connection = new AbortController();
	const params = new URLSearchParams();
	// Filter to the HITL category only, but DO NOT pin severity. Pending
	// requests are emitted at `attention` severity, but their matching
	// `*Resolved` / `*Responded` events ride at `info` — pinning the
	// stream to `attention` would silently drop every resolution and the
	// pending count would only ever grow. The store does its own
	// resolution-suffix matching in `ingestLine`.
	params.set('category', 'hitl');
	params.set('user_relevant', 'true');
	let aborted = false;
	try {
		const response = await timedFetch(`/api/magician/v3/events?${params.toString()}`, {
			signal: connection.signal,
			// SSE stream — must stay open for the full session. Default 30s
			// `timedFetch` timeout aborted the subscription after the first
			// half-minute, so the HITL badge silently froze and the
			// attention surfaces went blind to new requests. Use the long-fetch
			// timeout (same fix as `ChatTurnProgress` for `/events`).
			timeoutMs: LONG_FETCH_TIMEOUT_MS
		});
		if (!response.ok || !response.body) return;
		const reader = response.body.getReader();
		const decoder = new TextDecoder('utf-8');
		let buffer = '';
		while (true) {
			const { done, value } = await reader.read();
			if (done) return;
			// Data flowing again — future reconnects start from the fast
			// initial delay, not wherever the outage drove the backoff.
			reconnectBackoff.reset();
			buffer += decoder.decode(value, { stream: true });
			let newlineIndex: number;
			while ((newlineIndex = buffer.indexOf('\n')) !== -1) {
				const line = buffer.slice(0, newlineIndex);
				buffer = buffer.slice(newlineIndex + 1);
				if (line.trim()) ingestLine(line);
			}
		}
	} catch {
		// fetch aborted on scope change, unmount, or LONG_FETCH timeout.
		// In the timeout case (10 min) we want to reconnect so the badge
		// keeps tracking. In the unmount/scope-change case the abort
		// controller is the trigger; we detect that via the connection
		// being already nulled by `disconnect()`.
		aborted = connection === null;
	} finally {
		// If we exited the read loop NOT because the operator left
		// (scope change → disconnect() nulls `connection`), reconnect.
		// Backoff prevents a tight loop if the endpoint keeps 502-ing.
		if (!aborted && connection !== null) {
			scheduleReconnect(principal, workspace);
		}
	}
}

/** Mounts the subscription and re-binds when scope changes. Idempotent —
 *  subsequent calls reuse the single module-level subscriber, so multiple
 *  TopBar mounts (layout swaps, hot-reloads, theme cycles) don't stack
 *  duplicate scope listeners. */
export function ensurePendingHitlBridge(): void {
	if (!browser) return;
	if (scopeUnsub) return;
	scopeUnsub = scopeIdentityStore.subscribe((scope) => {
		const nextKey = `${scope.principal}::${scope.workspace}`;
		if (nextKey === lastScopeKey) return;
		lastScopeKey = nextKey;
		pending.set(new Map());
		void connect(scope.principal, scope.workspace);
	});
}

export function teardownPendingHitlBridge(): void {
	void disconnect();
	pending.set(new Map());
	lastScopeKey = '';
	if (scopeUnsub) {
		scopeUnsub();
		scopeUnsub = null;
	}
}
