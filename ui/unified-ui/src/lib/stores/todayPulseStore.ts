/**
 * Today's Pulse store — one shared 60s poll behind the Today page's
 * analytics band.
 *
 * Data flow per tick (all three POSTs run in PARALLEL; the LLM slice can
 * publish first, then the full snapshot replaces it when every slice settles):
 * - `/api/magician/v2/analytics/llm_calls/query`      ← buildLlmPulseSql
 * - `/api/magician/v2/analytics/query` (events table) ← buildCodingPulseSql
 * - `/api/magician/v2/analytics/memory_events/query`  ← buildMemoryPulseSql
 * All are POST `{ sql }` → `{ columns, rows }`. The workspace-bound bearer is
 * attached by `installScopedApiFetch`'s window.fetch patch —
 * this store never builds scope params itself, it only tracks the scope KEY
 * to reset on switches and to drop stale-scope responses.
 *
 * Task counts are NOT fetched here: they're computed from the task list the
 * UI already holds (`computeTaskCounts(get(taskStore).tasks, now)`). The
 * Today page starts taskStore before this band mounts, so the list is live;
 * if taskStore ever isn't started, counts read 0 — acceptable for a
 * decorative band, and no second /tasks poller is worth that edge.
 *
 * Failure semantics (pulse is decorative, never a page blocker):
 * - Partial failure (1-2 of the 3 queries fail): still publish — each failed
 *   slice keeps its values from the previous same-scope snapshot (last-good
 *   carry-forward), so a transient endpoint blip never renders a fake $0.00
 *   beside real numbers. Only when NO prior snapshot exists in this scope
 *   does the failed slice zero-fill. Failed slices are listed in
 *   `staleSlices` so the band could dim them; it's empty on a clean tick.
 * - Total failure (all 3 fail): KEEP the last snapshot (all slices marked
 *   stale) and throw into the shared poll (drives its backoff).
 *   `unavailable` turns true only when there has never been a snapshot in
 *   this scope — that's the band's "hide entirely" signal.
 * - console.warn fires ONCE per failure streak (any slice failing), not per
 *   60s tick; a fully clean tick resets the streak.
 *
 * Scope switch: mirror of todayStore's scope bridge — reset to
 * null/loading, `pollNow()` under the new scope; in-flight responses from
 * the old scope are dropped by comparing the scope key captured at fetch
 * start against the current one before publishing.
 */

import { get, writable } from 'svelte/store';
import { browser } from '$app/environment';

import { createSharedPoll } from '$lib/stores/sharedPoll';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { taskStore } from '$lib/stores/taskStore';
import { timedFetch } from '$lib/shared/fetch';
import {
	assemblePulseSnapshot,
	buildCodingPulseSql,
	buildLlmPulseSql,
	buildMemoryPulseSql,
	computeTaskCounts,
	parseCodingPulse,
	parseLlmPulse,
	parseMemoryPulse,
	type CodingPulse,
	type LlmPulse,
	type MemoryPulse,
	type PulseQueryResponse,
	type PulseSnapshot
} from '$lib/today/pulseQueries';

/** The three fetched slices of the snapshot (task counts never fail). */
export type PulseSliceName = 'llm' | 'coding' | 'memory';

export interface TodayPulseState {
	/** Last assembled snapshot; kept across failed refreshes. `null` until
	 *  the first successful fetch in the current scope. */
	snapshot: PulseSnapshot | null;
	/** True from store start / scope switch until the first fetch settles. */
	loading: boolean;
	/** True only when there has never been a snapshot in this scope AND the
	 *  last refresh failed outright — the band hides on this. */
	unavailable: boolean;
	/** Slices whose latest fetch failed — their snapshot values are carried
	 *  forward from the previous same-scope snapshot (or zero-filled when
	 *  none exists). Empty when the last tick was fully fresh. */
	staleSlices: ReadonlyArray<PulseSliceName>;
}

const POLL_INTERVAL_MS = 60_000;

const LLM_CALLS_ENDPOINT = '/api/magician/v2/analytics/llm_calls/query';
const EVENTS_ENDPOINT = '/api/magician/v2/analytics/query';
const MEMORY_EVENTS_ENDPOINT = '/api/magician/v2/analytics/memory_events/query';

