/**
 * Notify-overlay data layer — V3 event stream → card list.
 *
 * Subscribes to a widened NDJSON tail of `/api/magician/v3/events?
 * category=hitl,pipeline,execution,agentic&user_relevant=true`
 * — a superset of the `category=hitl` tail `lib/stores/pendingHitlStore.ts`
 * uses. Instead of counting it folds each line into a `NotifyCard`:
 *   - `HitlRequested` → an `actionable` card (via `hitlEventToCard`), dropped
 *     on the matching `HitlResolved` (any outcome);
 *   - lifecycle events (errors, completions) → `success`/`error` cards
 *     (via `infoEventToCard`), coalesced so a re-emitting source shows one
 *     updating card.
 *
 * What's copied from `pendingHitlStore.ts` vs what's deliberately NOT:
 *   - COPIED verbatim — the *transport*: reader loop, backoff, scope
 *     resolution, and abort lifecycle (≈ lines 318-444 there), so the two
 *     surfaces connect / reconnect / re-scope identically. See the comments
 *     below pointing at the lines each block mirrors.
 *   - NOT copied — `pendingHitlStore`'s `correlationKey` /
 *     `pickStringAcrossLayers` layer-walk (`data.event.payload.*`,
 *     top-level `payload.*`, `data.request.*`). Our field extraction
 *     (`hitlEventToCard`, and the `HitlResolved` `correlation_id` read in
 *     `applyEvent`) reads ONLY the canonical FLAT `data.*` layer. This is
 *     intentional, not an oversight: on the V3 `/events` tail a
 *     `HitlRequested` / `HitlResolved` is always the serialized
 *     `RuntimeTransportEvent` enum variant, and that enum carries
 *     `#[serde(tag = "event_type", content = "data")]`
 *     (magician/src/magician_v2/realtime_events.rs:59-60), so its fields are
 *     structurally guaranteed to land flat under `data.*` —
 *     `{event_type:"HitlRequested", data:{correlation_id, source, input_type,
 *     prompt, hint, ...}}` (variant def: realtime_events.rs:1819-1856; every
 *     emit site constructs the top-level variant directly, e.g.
 *     execution/agentic/executor.rs:6601, so it is never wrapped in an
 *     `AgentEvent` envelope). `pendingHitlStore` walks the deeper layers to
 *     guard against OTHER shapes/types it deduplicates against — the
 *     `AgentEvent` envelope (`event_type:"AgentEvent"`), the dot-string
 *     canonical-sink backfill (`event_type:"hitl.requested"`,
 *     fields at `payload.*`; artifact_v2/events.rs:1697-1727), and the legacy
 *     `data.request.*` UserRequest — but it ALSO gates on
 *     `eventType === 'HitlRequested'` (pendingHitlStore.ts:299) BEFORE reading
 *     those fields, so for the requested-card path the two surfaces ingest the
 *     exact same flat-`data` PascalCase events. The dot-string `hitl.requested`
 *     / `hitl.resolved` backfill rows (per-execution events.jsonl) are simply
 *     invisible to BOTH surfaces (event_type mismatch) — they never add a card
 *     the overlay then fails to dismiss, so the "never miss a pending approval"
 *     guarantee holds without the layer-walk.
 *
 * The line→state transition is factored into the pure `applyEvent` so the
 * stream loop and the unit tests exercise exactly one ingest path.
 */
import { browser } from '$app/environment';
import { writable } from 'svelte/store';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { timedFetch, LONG_FETCH_TIMEOUT_MS } from '$lib/shared/fetch';
import { createBackoff } from '$lib/realtime/backoff';
import { hitlEventToCard, type NotifyCard } from './cardModel';
import { infoEventToCard } from './infoModel';
import { coalesceCard } from './coalesce';
import { isActionableNotificationSuppressed } from './suppression';

/** The overlay's card stack. Newest cards appended; deduped by correlationId.
 *
 * There is intentionally NO hard cap on this list. A cap would risk dropping a
 * still-pending approval — the one thing the overlay exists to never let go
 * unseen. Growth is instead bounded by the two safe mechanisms: every
 * actionable card is removed by its matching `HitlResolved`
 * (responded / expired / cancelled / dismissed) in `applyEvent`, and the list
 * is reset wholesale on (re)connect and scope change (`connect` /
 * `startNotifyStream` / `stopNotifyStream` call `cards.set([])`). Actionable
 * approval cards deliberately persist until they are resolved or expire, so
 * the stack only holds genuinely-open requests for the active scope — never
 * an unbounded backlog a cap would need to trim. */
