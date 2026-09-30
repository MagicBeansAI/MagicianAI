/**
 * Coding-spine store — the live data spine behind the VibeDev cockpit.
 *
 * Owns ONE scope-level (`principal`+`workspace`) NDJSON tail of the `coding.*`
 * event stream and folds every line through `spineModel`. This lifts
 * `connectCodingStream` / `ingestCodingLine` out of the `+page.svelte` monolith
 * and DROPS its 12-row cap + 180-char detail trim — the view windows the list,
 * the store keeps everything. Run scoping is done client-side by the view
 * (run-chain task ids), matching the proven monolith behaviour, so the store
 * needs no unverified server-side `task_id` filter.
 *
 * Transport: `GET /api/magician/v3/events?event_type=coding.&since=&limit=`.
 * The handler substring-matches `event_type`, replays a bounded backfill, then
 * holds the connection open as a live tail (NDJSON, not SSE). Reconnect uses
 * the shared jittered backoff + a connect watchdog.
 */
import { writable, type Readable } from 'svelte/store';
import { browser } from '$app/environment';
import { timedFetch, LONG_FETCH_TIMEOUT_MS } from '$lib/shared/fetch';
import { createBackoff } from '$lib/realtime/backoff';
import {
	applyCodingEvent,
	emptySpineState,
	spineCardsList,
	unwrapCodingEvent,
	type RunMeta,
	type SpineCard,
	type SpineState
} from '$lib/shell/vibe/conversation/spineModel';

export type CodingStreamState = 'idle' | 'connecting' | 'live' | 'closed' | 'error';

export interface CodingSpineSnapshot {
	cards: SpineCard[];
	/** Per-shadow run metadata (cost/ctx/retry/queue/compaction). */
	meta: Map<string, RunMeta>;
	streamState: CodingStreamState;
	streamMessage: string;
}

const CONNECT_TIMEOUT_MS = 12_000;
const BACKFILL_LIMIT = 600;
const BACKFILL_WINDOW_MS = 24 * 60 * 60 * 1000;
// Per-run history hydrate (`hydrateRun`): a one-shot backfill that reaches PAST
// the 24h live tail so a finished run opened on refresh shows its recorded
// timeline. Wider window + higher cap than the live backfill; bounded to keep
// the one-shot fetch sane.
const HYDRATE_WINDOW_MS = 14 * 24 * 60 * 60 * 1000;
const HYDRATE_LIMIT = 4000;

function emptySnapshot(): CodingSpineSnapshot {
	return { cards: [], meta: new Map(), streamState: 'idle', streamMessage: '' };
}