async function runPulseQuery(endpoint: string, sql: string): Promise<PulseQueryResponse> {
	const response = await timedFetch(endpoint, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ sql })
	});
	if (!response.ok) {
		throw new Error(`Pulse query failed (${response.status}) at ${endpoint}`);
	}
	const payload = (await response.json()) as Partial<PulseQueryResponse>;
	return {
		columns: Array.isArray(payload.columns) ? payload.columns : [],
		rows: Array.isArray(payload.rows) ? payload.rows : []
	};
}

const ALL_SLICES: ReadonlyArray<PulseSliceName> = ['llm', 'coding', 'memory'];

/** Zero-fill fallback for a rejected slice with no prior snapshot to carry:
 *  parse an empty result set (fresh object per call — parsers own it). */
function emptyResponse(): PulseQueryResponse {
	return { columns: [], rows: [] };
}

function createTodayPulseStore() {
	const state = writable<TodayPulseState>({
		snapshot: null,
		loading: true,
		unavailable: false,
		staleSlices: []
	});

	let activeConsumers = 0;
	let pollUnsubscribe: (() => void) | null = null;
	let scopeUnsubscribe: (() => void) | null = null;
	let lastScopeKey = '';
	/** One console.warn per failure streak; cleared by a fully clean tick. */
	let warnedThisStreak = false;

	function currentScopeKey(): string {
		const scope = get(scopeIdentityStore);
		return `${scope.principal}:${scope.workspace}`;
	}

	async function fetchPulse(): Promise<PulseSnapshot | null> {
		const fetchScopeKey = currentScopeKey();
		const now = new Date();
		const llmPromise = runPulseQuery(LLM_CALLS_ENDPOINT, buildLlmPulseSql(now));
		const codingPromise = runPulseQuery(EVENTS_ENDPOINT, buildCodingPulseSql(now));
		const memoryPromise = runPulseQuery(MEMORY_EVENTS_ENDPOINT, buildMemoryPulseSql(now));
		const settledPromise = Promise.allSettled([llmPromise, codingPromise, memoryPromise]);

		try {
			const llmRes = await llmPromise;
			if (fetchScopeKey !== currentScopeKey()) return null;
			const prev = get(state).snapshot;
			const snapshot = assemblePulseSnapshot(
				parseLlmPulse(llmRes),
				prev ? { codingRunsToday: prev.codingRunsToday } : parseCodingPulse(emptyResponse()),
				prev
					? { memoriesToday: prev.memoriesToday, evals: prev.evals }
					: parseMemoryPulse(emptyResponse()),
				computeTaskCounts(get(taskStore).tasks, now)
			);
			state.set({
				snapshot,
				loading: false,
				unavailable: false,
				staleSlices: ['coding', 'memory']
			});
		} catch {
			// The full-settlement path below owns warnings, carry-forward, and
			// shared-poll backoff. A failed LLM slice just means no early paint.
		}

		const settled = await settledPromise;

		// Stale-scope drop: the scope bridge already reset state and
		// re-polled under the new scope — this response must not publish,
		// and it isn't a failure either (no warn, no unavailable).
		if (fetchScopeKey !== currentScopeKey()) return null;

		const failures = settled.filter(
			(r): r is PromiseRejectedResult => r.status === 'rejected'
		);
		if (failures.length > 0 && !warnedThisStreak) {
			warnedThisStreak = true;
			console.warn('[today-pulse] refresh degraded', {
				failedQueries: failures.length,
				firstError:
					failures[0].reason instanceof Error
						? failures[0].reason.message
						: String(failures[0].reason)
			});
		}

		if (failures.length === settled.length) {
			// Total failure: keep the last snapshot (every slice is stale);
			// the band only reads `unavailable` when it has nothing to show.
			state.update((s) => ({
				snapshot: s.snapshot,
				loading: false,
				unavailable: s.snapshot === null,
				staleSlices: s.snapshot === null ? [] : ALL_SLICES
			}));
			// Throw into the shared poll so its backoff slows a dead backend.
			throw new Error('Today pulse refresh failed (all queries)');
		}
		if (failures.length === 0) warnedThisStreak = false;

		const [llmRes, codingRes, memoryRes] = settled;
		// Last-good carry-forward: `prev` is same-scope by construction — the
		// scope bridge nulls the snapshot on switch, and the stale-scope drop
		// above bars old-scope responses from ever publishing into it.
		const prev = get(state).snapshot;
		const staleSlices: PulseSliceName[] = [];

		let llm: LlmPulse;
		if (llmRes.status === 'fulfilled') {
			llm = parseLlmPulse(llmRes.value);
		} else {
			staleSlices.push('llm');
			llm = prev ? prev.llm : parseLlmPulse(emptyResponse());
		}

		let coding: CodingPulse;
		if (codingRes.status === 'fulfilled') {
			coding = parseCodingPulse(codingRes.value);
		} else {
			staleSlices.push('coding');
			coding = prev
				? { codingRunsToday: prev.codingRunsToday }
				: parseCodingPulse(emptyResponse());
		}

		let memory: MemoryPulse;
		if (memoryRes.status === 'fulfilled') {
			memory = parseMemoryPulse(memoryRes.value);
		} else {
			staleSlices.push('memory');
			memory = prev
				? { memoriesToday: prev.memoriesToday, evals: prev.evals }
				: parseMemoryPulse(emptyResponse());
		}

		const snapshot = assemblePulseSnapshot(
			llm,
			coding,
			memory,
			// Piggyback on the task list the page already polls — see the
			// module header for the not-started ⇒ zero-counts contract.
			computeTaskCounts(get(taskStore).tasks, now)
		);
		state.set({ snapshot, loading: false, unavailable: false, staleSlices });
		return snapshot;
	}

	const poll = createSharedPoll<PulseSnapshot | null>({
		fetcher: fetchPulse,
		idleMs: POLL_INTERVAL_MS,
		// No fast leases are handed out (requestFast isn't exposed) — the
		// pulse is a background band; fastMs only exists to satisfy the API.
		fastMs: POLL_INTERVAL_MS
	});

	function startScopeBridge(): void {
		if (scopeUnsubscribe || !browser) return;
		lastScopeKey = currentScopeKey();
		scopeUnsubscribe = scopeIdentityStore.subscribe((scope) => {
			const scopeKey = `${scope.principal}:${scope.workspace}`;
			if (scopeKey === lastScopeKey) return;
			lastScopeKey = scopeKey;
			warnedThisStreak = false;
			// The previous scope's numbers must never flash under the new
			// scope — reset to the initial loading state (null snapshot also
			// bars cross-scope carry-forward), then re-poll.
			state.set({ snapshot: null, loading: true, unavailable: false, staleSlices: [] });
			poll.pollNow();
		});
	}

	function stopScopeBridge(): void {
		if (scopeUnsubscribe) {
			scopeUnsubscribe();
			scopeUnsubscribe = null;
		}
		lastScopeKey = '';
	}

	return {
		subscribe: state.subscribe,

		/** Refcounted like taskStore: the first consumer arms the shared
		 *  poll (subscribing to its readable starts the timer), the last
		 *  `stop()` parks it. The cached snapshot survives stop/start, so a
		 *  remount paints instantly and refreshes in the background. */
		start(): void {
			activeConsumers += 1;
			if (activeConsumers !== 1) return;
			startScopeBridge();
			// We never read poll.value — the public surface is our writable
			// (which also carries loading/unavailable); this subscription
			// exists purely to drive the shared poll's lifecycle.
			pollUnsubscribe = poll.value.subscribe(() => {});
		},

		stop(): void {
			activeConsumers = Math.max(0, activeConsumers - 1);
			if (activeConsumers !== 0) return;
			if (pollUnsubscribe) {
				pollUnsubscribe();
				pollUnsubscribe = null;
			}
			stopScopeBridge();
		},

		/** Immediate refresh (e.g. after a mutation lands); coalesces with
		 *  an in-flight poll and no-ops when the store isn't started. */
		pollNow(): void {
			poll.pollNow();
		}
	};
}

export const todayPulseStore = createTodayPulseStore();