export const cards = writable<NotifyCard[]>([]);

// Newest event timestamp the LIVE stream has delivered (set in `ingestLine`). The
// reconcile compares this against a fresh backfill to detect a half-open / stale
// live stream (e.g. after a backend restart) and force a reconnect — see
// `reconcileStaleCards`. Server-emitted `data.timestamp`, so comparable to the
// backfill's timestamps.
let lastSeenEventTs = 0;

/**
 * Informational cards (errors, completions) are LIVE-ONLY. On every (re)connect
 * the server replays its whole backfill window with no backfill→live boundary
 * marker on the wire, so without a guard a long-resolved — or already-dismissed
 * — error/completion toast would resurrect on each restart. We can't tell
 * backfill from live structurally, so we use the event's own emit timestamp
 * (`data.timestamp`, epoch ms): an informational event older than this window is
 * treated as stale and skipped. Far larger than real live latency (sub-second on
 * localhost) and than the cards' own 5s/15s dismiss windows, so a genuinely-live
 * toast is never dropped. Actionable HITL approvals are exempt — they MUST
 * backfill (never miss a pending approval) and are removed by `HitlResolved`.
 */
const INFO_MAX_AGE_MS = 30_000;

/**
 * Pure reducer: fold one canonical V3 event into the card list.
 *
 * Shared by the live stream loop and the tests so dedup/dismiss/coalesce logic
 * is verified once. Each event is tried against BOTH mappers:
 *   - `HitlRequested` → `hitlEventToCard` → an `actionable` card.
 *   - lifecycle (ExecutionFailed, ProcessingError, AgenticStepFailed,
 *     AgenticMaxIterationsReached, ExecutionCompleted) → `infoEventToCard` →
 *     a `success`/`error` card.
 *   - `HitlResolved` (any `outcome` — responded / expired / cancelled /
 *     dismissed) → remove the actionable card whose `correlationId` matches
 *     `data.correlation_id`.
 *   - anything else (or a malformed / null-mapped event) → list unchanged.
 *
 * Add-or-replace goes through `coalesceCard` (keyed on `coalesceKey`), so a
 * re-emitted/backfilled event collapses onto the existing card in place — the
 * actionable dedup-by-correlationId is the same code path as info/success/error
 * dedup, just with a different key. The reducer stays PURE: it only stamps
 * `dismissAfterMs` onto info/success cards; the VIEW owns the dismiss timers.
 *
 * Returns the same array reference when nothing changed, so callers can skip
 * a store update; order-independent and reentrant — replaying the same backlog
 * twice converges on the same set.
 */
export function applyEvent(
	current: NotifyCard[],
	event: { event_type: string; data: Record<string, unknown> },
	opts?: { nowMs?: number }
): NotifyCard[] {
	if (event.event_type === 'HitlResolved') {
		const correlationId = event.data?.correlation_id;
		if (typeof correlationId !== 'string' || correlationId.length === 0) {
			return current;
		}
		// Only actionable cards carry a correlationId; narrow before reading it
		// so an info card can never be matched/removed by a HitlResolved.
		if (
			!current.some((card) => card.kind === 'actionable' && card.correlationId === correlationId)
		) {
			return current;
		}
		return current.filter(
			(card) => !(card.kind === 'actionable' && card.correlationId === correlationId)
		);
	}

	// Try the actionable mapper first, then the informational mapper. A given
	// event_type matches at most one (HitlRequested → actionable; the lifecycle
	// / messaging types → info/success/error), so the order is immaterial.
	const card = hitlEventToCard(event) ?? infoEventToCard(event);
	if (!card) return current;

	// Backfill replays the whole window; drop a stale informational card so a
	// resolved/dismissed toast doesn't resurrect on (re)connect. `nowMs` is
	// injected by the caller (the live `ingestLine`) rather than read here, so
	// this reducer stays pure + deterministic for the replay tests, which call
	// `applyEvent` without `opts` and therefore never age-gate. Gated on card
	// kind (not event_type) so it tracks `infoModel` automatically.
	if (card.kind !== 'actionable' && typeof opts?.nowMs === 'number') {
		const ts = typeof event.data?.timestamp === 'number' ? event.data.timestamp : 0;
		if (ts > 0 && opts.nowMs - ts > INFO_MAX_AGE_MS) return current;
	}

	return coalesceCard(current, card);
}