function createCodingSpineStore() {
	const { subscribe, set, update } = writable<CodingSpineSnapshot>(emptySnapshot());

	let state: SpineState = emptySpineState();
	let connection: AbortController | null = null;
	let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
	let scopeKey = '';
	let principal = '';
	let workspace = '';
	// Runs whose AUTHORITATIVE durable log has been folded — the live scope tail
	// then skips their events so a recent run can't be double-folded (durable
	// coalesced cards + live individual deltas) when a cold-load hydrate races the
	// tail backfill. Reset on scope change (see `start`).
	let durablyHydratedTaskIds = new Set<string>();
	const backoff = createBackoff({ initialMs: 2_000, maxMs: 60_000 });

	function publish(streamState: CodingStreamState, streamMessage = ''): void {
		set({
			cards: spineCardsList(state),
			meta: state.meta,
			streamState,
			streamMessage
		});
	}

	function publishStatus(streamState: CodingStreamState, streamMessage = ''): void {
		update((snap) => ({ ...snap, streamState, streamMessage }));
	}

	function foldLine(target: SpineState, line: string, skipTaskIds?: Set<string>): boolean {
		let parsed: Record<string, unknown>;
		try {
			parsed = JSON.parse(line);
		} catch {
			return false;
		}
		// Tolerate the backend's synthetic lag/partial markers.
		if (parsed.__events_lagged__ || parsed.__events_partial__) return false;
		const event = unwrapCodingEvent(parsed);
		if (!event) return false;
		// A run already folded from its AUTHORITATIVE durable log must not also be
		// folded from the live scope tail — the tail's individual per-token deltas
		// would duplicate/garble the coalesced durable cards (different turn ids).
		if (skipTaskIds && skipTaskIds.size > 0) {
			const tid = event.payload.task_id;
			if (typeof tid === 'string' && skipTaskIds.has(tid)) return false;
		}
		return applyCodingEvent(target, event);
	}
	function ingestLine(line: string): boolean {
		return foldLine(state, line, durablyHydratedTaskIds);
	}

	async function connect(): Promise<void> {
		disconnect();
		const controller = new AbortController();
		connection = controller;
		publishStatus('connecting');
		let connectTimedOut = false;
		const watchdog = setTimeout(() => {
			connectTimedOut = true;
			controller.abort();
		}, CONNECT_TIMEOUT_MS);
		try {
			const params = new URLSearchParams();
			params.set('event_type', 'coding.');
			params.set('limit', String(BACKFILL_LIMIT));
			params.set('since', String(Date.now() - BACKFILL_WINDOW_MS));
			const response = await timedFetch(`/api/magician/v3/events?${params.toString()}`, {
				signal: controller.signal,
				timeoutMs: LONG_FETCH_TIMEOUT_MS
			});
			if (controller !== connection) return;
			clearTimeout(watchdog);
			if (!response.ok || !response.body) {
				publishStatus('error', `${response.status} ${response.statusText}`);
				scheduleReconnect();
				return;
			}
			backoff.reset();
			publish('live');
			const reader = response.body.getReader();
			const decoder = new TextDecoder('utf-8');
			let buffer = '';
			let dirty = false;
			let flushTimer: ReturnType<typeof setTimeout> | null = null;
			const flush = () => {
				flushTimer = null;
				if (dirty) {
					dirty = false;
					publish('live');
				}
			};
			while (true) {
				const { done, value } = await reader.read();
				if (controller !== connection) return;
				if (done) {
					if (dirty) publish('live');
					publishStatus('closed');
					scheduleReconnect();
					return;
				}
				backoff.reset();
				buffer += decoder.decode(value, { stream: true });
				let nl: number;
				while ((nl = buffer.indexOf('\n')) !== -1) {
					const rawLine = buffer.slice(0, nl);
					buffer = buffer.slice(nl + 1);
					if (rawLine.trim() && ingestLine(rawLine)) dirty = true;
				}
				// Coalesce rapid deltas into one publish per animation frame-ish tick.
				if (dirty && flushTimer === null) flushTimer = setTimeout(flush, 80);
			}
		} catch (error) {
			if (controller !== connection) return;
			clearTimeout(watchdog);
			if (controller.signal.aborted) {
				if (connectTimedOut) {
					publishStatus('error', 'Coding event stream did not connect in time.');
					scheduleReconnect();
				} else {
					publishStatus('closed');
				}
				return;
			}
			publishStatus('error', error instanceof Error ? error.message : String(error));
			scheduleReconnect();
		} finally {
			clearTimeout(watchdog);
		}
	}

	function scheduleReconnect(): void {
		if (!browser) return;
		if (reconnectTimer) {
			clearTimeout(reconnectTimer);
			reconnectTimer = null;
		}
		const key = scopeKey;
		reconnectTimer = setTimeout(() => {
			reconnectTimer = null;
			if (scopeKey !== key) return;
			void connect();
		}, backoff.nextMs());
	}

	function disconnect(): void {
		if (reconnectTimer) {
			clearTimeout(reconnectTimer);
			reconnectTimer = null;
		}
		if (connection) {
			connection.abort();
			connection = null;
		}
	}

	return {
		subscribe: subscribe as Readable<CodingSpineSnapshot>['subscribe'],

		/** Start (or rebind) the scope-level live tail. Idempotent per scope. */
		start(scope: { principal?: string | null; workspace?: string | null } | null | undefined): void {
			if (!browser || !scope?.principal || !scope?.workspace) return;
			const key = `${scope.principal}::${scope.workspace}`;
			if (key === scopeKey && connection) return;
			scopeKey = key;
			principal = scope.principal;
			workspace = scope.workspace;
			// New scope → fresh spine.
			state = emptySpineState();
			durablyHydratedTaskIds = new Set();
			publish('connecting');
			void connect();
		},

		stop(): void {
			disconnect();
			scopeKey = '';
			state = emptySpineState();
			durablyHydratedTaskIds = new Set();
			set(emptySnapshot());
		},

		/**
		 * One-shot per-run history hydrate. Pulls recorded `coding.*` events from
		 * the durable log with a WIDE backfill window (reaching past the live 24h
		 * tail) and folds them into the spine, so a FINISHED run opened on refresh
		 * renders its timeline instead of going blank. Folds into the shared scope
		 * state (events are keyed by id → no duplication with the live tail); the
		 * view filters the folded cards to the active run chain. Best-effort:
		 * network/parse errors are swallowed. Requires the scope to be `start()`ed
		 * (uses its principal/workspace). `sinceMs` lets the caller bound the fetch
		 * to the run's lifetime; defaults to a 14-day window.
		 */
		async hydrateRun(taskId: string, sinceMs?: number): Promise<void> {
			if (!browser || !principal || !workspace) return;
			const headers: Record<string, string> = {};
			let folded = false;
			// Primary: the DURABLE per-run coding-event log — written per execution
			// and immune to the scope-log retention trim, so it carries the FULL run
			// (every turn / tool / thinking block), ascending so turns fold correctly.
			if (taskId) {
				try {
					const params = new URLSearchParams();
					const response = await timedFetch(
						`/api/magician/v2/vibedev/runs/${encodeURIComponent(taskId)}/coding-events?${params.toString()}`,
						{ headers, timeoutMs: LONG_FETCH_TIMEOUT_MS }
					);
					if (response.ok) {
						const text = await response.text();
						// Fold the DURABLE (coalesced) stream into an ISOLATED state, then
						// REPLACE this run's cards in the shared state. Folding coalesced
						// records (full text stamped at the first delta's sequence) directly
						// into the live tail's state would mix them with its individual
						// per-token deltas on the SAME card and garble the prose; isolation
						// keeps the durable fold authoritative and self-consistent, and the
						// taskId-scoped replace wipes any partial tail-folded cards.
						const isolated = emptySpineState();
						let durableFolded = false;
						for (const line of text.split('\n')) {
							if (line.trim() && foldLine(isolated, line)) durableFolded = true;
						}
						if (durableFolded) {
							for (const [id, card] of state.cards) {
								if (card.taskId === taskId) state.cards.delete(id);
							}
							for (const [id, card] of isolated.cards) state.cards.set(id, card);
							for (const [shadow, runMeta] of isolated.meta) state.meta.set(shadow, runMeta);
							for (const [shadow, t] of isolated.turnByShadow) state.turnByShadow.set(shadow, t);
							durablyHydratedTaskIds.add(taskId);
							folded = true;
						}
					}
				} catch {
					// fall through to the scope-tail backfill
				}
			}
			// Fallback: a run predating the durable log (e.g. created before this
			// fix) has no per-run file — fold whatever survives in the (trimmed)
			// scope tail so it isn't totally blank. Pre-fix behaviour for old runs.
			if (!folded) {
				const params = new URLSearchParams();
				params.set('event_type', 'coding.');
				params.set('backfill_only', 'true');
				params.set('since', String(sinceMs ?? Date.now() - HYDRATE_WINDOW_MS));
				params.set('limit', String(HYDRATE_LIMIT));
				try {
					const response = await timedFetch(`/api/magician/v3/events?${params.toString()}`, {
						headers,
						timeoutMs: LONG_FETCH_TIMEOUT_MS
					});
					if (response.ok) {
						const text = await response.text();
						for (const line of text.split('\n')) {
							if (line.trim() && ingestLine(line)) folded = true;
						}
					}
				} catch {
					// best-effort hydrate; the live tail + empty-state copy remain
				}
			}
			if (folded) {
				// Refresh cards/meta WITHOUT touching the scope tail's streamState.
				update((snap) => ({ ...snap, cards: spineCardsList(state), meta: state.meta }));
			}
		}
	};
}

export const codingSpineStore = createCodingSpineStore();
