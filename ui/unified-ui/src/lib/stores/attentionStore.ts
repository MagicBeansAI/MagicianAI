import { browser } from '$app/environment';
import { get, writable } from 'svelte/store';

import type {
	FeedAttentionCounts,
	FeedAttentionPages,
	FeedAttentionResponse,
	FeedAttentionTotals,
	FeedItem
} from '$lib/feed/types';
import { getV2EventSequence, v2Events, type V2WebSocketEvent } from '$lib/realtime/v2-websocket';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { timedFetch } from '$lib/shared/fetch';

const POLL_INTERVAL_MS = 15_000;
const REALTIME_DEBOUNCE_MS = 750;
// The bot-auth ping (`pingBotsAuthForBrokerTransition`) only *drives* the
// backend AuthHitlBroker; the live GWS probe it triggers is already cached
// backend-side (~10 min). Pinging on the full 15s attention cadence adds no
// freshness, so the ping is throttled to its own slower cadence and skipped
// while the tab is hidden.
const AUTH_PING_INTERVAL_MS = 60_000;
let lastAuthPingAt = 0;
const MAX_ATTENTION_FETCH_LIMIT = 200;

function emptyCounts(): FeedAttentionCounts {
	return {
		requests: 0,
		approvals: 0,
		escalations: 0,
		needs_action: 0,
		failed: 0,
		running: 0
	};
}

function emptyTotals(): FeedAttentionTotals {
	return { requests: 0, approvals: 0, escalations: 0, failed: 0, running: 0 };
}

function emptyPages(): FeedAttentionPages {
	const page = { total: 0, limit: ATTENTION_PAGE, cursor: null, next_cursor: null, has_more: false };
	return {
		requests: { ...page },
		approvals: { ...page },
		escalations: { ...page },
		failed: { ...page },
		running: { ...page }
	};
}

export interface AttentionStoreState {
	isLoading: boolean;
	error: string | null;
	expanded: boolean;
	lastLoadedAt: number | null;
	counts: FeedAttentionCounts;
	/** True per-lane totals (may exceed the loaded lists — drives "load more"). */
	totals: FeedAttentionTotals;
	/** Current server page size for the volume lanes. */
	limit: number;
	/** Cursor page metadata for the loaded volume lanes. */
	pages: FeedAttentionPages;
	requests: FeedItem[];
	approvals: FeedItem[];
	escalations: FeedItem[];
	failed: FeedItem[];
	running: FeedItem[];
}

export interface AttentionResolutionNotice {
	revision: number;
	correlationId: string;
}

export interface AttentionResolutionJournal {
	revision: number;
	notices: AttentionResolutionNotice[];
}

const ATTENTION_PAGE = 25;

const defaultState: AttentionStoreState = {
	isLoading: false,
	error: null,
	expanded: false,
	lastLoadedAt: null,
	counts: emptyCounts(),
	totals: emptyTotals(),
	limit: ATTENTION_PAGE,
	pages: emptyPages(),
	requests: [],
	approvals: [],
	escalations: [],
	failed: [],
	running: []
};

function isFeedDeltaEvent(event: V2WebSocketEvent): boolean {
	return (
		event.event_type === 'FeedItemCreated'
		|| event.event_type === 'FeedItemUpdated'
		|| event.event_type === 'FeedItemRemoved'
	);
}

/**
 * Canonical HITL envelopes that should also trigger a refresh /
 * in-place mutation. The polled `/feed/attention` runs every 15s,
 * so without these the bar would lag behind a HITL resolve by up to
 * 15s — long enough for the operator to click the now-stale row and
 * get a 404. Listening to the canonical bus events drops resolved
 * rows in the same frame, then schedules a refresh to fold in any
 * new feed projections.
 */
function isHitlCanonicalEvent(event: V2WebSocketEvent): boolean {
	// Widen via string: the V2WebSocketEvent typed union doesn't include
	// every canonical event_type the bus carries (HitlRequested /
	// HitlResolved live in the wider taxonomy). String compare avoids
	// type-narrowing failures while still being exact-match.
	const t = String(event.event_type);
	return t === 'HitlRequested' || t === 'HitlResolved';
}