/** Parse one NDJSON line into `{ event_type, data }` and fold it in.
 *  Mirrors `pendingHitlStore.ingestLine` (its lines ~271-316): swallow
 *  parse failures and skip the `__events_*` control frames, then route the
 *  canonical event through the shared reducer. */
function ingestLine(line: string): void {
	let parsed: Record<string, unknown>;
	try {
		parsed = JSON.parse(line);
	} catch {
		return;
	}
	const eventType = String(parsed.event_type ?? '');
	if (!eventType || eventType.startsWith('__events_')) return;
	const data =
		parsed.data !== null && typeof parsed.data === 'object'
			? (parsed.data as Record<string, unknown>)
			: {};
	const ts = data.timestamp;
	if (typeof ts === 'number' && ts > lastSeenEventTs) lastSeenEventTs = ts;
	cards.update((current) => {
		const next = applyEvent(current, { event_type: eventType, data }, { nowMs: Date.now() });
		return next.filter(
			(card) =>
				card.kind !== 'actionable' ||
				!isActionableNotificationSuppressed(card.correlationId)
		);
	});
}

// ── Connection lifecycle (copied from pendingHitlStore.ts ~lines 79-87) ──
let connection: AbortController | null = null;
let lastScopeKey = '';
let scopeUnsub: (() => void) | null = null;

// Reconnect backoff after the stream ends (cleanly OR via timeout).
// Exponential with jitter, reset when a stream actually delivers data —
// identical to pendingHitlStore.ts ~lines 329-336.
const reconnectBackoff = createBackoff({ initialMs: 2_000, maxMs: 60_000 });
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

/** Tear down the active subscription + any pending reconnect.
 *  Mirrors pendingHitlStore.disconnect (~lines 318-327). */
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

/** Schedule a reconnect, guarding against stacking and stale scopes.
 *  Mirrors pendingHitlStore.scheduleReconnect (~lines 338-350). */
function scheduleReconnect(principal: string, workspace: string): void {
	if (reconnectTimer) return; // already pending
	reconnectTimer = setTimeout(() => {
		reconnectTimer = null;
		// Scope may have changed while we were waiting — bail if the active
		// scope no longer matches the one this reconnect was queued for.
		const expectedKey = `${principal}::${workspace}`;
		if (lastScopeKey !== expectedKey) return;
		void connect(principal, workspace);
	}, reconnectBackoff.nextMs());
}

/** Open the NDJSON tail for a scope and pump lines through `ingestLine`.
 *  Reader loop + headers + URL + backoff-reset-on-data + reconnect-on-drop
 *  are copied from pendingHitlStore.connect (~lines 352-418); the only
 *  divergences are the card-store reset (vs the pending Map) and that
 *  `ingestLine` builds cards instead of counting. */
