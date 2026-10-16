/**
 * Notify-overlay data layer — V3 event stream → card list.
 *
 * Subscribes to this webview's event hub (`$lib/realtime/eventHub` — the
 * overlay is its own Tauri webview, so it owns its own single socket) with
 * `categories: hitl,pipeline,execution,agentic, userRelevant: true` — a
 * superset of the `hitl` filter `lib/stores/pendingHitlStore.ts` uses — and
 * reads history with one `/api/magician/v3/events?…&backfill_only=true`
 * request per scope (again on every hub resync). Instead of counting it folds each line into a `NotifyCard`:
 *   - `HitlRequested` → an `actionable` card (via `hitlEventToCard`), dropped
 *     on the matching `HitlResolved` (any outcome);
 *   - lifecycle events (errors, completions) → `success`/`error` cards
 *     (via `infoEventToCard`), coalesced so a re-emitting source shows one
 *     updating card.
 *
 * What's copied from `pendingHitlStore.ts` vs what's deliberately NOT:
 *   - SHARED — the *transport*: both ride the event hub with one REST
 *     backfill per scope/resync, so the two surfaces connect / reconnect /
 *     re-scope identically.
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
import { timedFetch } from '$lib/shared/fetch';
import { eventHub, parseNdjsonFrames, subscribeWithBackfill } from '$lib/realtime/eventHub';
import { hitlEventToCard, type NotifyCard } from './cardModel';
import { rememberNotchEvent, resetNotchMemory, syncNotchMemory } from './notchInbox';
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

// Newest event timestamp the LIVE stream has delivered (set in `ingestFrame`). The
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
	// injected by the caller (the live `ingestFrame`) rather than read here, so
	// this reducer stays pure + deterministic for the replay tests, which call
	// `applyEvent` without `opts` and therefore never age-gate. Gated on card
	// kind (not event_type) so it tracks `infoModel` automatically.
	if (card.kind !== 'actionable' && typeof opts?.nowMs === 'number') {
		const ts = typeof event.data?.timestamp === 'number' ? event.data.timestamp : 0;
		if (ts > 0 && opts.nowMs - ts > INFO_MAX_AGE_MS) return current;
	}

	return coalesceCard(current, card);
}

/** Fold one frame (live from the hub, or a backfill row) into the cards:
 *  skip the `__events_*` control rows, then route the canonical event
 *  through the shared reducer. */
function ingestFrame(parsed: Record<string, unknown>): void {
	const eventType = String(parsed.event_type ?? '');
	if (!eventType || eventType.startsWith('__events_')) return;
	const data =
		parsed.data !== null && typeof parsed.data === 'object'
			? (parsed.data as Record<string, unknown>)
			: {};
	const ts = data.timestamp;
	if (typeof ts === 'number' && ts > lastSeenEventTs) lastSeenEventTs = ts;
	const event = { event_type: eventType, data };
	rememberNotchEvent(event);
	cards.update((current) => {
		const next = applyEvent(current, event, { nowMs: Date.now() });
		const visible = next.filter(
			(card) =>
				card.kind !== 'actionable' ||
				!isActionableNotificationSuppressed(card.correlationId)
		);
		syncNotchMemory(visible);
		return visible;
	});
}

// ── Connection lifecycle ──
let lastScopeKey = '';
let scopeUnsub: (() => void) | null = null;
let liveUnsub: (() => void) | null = null;

const NOTIFY_CATEGORIES = ['hitl', 'pipeline', 'execution', 'agentic'];

function disconnect(): void {
	if (liveUnsub) {
		liveUnsub();
		liveUnsub = null;
	}
}

/** One `backfill_only` request with the live filter's categories. The
 *  category param is ONE comma-separated value (`events_api::parse_categories`);
 *  repeated params would keep only one. Severity is NOT pinned: the set spans
 *  `attention` (HitlRequested), `info` (HitlResolved, ExecutionCompleted),
 *  `warn` and `error`, and HitlResolved at `info` is what dismisses a card.
 *  Resets the cards first so a card whose resolution was missed while the hub
 *  was down cannot strand; the backfill replays the resolution in order. */
async function backfill(signal: AbortSignal): Promise<void> {
	const params = new URLSearchParams();
	params.set('category', NOTIFY_CATEGORIES.join(','));
	params.set('user_relevant', 'true');
	params.set('backfill_only', 'true');
	const response = await timedFetch(`/api/magician/v3/events?${params.toString()}`, { signal });
	if (signal.aborted || !response.ok) return;
	const body = await response.text();
	if (signal.aborted) return;
	cards.set([]);
	resetNotchMemory();
	for (const frame of parseNdjsonFrames(body)) ingestFrame(frame);
}

function connect(): void {
	disconnect();
	cards.set([]);
	resetNotchMemory();
	liveUnsub = subscribeWithBackfill({
		filter: { categories: NOTIFY_CATEGORIES, userRelevant: true },
		backfill: ({ signal }) => backfill(signal),
		onEvent: (event) => ingestFrame(event.raw)
	});
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
		connect();
	});
}

/** Tear down the subscription and clear the cards. Idempotent.
 *  Mirrors pendingHitlStore.teardownPendingHitlBridge (~lines 436-444). */
export function stopNotifyStream(): void {
	disconnect();
	cards.set([]);
	resetNotchMemory();
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
//      → ask the hub to reconnect, which resyncs (re-backfills) and live
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
	//    Pruning a resolution the live stream missed is itself proof. Reconnect
	//    the hub's socket; the reconnect resyncs every subscriber. The
	//    `lastSeenEventTs > 0` guard skips a freshly-mounted overlay.
	const liveBehind =
		lastSeenEventTs > 0 && latestBackfillTs > lastSeenEventTs + STALE_STREAM_SKEW_MS;
	if (prunedAny || liveBehind) {
		eventHub.reconnectNow();
	}
}