function extractCorrelationId(event: V2WebSocketEvent): string | null {
	const data = (event as { data?: unknown }).data;
	if (!data || typeof data !== 'object') return null;
	const record = data as Record<string, unknown>;
	const candidates = ['correlation_id', 'pause_state_id', 'approval_id', 'request_id'];
	for (const key of candidates) {
		const value = record[key];
		if (typeof value === 'string' && value.length > 0) return value;
	}
	return null;
}

function itemMatchesCorrelation(item: FeedItem, correlationId: string): boolean {
	if (item.id === correlationId) return true;
	const metadata = item.metadata as Record<string, unknown> | undefined;
	if (!metadata) return false;
	const candidates = ['correlation_id', 'pause_state_id', 'approval_id', 'request_id'];
	for (const key of candidates) {
		const value = metadata[key];
		if (typeof value === 'string' && value === correlationId) return true;
	}
	return false;
}

function dropItemFromState(state: AttentionStoreState, correlationId: string): AttentionStoreState {
	let mutated = false;
	const filterList = (rows: FeedItem[]): FeedItem[] => {
		const next = rows.filter((row) => !itemMatchesCorrelation(row, correlationId));
		if (next.length !== rows.length) mutated = true;
		return next;
	};
	const requests = filterList(state.requests);
	const approvals = filterList(state.approvals);
	const escalations = filterList(state.escalations);
	if (!mutated) return state;
	const droppedRequests = state.requests.length - requests.length;
	const droppedApprovals = state.approvals.length - approvals.length;
	const droppedEscalations = state.escalations.length - escalations.length;
	const droppedCount = droppedRequests + droppedApprovals + droppedEscalations;
	return {
		...state,
		requests,
		approvals,
		escalations,
		counts: {
			...state.counts,
			requests: Math.max(0, state.counts.requests - droppedRequests),
			approvals: Math.max(0, state.counts.approvals - droppedApprovals),
			escalations: Math.max(0, state.counts.escalations - droppedEscalations),
			needs_action: Math.max(0, state.counts.needs_action - droppedCount)
		},
		totals: {
			...state.totals,
			requests: Math.max(0, state.totals.requests - droppedRequests),
			approvals: Math.max(0, state.totals.approvals - droppedApprovals),
			escalations: Math.max(0, state.totals.escalations - droppedEscalations)
		},
		pages: {
			...state.pages,
			requests: {
				...state.pages.requests,
				total: Math.max(0, state.pages.requests.total - droppedRequests)
			},
			approvals: {
				...state.pages.approvals,
				total: Math.max(0, state.pages.approvals.total - droppedApprovals)
			},
			escalations: {
				...state.pages.escalations,
				total: Math.max(0, state.pages.escalations.total - droppedEscalations)
			}
		}
	};
}

function normalizeErrorMessage(error: unknown): string {
	if (error instanceof Error && error.message.trim().length > 0) {
		return error.message;
	}
	return 'Failed to load attention state';
}

function firstScopedItem(payload: FeedAttentionResponse): FeedItem | null {
	return (
		payload.approvals[0]
		|| payload.requests[0]
		|| payload.escalations[0]
		|| payload.failed[0]
		|| payload.running[0]
		|| null
	);
}

function currentScopeKey(): string {
	const scope = get(scopeIdentityStore);
	return `${scope.principal}:${scope.workspace}`;
}

function eventMatchesCurrentScope(event: V2WebSocketEvent): boolean {
	const scope = get(scopeIdentityStore);
	switch (event.event_type) {
		case 'FeedItemCreated':
			return (
				event.data.item.principal === scope.principal
				&& event.data.item.workspace === scope.workspace
			);
		case 'FeedItemUpdated':
		case 'FeedItemRemoved':
			return (
				event.data.principal === scope.principal
				&& event.data.workspace === scope.workspace
			);
		default:
			return false;
	}
}