async function connect(principal: string, workspace: string): Promise<void> {
	await disconnect();
	// Reset the card list before replaying backfill so stale cards (a
	// HitlRequested whose HitlResolved fired while the previous stream was
	// timed-out/disconnected) don't linger. The /events backfill includes the
	// resolution in chronological order, so a fresh replay always converges.
	cards.set([]);
	connection = new AbortController();
	const params = new URLSearchParams();
	// Widen beyond HITL to also receive the informational events the overlay
	// now surfaces (errors / completions). The `category`
	// query param is a single COMMA-SEPARATED value: the handler splits it on
	// ',' and matches each token against `EventCategory` — see
	// `events_api.rs::parse_categories` (lines 194-214) wired via
	// `CompiledFilters::from_query` (line 131). Repeated `?category=a&category=b`
	// would NOT work (actix `web::Query<EventsQuery>` deserializes `category`
	// as a single `Option<String>`, keeping only one occurrence), so we MUST
	// use the comma form. Categories chosen from the taxonomy
	// (realtime_events.rs:2058/2106/2108/2135/2141/2152/2176):
	//   hitl          -> HitlRequested / HitlResolved
	//   pipeline      -> ProcessingError
	//   execution     -> ExecutionFailed / ExecutionCompleted
	//   agentic       -> AgenticStepFailed / AgenticMaxIterationsReached
	params.set('category', 'hitl,pipeline,execution,agentic');
	// DO NOT pin severity. The events we want span every severity:
	// `attention` (HitlRequested), `info` (HitlResolved, ExecutionCompleted),
	// `warn` (AgenticMaxIterationsReached) and `error`
	// (ExecutionFailed / ProcessingError / AgenticStepFailed) — so any pin
	// would silently drop part of the set (notably HitlResolved at `info`,
	// without which actionable cards would never dismiss). `applyEvent`
	// already filters to exactly the event_types it maps, and the server's
	// `user_relevant=true` filter (all the above are user_relevant) keeps the
	// stream free of internal noise.
	params.set('user_relevant', 'true');
	let aborted = false;
	try {
		const response = await timedFetch(`/api/magician/v3/events?${params.toString()}`, {
			signal: connection.signal,
			// NDJSON stream — must stay open for the full session. The default
			// 30s `timedFetch` timeout would abort the subscription after the
			// first half-minute; use the long-fetch timeout like pendingHitlStore.
			timeoutMs: LONG_FETCH_TIMEOUT_MS
		});
		if (!response.ok || !response.body) return;
		const reader = response.body.getReader();
		const decoder = new TextDecoder('utf-8');
		let buffer = '';
		while (true) {
			const { done, value } = await reader.read();
			if (done) return;
			// Data flowing again — future reconnects start from the fast initial
			// delay, not wherever the outage drove the backoff.
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
		// fetch aborted on scope change, unmount, or LONG_FETCH timeout. In the
		// timeout case we want to reconnect; in the unmount/scope-change case the
		// abort controller is the trigger, detected via `connection` being nulled.
		aborted = connection === null;
	} finally {
		// Reschedule a reconnect on EVERY failure unless the stream was
		// intentionally stopped (`aborted`) or the scope this connect was for is
		// no longer the active one. We key the decision on `lastScopeKey` — NOT
		// on `connection !== null` — because the overlay window is created at app
		// launch before the backend is up: the first fetch then gets a 503
		// ("backend_starting" from the vite proxy) → `!response.ok` early-return →
		// finally. The old `connection !== null` guard could skip the reschedule
		// if a concurrent `disconnect()` (teardown/scope race) had nulled
		// `connection` in that window, leaving the stream permanently dead for the
		// session (only a process restart recovered). Comparing against the
		// scope this connect was queued for instead retries reliably with backoff
		// until the backend comes up, while still NOT reconnecting after an
		// intentional stop (`stopNotifyStream` sets `lastScopeKey = ''`) or a real
		// scope change (the scope subscriber overwrites `lastScopeKey`) — so no
		// runaway/duplicate loops. `scheduleReconnect` also re-checks the scope
		// when its timer fires, and is guarded against stacking.
		if (!aborted && lastScopeKey === `${principal}::${workspace}`) {
			scheduleReconnect(principal, workspace);
		}
	}
}

/**
 * Start the overlay's HITL subscription and re-bind on scope changes.
 * Idempotent — subsequent calls reuse the single module-level scope
 * subscriber so repeated overlay mounts don't stack listeners. Mirrors
 * pendingHitlStore.ensurePendingHitlBridge (~lines 424-434).
 */
export function startNotifyStream(): void {
	if (!browser) return;
	if (scopeUnsub) return;
	scopeUnsub = scopeIdentityStore.subscribe((scope) => {
		const nextKey = `${scope.principal}::${scope.workspace}`;
		if (nextKey === lastScopeKey) return;
		lastScopeKey = nextKey;
		cards.set([]);
		void connect(scope.principal, scope.workspace);
	});
}

/** Tear down the subscription and clear the cards. Idempotent.
 *  Mirrors pendingHitlStore.teardownPendingHitlBridge (~lines 436-444). */
export function stopNotifyStream(): void {
	void disconnect();
	cards.set([]);
	lastScopeKey = '';
	if (scopeUnsub) {
		scopeUnsub();
		scopeUnsub = null;
	}
}

// ── Self-healing reconcile (stale cards + half-open live stream) ─────────────
// An actionable card is normally removed only by a LIVE matching `HitlResolved`
// or a wholesale reset on (re)connect. If a resolution is MISSED live — the
// overlay window was throttled/asleep, the broadcast lagged and dropped it (the
// `__events_lagged__` sentinel is skipped), or it fired while the connection was
// half-open after a backend restart (the reader hangs, no `done`/error, so no
// reconnect) — the card strands. This reconcile closes the gap for EVERY HITL
// source by pulling the bounded `/v3/events` backfill (last ~24h, row-capped)
// with `backfill_only=true` (the connection closes once the backfill drains, so
// there's no hanging stream to manage) and:
//   1. drops any actionable card whose correlationId has a `HitlResolved` in the
//      backfill — source-agnostic (agentic / clarification / approval / …);
//   2. if the backfill carries events NEWER than anything the LIVE stream has
//      delivered (`lastSeenEventTs`), the live stream is provably stale/half-open
//      → abort it so connect()'s `finally` reschedules a reconnect and live
//      events flow again (there is no server heartbeat, so we trigger off proven
//      staleness, never a blind idle timeout — a healthy idle stream is never
//      reconnected).
// Fail-open: a non-2xx / network / parse failure prunes nothing and forces no
// reconnect — a transient blip never drops a genuinely-pending card.
const STALE_STREAM_SKEW_MS = 5_000; // backfill must lead the live stream by this before we reconnect

export async function reconcileStaleCards(): Promise<void> {
	if (!browser) return;
	const scopeKey = lastScopeKey;
	if (!scopeKey) return; // not subscribed to a scope
	const [principal, workspace] = scopeKey.split('::');
	if (!principal || !workspace) return;

	// Same categories as the live stream so `latestBackfillTs` is comparable to
	// `lastSeenEventTs`; `backfill_only=true` returns the backfill then closes.
	const params = new URLSearchParams();
	params.set('category', 'hitl,pipeline,execution,agentic');
	params.set('user_relevant', 'true');
	params.set('backfill_only', 'true');

	let body: string;
	try {
		const response = await timedFetch(`/api/magician/v3/events?${params.toString()}`, {
		});
		if (!response.ok) return; // fail-open
		body = await response.text();
	} catch {
		return; // fail-open (also covers an aborted/closed read)
	}

	// Parse the backfill: resolved correlation_ids (any source) + newest event ts.
	const resolved = new Set<string>();
	let latestBackfillTs = 0;
	for (const line of body.split('\n')) {
		const trimmed = line.trim();
		if (!trimmed) continue;
		let evt: { event_type?: unknown; data?: unknown };
		try {
			evt = JSON.parse(trimmed);
		} catch {
			continue;
		}
		const eventType = String(evt.event_type ?? '');
		if (eventType.startsWith('__events_')) continue; // control frames carry no real event ts
		const data =
			evt.data !== null && typeof evt.data === 'object'
				? (evt.data as Record<string, unknown>)
				: {};
		const ts = data.timestamp;
		if (typeof ts === 'number' && ts > latestBackfillTs) latestBackfillTs = ts;
		// `HitlResolved` (live PascalCase) or `hitl.resolved` (dot-string sink form,
		// which nests fields under `payload`).
		if (eventType === 'HitlResolved' || eventType === 'hitl.resolved') {
			const payload =
				data.payload !== null && typeof data.payload === 'object'
					? (data.payload as Record<string, unknown>)
					: undefined;
			const cid =
				typeof data.correlation_id === 'string'
					? data.correlation_id
					: typeof payload?.correlation_id === 'string'
						? (payload.correlation_id as string)
						: '';
			if (cid) resolved.add(cid);
		}
	}

	// 1) Drop actionable cards resolved in the backfill (every source). Fail-open
	//    is implicit: a card whose correlationId is NOT in `resolved` is kept.
	let prunedAny = false;
	cards.update((list) => {
		let changed = false;
		const next = list.filter((c) => {
			if (c.kind !== 'actionable') return true;
			if (resolved.has(c.correlationId)) {
				changed = true;
				return false; // confirmed resolved → drop the orphan
			}
			return true;
		});
		if (changed) prunedAny = true;
		return changed ? next : list;
	});

	// 2) Half-open detection. The backfill (a fresh connection) reflects the true
	//    backend state; if it leads the live stream, the live stream is behind.
	//    Pruning a resolution the live stream missed is itself proof. Abort WITHOUT
	//    nulling `connection` so connect()'s `finally` treats it as a recoverable
	//    drop (→ scheduleReconnect), not an intentional stop. The `lastSeenEventTs
	//    > 0` guard skips a freshly-mounted stream still draining its own backfill.
	const liveBehind =
		lastSeenEventTs > 0 && latestBackfillTs > lastSeenEventTs + STALE_STREAM_SKEW_MS;
	if (prunedAny || liveBehind) {
		connection?.abort();
	}
}