export function createAttentionStore() {
	const { subscribe, update } = writable<AttentionStoreState>(defaultState);
	const resolutionJournal = writable<AttentionResolutionJournal>({ revision: 0, notices: [] });
	const MAX_RESOLUTION_NOTICES = 128;
	function recordResolution(correlationId: string): void {
		if (!correlationId) return;
		resolutionJournal.update((journal) => {
			const revision = journal.revision + 1;
			return {
				revision,
				notices: [
					...journal.notices.slice(-(MAX_RESOLUTION_NOTICES - 1)),
					{ revision, correlationId }
				]
			};
		});
	}

	// Persisted dismissals for failed (terminal) items. Failures are reports,
	// not actionable pauses, and the backend re-projects them on every poll, so
	// "never show again" has to be remembered client-side. Applied centrally
	// here (not in the page) so the count drops everywhere it's read — the
	// /attention list, the top-bar Attention signal (`counts.failed`), and the command
	// palette — not just on the page that owns the Dismiss button.
	const DISMISSED_FAILED_KEY = 'attention:dismissed-failed';
	function loadDismissedFailed(): Set<string> {
		if (!browser) return new Set();
		try {
			const raw = localStorage.getItem(DISMISSED_FAILED_KEY);
			return raw ? new Set(JSON.parse(raw) as string[]) : new Set();
		} catch {
			return new Set();
		}
	}
	function persistDismissedFailed(): void {
		if (!browser) return;
		try {
			localStorage.setItem(DISMISSED_FAILED_KEY, JSON.stringify([...dismissedFailedIds]));
		} catch {
			// Quota/serialization failure — in-memory dismissal still applies
			// this session; it may reappear after a reload. Non-fatal.
		}
	}
	let dismissedFailedIds: Set<string> = loadDismissedFailed();

	// Strip dismissed items from every bucket and adjust the matching counts so
	// all consumers reflect the dismissal. A failed execution can surface either
	// as a V3 attention summary (in `requests`, `attention_kind: execution.failed`)
	// or as a `status: failed` feed item (in `failed`), so we filter every bucket
	// — matched by id/correlation the same way the realtime drop does, since the
	// page dismisses by the row's dedupe key (id OR correlation_id). Counts,
	// totals, AND the per-lane page totals are decremented (not recomputed):
	// the tab counts, the pager denominator, and the header all read `totals`,
	// so leaving those standing made the tabs claim items the list no longer
	// rendered and the badge no longer counted.
	function isDismissed(item: FeedItem): boolean {
		for (const key of dismissedFailedIds) {
			if (itemMatchesCorrelation(item, key)) return true;
		}
		return false;
	}
	function applyDismissedFailed(state: AttentionStoreState): AttentionStoreState {
		if (dismissedFailedIds.size === 0) return state;
		const requests = state.requests.filter((item) => !isDismissed(item));
		const approvals = state.approvals.filter((item) => !isDismissed(item));
		const escalations = state.escalations.filter((item) => !isDismissed(item));
		const failed = state.failed.filter((item) => !isDismissed(item));
		const dRequests = state.requests.length - requests.length;
		const dApprovals = state.approvals.length - approvals.length;
		const dEscalations = state.escalations.length - escalations.length;
		const dFailed = state.failed.length - failed.length;
		if (dRequests + dApprovals + dEscalations + dFailed === 0) return state;
		return {
			...state,
			requests,
			approvals,
			escalations,
			failed,
			counts: {
				...state.counts,
				requests: Math.max(0, state.counts.requests - dRequests),
				approvals: Math.max(0, state.counts.approvals - dApprovals),
				escalations: Math.max(0, state.counts.escalations - dEscalations),
				failed: Math.max(0, state.counts.failed - dFailed),
				needs_action: Math.max(
					0,
					state.counts.needs_action - (dRequests + dApprovals + dEscalations)
				)
			},
			totals: {
				...state.totals,
				requests: Math.max(0, state.totals.requests - dRequests),
				approvals: Math.max(0, state.totals.approvals - dApprovals),
				escalations: Math.max(0, state.totals.escalations - dEscalations),
				failed: Math.max(0, state.totals.failed - dFailed)
			},
			pages: {
				...state.pages,
				requests: {
					...state.pages.requests,
					total: Math.max(0, state.pages.requests.total - dRequests)
				},
				approvals: {
					...state.pages.approvals,
					total: Math.max(0, state.pages.approvals.total - dApprovals)
				},
				escalations: {
					...state.pages.escalations,
					total: Math.max(0, state.pages.escalations.total - dEscalations)
				},
				failed: {
					...state.pages.failed,
					total: Math.max(0, state.pages.failed.total - dFailed)
				}
			}
		};
	}

	/**
	 * The server filters dismissed rows out of every response, so an item the
	 * server still returns is an item the server has not recorded a dismissal
	 * for — the local dismissal POST was fire-and-forget and may have failed.
	 * Re-posting whenever the server still serves the row converges both sides
	 * without a dedicated reconciliation endpoint: once the server records it,
	 * the row stops appearing and this stops firing. Bounded by the page size.
	 */
	function reconcileServerDismissals(payload: FeedAttentionResponse): void {
		if (dismissedFailedIds.size === 0) return;
		for (const item of [
			...payload.requests,
			...payload.approvals,
			...payload.escalations,
			...payload.failed
		]) {
			if (isDismissed(item)) {
				void postDismiss(item.id, true);
			}
		}
	}

	let activeConsumers = 0;
	let pollHandle: ReturnType<typeof setInterval> | null = null;
	let realtimeUnsubscribe: (() => void) | null = null;
	let scopeUnsubscribe: (() => void) | null = null;
	let debounceHandle: ReturnType<typeof setTimeout> | null = null;
	let lastObservedEventSequence = 0;
	let inFlightRefresh: Promise<void> | null = null;
	let inFlightLoadMore: Promise<void> | null = null;
	let queuedRefresh = false;
	let lastScopeKey = '';
	type FeedLaneKey = keyof FeedAttentionTotals;
	type FeedCursorParams = Record<FeedLaneKey, string | null>;
	const feedLaneKeys: FeedLaneKey[] = ['requests', 'approvals', 'escalations', 'failed', 'running'];
	let feedNextCursors: FeedCursorParams = emptyFeedCursors();
	let feedRefreshLimit = ATTENTION_PAGE;

	function emptyFeedCursors(): FeedCursorParams {
		return { requests: null, approvals: null, escalations: null, failed: null, running: null };
	}

	function nextFeedCursors(pages?: FeedAttentionPages): FeedCursorParams {
		return {
			requests: pages?.requests?.next_cursor ?? null,
			approvals: pages?.approvals?.next_cursor ?? null,
			escalations: pages?.escalations?.next_cursor ?? null,
			failed: pages?.failed?.next_cursor ?? null,
			running: pages?.running?.next_cursor ?? null
		};
	}

	function mergeAppendedFeedCursors(
		current: FeedCursorParams,
		appendCursors: FeedCursorParams,
		pages?: FeedAttentionPages
	): FeedCursorParams {
		const payloadNext = nextFeedCursors(pages);
		const next = { ...current };
		for (const lane of feedLaneKeys) {
			if (appendCursors[lane]) {
				next[lane] = payloadNext[lane];
			}
		}
		return next;
	}

	function mergeAppendedPages(
		current: FeedAttentionPages,
		appendCursors: FeedCursorParams,
		pages?: FeedAttentionPages
	): FeedAttentionPages {
		if (!pages) return current;
		const next = { ...current };
		for (const lane of feedLaneKeys) {
			if (appendCursors[lane]) {
				next[lane] = pages[lane];
			}
		}
		return next;
	}

	function hasAnyCursor(cursors: FeedCursorParams): boolean {
		return Object.values(cursors).some((cursor) => typeof cursor === 'string' && cursor.length > 0);
	}

	function mergeFeedItems(existing: FeedItem[], incoming: FeedItem[]): FeedItem[] {
		const seen = new Set<string>();
		return [...existing, ...incoming].filter((item) =>
			seen.has(item.id) ? false : (seen.add(item.id), true)
		);
	}

	async function fetchAttention(
		cursors: FeedCursorParams = emptyFeedCursors(),
		limit = ATTENTION_PAGE
	): Promise<FeedAttentionResponse> {
		const params = new URLSearchParams();
		params.set('limit', String(Math.max(1, Math.min(MAX_ATTENTION_FETCH_LIMIT, Math.floor(limit)))));
		if (cursors.requests) params.set('requests_cursor', cursors.requests);
		if (cursors.approvals) params.set('approvals_cursor', cursors.approvals);
		if (cursors.escalations) params.set('escalations_cursor', cursors.escalations);
		if (cursors.failed) params.set('failed_cursor', cursors.failed);
		if (cursors.running) params.set('running_cursor', cursors.running);
		const suffix = params.toString();
		const response = await timedFetch(`/api/magician/v2/feed/attention${suffix ? `?${suffix}` : ''}`);
		if (!response.ok) {
			const body = await response.text().catch(() => '');
			throw new Error(body || `Failed to load attention (${response.status})`);
		}
		return (await response.json()) as FeedAttentionResponse;
	}

	async function fetchAttentionItem(itemId: string): Promise<FeedItem | null> {
		if (!browser) return null;
		const normalized = itemId.trim();
		if (!normalized) return null;
		const response = await timedFetch(
			`/api/magician/v2/feed/attention/${encodeURIComponent(normalized)}`
		);
		if (response.status === 404) return null;
		if (!response.ok) {
			const body = await response.text().catch(() => '');
			throw new Error(body || `Failed to load attention item (${response.status})`);
		}
		return (await response.json()) as FeedItem;
	}

	// Record (or clear) a server-side dismissal for a failed attention item.
	// Fire-and-forget: the optimistic in-memory + localStorage removal already
	// hid the row, so a failed POST (network blip, or an older backend without
	// the endpoint) is logged but never breaks the UX. Reuses the same relative
	// `/api/magician/v2/feed/attention` base; the bearer binds every operation to
	// the same server-side scope.
	async function postDismiss(itemId: string, dismissed: boolean): Promise<void> {
		if (!browser || !itemId) return;
		try {
			const response = await timedFetch(
				'/api/magician/v2/feed/attention/dismiss',
				{
					method: 'POST',
					headers: { 'content-type': 'application/json' },
					body: JSON.stringify({ item_id: itemId, dismissed })
				}
			);
			if (!response.ok) {
				console.warn(
					`[attentionStore] server dismiss failed (${response.status}) for ${itemId}; local dismissal still applies`
				);
			}
		} catch (error) {
			console.warn('[attentionStore] server dismiss request errored; local dismissal still applies', error);
		}
	}

	async function postUndismiss(itemId: string): Promise<void> {
		if (!browser || !itemId) return;
		try {
			const response = await timedFetch(
				'/api/magician/v2/feed/attention/undismiss',
				{
					method: 'POST',
					headers: { 'content-type': 'application/json' },
					body: JSON.stringify({ item_id: itemId })
				}
			);
			if (!response.ok) {
				console.warn(
					`[attentionStore] server undismiss failed (${response.status}) for ${itemId}`
				);
			}
		} catch (error) {
			console.warn('[attentionStore] server undismiss request errored', error);
		}
	}

	async function pingBotsAuthForBrokerTransition(): Promise<void> {
		// Bot adapters signal "needs auth" by exiting with EX_NEEDS_AUTH
		// (79) + writing a sidecar; the backend `AuthHitlBroker` (see
		// `bots/auth_hitl_broker.rs`) compares each snapshot to its
		// per-process cache on every `/bots/auth` call and emits
		// canonical `HitlRequested` / `HitlResolved` events when the
		// status transitions. Those land in the V3 attention summary,
		// the same pipeline as every other
		// HITL source.
		//
		// We hit the endpoint here purely to *drive* the broker's
		// transition detection — the response body is discarded. It is
		// throttled to AUTH_PING_INTERVAL_MS (independent of the 15s attention
		// cadence) and skipped while the tab is hidden: the live GWS probe it
		// triggers is already cached backend-side (~10 min), so a faster ping
		// buys no freshness, and a hidden tab has no operator watching the bar.
		// Real failures still surface via the sidecar on the next visible ping.
		// Best-effort: failures never break attention state (operator can
		// still navigate to Bot Control manually).
		if (typeof document !== 'undefined' && document.hidden) return;
		const now = Date.now();
		if (now - lastAuthPingAt < AUTH_PING_INTERVAL_MS) return;
		lastAuthPingAt = now;
		try {
			await timedFetch('/api/magician/v2/bots/auth');
		} catch {
			// Swallow — broker emit just doesn't fire this tick.
		}
	}

	async function runRefresh(): Promise<void> {
		if (!browser) return;
		if (inFlightRefresh) {
			queuedRefresh = true;
			return inFlightRefresh;
		}

		inFlightRefresh = (async () => {
			// An append expands feedRefreshLimit and the loaded cursor window.
			// Wait for it before taking the scope snapshot so this refresh cannot
			// overtake the append and replace the store with a smaller first page.
			if (inFlightLoadMore) await inFlightLoadMore;
			const refreshScopeKey = currentScopeKey();
			update((state) => ({
				...state,
				isLoading: state.lastLoadedAt === null,
				error: null
			}));
			try {
				// Run both in parallel: `fetchAttention` is the canonical
				// data fetch; `pingBotsAuthForBrokerTransition` drives
				// the backend `AuthHitlBroker` to compare snapshots and
				// emit `HitlRequested` / `HitlResolved` for any bot
				// transitions since the last poll. Bot auth requests
				// flow into `payload.requests` via the V3 attention
				// summary on the very same fetch — no client-side merge.
				const [payload, _bot_ping] = await Promise.all([
					fetchAttention(emptyFeedCursors(), feedRefreshLimit),
					pingBotsAuthForBrokerTransition()
				]);
				if (refreshScopeKey !== currentScopeKey()) {
					return;
				}
				const scopedItem = firstScopedItem(payload);
				if (scopedItem) {
					scopeIdentityStore.observe(scopedItem.principal, scopedItem.workspace);
				}
				feedNextCursors = nextFeedCursors(payload.pages);
				update((state) =>
					applyDismissedFailed({
						...state,
						isLoading: false,
						error: null,
						lastLoadedAt: Date.now(),
						counts: payload.counts,
						totals: payload.totals ?? emptyTotals(),
						limit: ATTENTION_PAGE,
						pages: payload.pages ?? emptyPages(),
						requests: payload.requests,
						approvals: payload.approvals,
						escalations: payload.escalations,
						failed: payload.failed,
						running: payload.running
					})
				);
				reconcileServerDismissals(payload);
			} catch (error) {
				if (refreshScopeKey !== currentScopeKey()) {
					return;
				}
				update((state) => ({
					...state,
					isLoading: false,
					error: normalizeErrorMessage(error)
				}));
			} finally {
				inFlightRefresh = null;
				if (queuedRefresh) {
					queuedRefresh = false;
					void runRefresh();
				}
			}
		})();

		return inFlightRefresh;
	}

	function runLoadMore(): Promise<void> {
		if (inFlightLoadMore) return inFlightLoadMore;
		if (!browser || !hasAnyCursor(feedNextCursors)) return Promise.resolve();
		inFlightLoadMore = (async () => {
			if (inFlightRefresh) {
				await inFlightRefresh;
				if (!hasAnyCursor(feedNextCursors)) return;
			}
			const appendCursors = { ...feedNextCursors };
			const refreshScopeKey = currentScopeKey();
			update((state) => ({ ...state, error: null }));
			try {
				const payload = await fetchAttention(appendCursors, ATTENTION_PAGE);
				if (refreshScopeKey !== currentScopeKey()) return;
				const scopedItem = firstScopedItem(payload);
				if (scopedItem) {
					scopeIdentityStore.observe(scopedItem.principal, scopedItem.workspace);
				}
				feedNextCursors = mergeAppendedFeedCursors(feedNextCursors, appendCursors, payload.pages);
				feedRefreshLimit = Math.min(MAX_ATTENTION_FETCH_LIMIT, feedRefreshLimit + ATTENTION_PAGE);
				update((state) =>
					applyDismissedFailed({
						...state,
						isLoading: false,
						error: null,
						lastLoadedAt: Date.now(),
						counts: payload.counts,
						totals: payload.totals ?? state.totals,
						limit: ATTENTION_PAGE,
						pages: mergeAppendedPages(state.pages, appendCursors, payload.pages),
						requests: appendCursors.requests
							? mergeFeedItems(state.requests, payload.requests)
							: state.requests,
						approvals: appendCursors.approvals
							? mergeFeedItems(state.approvals, payload.approvals)
							: state.approvals,
						escalations: appendCursors.escalations
							? mergeFeedItems(state.escalations, payload.escalations)
							: state.escalations,
						failed: appendCursors.failed
							? mergeFeedItems(state.failed, payload.failed)
							: state.failed,
						running: appendCursors.running
							? mergeFeedItems(state.running, payload.running)
							: state.running
					})
				);
				reconcileServerDismissals(payload);
			} catch (error) {
				if (refreshScopeKey !== currentScopeKey()) return;
				update((state) => ({ ...state, error: normalizeErrorMessage(error) }));
			}
		})().finally(() => {
			inFlightLoadMore = null;
		});
		return inFlightLoadMore;
	}

	function scheduleRefresh(): void {
		if (!browser) return;
		if (debounceHandle) {
			clearTimeout(debounceHandle);
		}
		debounceHandle = setTimeout(() => {
			debounceHandle = null;
			void runRefresh();
		}, REALTIME_DEBOUNCE_MS);
	}

	function startPolling(): void {
		if (pollHandle || !browser) return;
		pollHandle = setInterval(() => {
			void runRefresh();
		}, POLL_INTERVAL_MS);
	}

	function stopPolling(): void {
		if (pollHandle) {
			clearInterval(pollHandle);
			pollHandle = null;
		}
	}

	function startRealtimeBridge(): void {
		if (realtimeUnsubscribe || !browser) return;
		realtimeUnsubscribe = v2Events.subscribe((events) => {
			let nextSequence = lastObservedEventSequence;
			let shouldRefresh = false;
			const resolvedCorrelationIds: string[] = [];

			for (const event of events) {
				const sequence = getV2EventSequence(event);
				if (sequence <= lastObservedEventSequence) continue;
				nextSequence = Math.max(nextSequence, sequence);
				if (isFeedDeltaEvent(event) && eventMatchesCurrentScope(event)) {
					shouldRefresh = true;
					continue;
				}
				if (isHitlCanonicalEvent(event)) {
					// Schedule a polled refresh either way so attentionStore
					// folds in any new feed projections (e.g. a HitlRequested
					// that hasn't reached `/feed/attention` yet).
					shouldRefresh = true;
					if (String(event.event_type) === 'HitlResolved') {
						const correlationId = extractCorrelationId(event);
						if (correlationId) resolvedCorrelationIds.push(correlationId);
					}
				}
			}

			if (resolvedCorrelationIds.length > 0) {
				// 1-frame drop: strip the resolved rows from the in-memory
				// lists immediately so the operator doesn't see (or click)
				// stale items in the 0-15s window before the next poll.
				for (const correlationId of resolvedCorrelationIds) {
					recordResolution(correlationId);
				}
				update((state) => {
					let next = state;
					for (const cid of resolvedCorrelationIds) {
						next = dropItemFromState(next, cid);
					}
					return next;
				});
			}

			if (nextSequence <= lastObservedEventSequence) return;
			lastObservedEventSequence = nextSequence;
			if (shouldRefresh) {
				scheduleRefresh();
			}
		});
	}

	function startScopeBridge(): void {
		if (scopeUnsubscribe || !browser) return;
		lastScopeKey = currentScopeKey();
		scopeUnsubscribe = scopeIdentityStore.subscribe((scope) => {
			const scopeKey = `${scope.principal}:${scope.workspace}`;
			if (scopeKey === lastScopeKey) return;
			lastScopeKey = scopeKey;
			lastObservedEventSequence = 0;
			update((state) => ({
				...defaultState,
				expanded: state.expanded
			}));
			feedNextCursors = emptyFeedCursors();
			feedRefreshLimit = ATTENTION_PAGE;
			if (activeConsumers > 0) {
				void runRefresh();
			}
		});
	}

	function stopRealtimeBridge(): void {
		if (realtimeUnsubscribe) {
			realtimeUnsubscribe();
			realtimeUnsubscribe = null;
		}
		if (scopeUnsubscribe) {
			scopeUnsubscribe();
			scopeUnsubscribe = null;
		}
		if (debounceHandle) {
			clearTimeout(debounceHandle);
			debounceHandle = null;
		}
	}

	return {
		subscribe,
		/** Bounded lifecycle journal used by exact/deep-linked prompts. Unlike
		 *  the feed rows, it records resolutions even when the item was never in
		 *  the capped in-memory page. */
		resolutions: { subscribe: resolutionJournal.subscribe },
		start(): void {
			activeConsumers += 1;
			if (activeConsumers !== 1) return;
			if (v2Events.getConnectionState() === 'CLOSED') {
				v2Events.connectGlobal();
			}
			startPolling();
			startRealtimeBridge();
			startScopeBridge();
			void runRefresh();
		},
		stop(): void {
			activeConsumers = Math.max(0, activeConsumers - 1);
			if (activeConsumers !== 0) return;
			stopPolling();
			stopRealtimeBridge();
		},
		refresh(): Promise<void> {
			return runRefresh();
		},
		/** Resolve one scoped item without walking the cursor-paginated inbox. */
		fetchItem(itemId: string): Promise<FeedItem | null> {
			return fetchAttentionItem(itemId);
		},
		/** Fetch the next cursor page for each volume lane that still has one. */
		loadMore(): Promise<void> {
			return runLoadMore();
		},
		/** Reset cursor paging back to the first page (e.g. on scope switch). */
		resetPage(): void {
			feedNextCursors = emptyFeedCursors();
			feedRefreshLimit = ATTENTION_PAGE;
		},
		toggleExpanded(): void {
			update((state) => ({ ...state, expanded: !state.expanded }));
		},
		setExpanded(expanded: boolean): void {
			update((state) => ({ ...state, expanded }));
		},
		clearError(): void {
			update((state) => ({ ...state, error: null }));
		},
		/** Permanently dismiss a failed (terminal) attention item.
		 *
		 *  Belt-and-suspenders:
		 *   1. Optimistic + durable client-side: `id` (the row's dedupe key —
		 *      FeedItem.id OR correlation_id) is added to the in-memory set,
		 *      persisted to localStorage, and stripped from every bucket +
		 *      count immediately via `applyDismissedFailed`. Instant UX + a
		 *      graceful fallback if the server is an older build without the
		 *      dismiss endpoint.
		 *   2. Server-side (durable + cross-device): fire-and-forget POST to
		 *      `/feed/attention/dismiss`. The server keys strictly on the raw
		 *      `FeedItem.id` (never correlation_id), so we send `feedItemId`
		 *      (the row's raw `.id`) when the caller has it, falling back to
		 *      `id` only when it doesn't. The server then filters the item out
		 *      of all lanes + decrements counts on subsequent `/feed/attention`
		 *      polls, so the dismissal survives a localStorage clear / new
		 *      device. Both hide the same id — redundant but safe. */
		dismissFailed(id: string, feedItemId?: string): void {
			const serverItemId = feedItemId ?? id;
			void postDismiss(serverItemId, true);
			if (dismissedFailedIds.has(id)) return;
			dismissedFailedIds.add(id);
			persistDismissedFailed();
			update((state) => applyDismissedFailed(state));
		},
		/** Un-dismiss a previously dismissed failed item: clears it from the
		 *  server-side store (so it can re-surface) and drops it from the local
		 *  persisted set. Mirror of `dismissFailed`; only wire a UI affordance
		 *  for this if there's a visible undo. Fire-and-forget on the server;
		 *  local removal is synchronous. */
		undismissFailed(id: string, feedItemId?: string): void {
			const serverItemId = feedItemId ?? id;
			void postUndismiss(serverItemId);
			if (dismissedFailedIds.delete(id)) {
				persistDismissedFailed();
			}
		},
		/** Optimistically drop a HITL item (request/approval/escalation) by any
		 *  of its ids — correlation/pause/approval/request — the instant it's
		 *  resolved, whether from the local respond click or a remote
		 *  `HitlResolved` relayed by the SSE bus. Lets the feed copy disappear
		 *  immediately instead of waiting on the v2 WS relay or the 15s poll.
		 *  Idempotent: a no-op if no bucket holds the id. */
		dropResolved(correlationId: string): void {
			if (!correlationId) return;
			recordResolution(correlationId);
			update((state) => dropItemFromState(state, correlationId));
		}
	};
}

export const attentionStore = createAttentionStore();
