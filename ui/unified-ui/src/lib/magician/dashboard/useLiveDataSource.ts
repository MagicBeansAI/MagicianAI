/**
 * Live data source binding for MUI chart components.
 *
 * Each chart that accepts a `dataSource` prop calls `setupLiveDataSource`
 * from `onMount`. The helper
 * fetches the SQL via the analytics REST endpoint, calls `onRows(records)`
 * with the populated array, and re-fetches whenever a
 * `magician:dashboard-refresh` event bubbles through the chart's ancestor
 * tree (DashboardChrome's Refresh button dispatches this event).
 *
 * Returns a controller with `refresh()` and `destroy()` hooks.
 */

import { timedFetch } from '$lib/shared/fetch';
export interface LiveDataSource {
	kind: 'llm_calls_sql' | 'memory_events_sql' | 'llm_embeddings_sql';
	sql: string;
	/**
	 * Batches of the same kind run one at a time; a source in its own lane
	 * batches and waits separately, so a slow lifetime rollup does not hold
	 * back quick queries behind it. /llm puts its <=1-day windows in
	 * `LLM_SHORT_WINDOW_LANE` (`$lib/llm/queryWindow`).
	 */
	lane?: string;
}

export interface LiveDataResult {
	columns: string[];
	rows: unknown[][];
	records: Array<Record<string, unknown>>;
}

export type LiveDataHandler = (result: LiveDataResult) => void;
export type LiveDataErrorHandler = (error: Error) => void;

interface QueryResponse {
	columns: string[];
	rows: unknown[][];
}

interface QueryBatchItemResponse extends QueryResponse {
	error?: string | null;
}

interface QueryBatchResponse {
	results: QueryBatchItemResponse[];
}

interface QueuedLiveDataFetch {
	dataSource: LiveDataSource;
	resolve: (value: QueryResponse) => void;
	reject: (reason?: unknown) => void;
	signal?: AbortSignal;
}

interface PendingBatchFetch extends QueuedLiveDataFetch {
	abortHandler?: () => void;
}

const activeFetches = new Map<LiveDataSource['kind'], number>();
const pendingFetches = new Map<LiveDataSource['kind'], QueuedLiveDataFetch[]>();
/** Batch queues are per kind and lane (`kind` alone for the default lane). */
type BatchKey = string;
const pendingBatchFetches = new Map<BatchKey, PendingBatchFetch[]>();
const batchTimers = new Map<BatchKey, ReturnType<typeof setTimeout>>();
const batchInFlight = new Map<BatchKey, boolean>();

function batchKeyOf(dataSource: LiveDataSource): BatchKey {
	return dataSource.lane ? `${dataSource.kind}#${dataSource.lane}` : dataSource.kind;
}

function kindOfBatchKey(key: BatchKey): LiveDataSource['kind'] {
	return key.split('#', 1)[0] as LiveDataSource['kind'];
}

/**
 * Client cap for one `llm_calls/query_batch` request. MUST NOT exceed the
 * server's `LLM_CALLS_BATCH_MAX_QUERIES` in `magician-api/src/analytics_api.rs`:
 * the server silently drops queries past its cap (`.take(..)`), and each
 * dropped one would reject here as "batch response missing result". Large
 * enough that a whole /llm tab goes in one batch per lane, since every extra
 * batch waits for the one in flight and pays the server's setup again.
 */
export const LLM_CALLS_BATCH_MAX_QUERIES = 16;

const BATCH_CONFIG: Record<
	LiveDataSource['kind'],
	{
		endpoint: string;
		debounceMs: number;
		maxQueries: number;
		timeoutMs: number;
	}
> = {
	llm_calls_sql: {
		endpoint: '/api/magician/v2/analytics/llm_calls/query_batch',
		debounceMs: 40,
		maxQueries: LLM_CALLS_BATCH_MAX_QUERIES,
		timeoutMs: 60_000
	},
	memory_events_sql: {
		endpoint: '/api/magician/v2/analytics/memory_events/query_batch',
		debounceMs: 40,
		maxQueries: 4,
		timeoutMs: 60_000
	},
	llm_embeddings_sql: {
		endpoint: '/api/magician/v2/analytics/llm_embeddings/query_batch',
		debounceMs: 40,
		maxQueries: 8,
		timeoutMs: 60_000
	}
};

function endpointForKind(kind: LiveDataSource['kind']): string {
	if (kind === 'memory_events_sql') {
		return '/api/magician/v2/analytics/memory_events/query';
	}
	if (kind === 'llm_embeddings_sql') {
		return '/api/magician/v2/analytics/llm_embeddings/query';
	}
	return '/api/magician/v2/analytics/llm_calls/query';
}

function concurrencyForKind(kind: LiveDataSource['kind']): number {
	return kind === 'memory_events_sql' ? 1 : 3;
}

function abortError(): Error {
	const error = new Error('Live data fetch cancelled');
	error.name = 'AbortError';
	return error;
}

async function fetchOnce(dataSource: LiveDataSource, signal?: AbortSignal): Promise<QueryResponse> {
	const response = await timedFetch(endpointForKind(dataSource.kind), {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		signal,
		body: JSON.stringify({ sql: dataSource.sql })
	});
	if (!response.ok) {
		const body = await response.text();
		throw new Error(`Live data fetch failed (${response.status}): ${body}`);
	}
	return response.json();
}

async function fetchBatch(dataSources: LiveDataSource[]): Promise<QueryBatchResponse> {
	const first = dataSources[0];
	if (!first) return { results: [] };
	const config = BATCH_CONFIG[first.kind];
	const response = await timedFetch(config.endpoint, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		timeoutMs: config.timeoutMs,
		body: JSON.stringify({ queries: dataSources.map((source) => source.sql) })
	});
	if (response.status === 404 || response.status === 405) {
		return fetchBatchFallback(dataSources);
	}
	if (!response.ok) {
		const body = await response.text();
		throw new Error(`Live data batch fetch failed (${response.status}): ${body}`);
	}
	return response.json();
}

async function fetchBatchFallback(dataSources: LiveDataSource[]): Promise<QueryBatchResponse> {
	const results: QueryBatchItemResponse[] = [];
	for (const dataSource of dataSources) {
		try {
			const response = await fetchOnce(dataSource);
			results.push({ ...response, error: null });
		} catch (error) {
			results.push({
				columns: [],
				rows: [],
				error: error instanceof Error ? error.message : String(error)
			});
		}
	}
	return { results };
}

function scheduleQueuedFetches(kind: LiveDataSource['kind']): void {
	const queue = pendingFetches.get(kind);
	if (!queue || queue.length === 0) return;
	const active = activeFetches.get(kind) ?? 0;
	const limit = concurrencyForKind(kind);
	if (active >= limit) return;

	const entry = queue.shift();
	if (!entry) return;
	if (entry.signal?.aborted) {
		entry.reject(abortError());
		scheduleQueuedFetches(kind);
		return;
	}

	activeFetches.set(kind, active + 1);
	void fetchOnce(entry.dataSource, entry.signal)
		.then(entry.resolve, entry.reject)
		.finally(() => {
			activeFetches.set(kind, Math.max(0, (activeFetches.get(kind) ?? 1) - 1));
			scheduleQueuedFetches(kind);
		});

	scheduleQueuedFetches(kind);
}

function pendingBatchQueueForKey(key: BatchKey): PendingBatchFetch[] {
	const existing = pendingBatchFetches.get(key);
	if (existing) return existing;
	const queue: PendingBatchFetch[] = [];
	pendingBatchFetches.set(key, queue);
	return queue;
}

function removePendingBatchFetch(entry: PendingBatchFetch): void {
	const queue = pendingBatchFetches.get(batchKeyOf(entry.dataSource));
	if (!queue) return;
	const index = queue.indexOf(entry);
	if (index >= 0) queue.splice(index, 1);
}

function scheduleBatch(key: BatchKey): void {
	if (batchTimers.has(key) || batchInFlight.get(key)) return;
	const config = BATCH_CONFIG[kindOfBatchKey(key)];
	batchTimers.set(
		key,
		setTimeout(() => {
			batchTimers.delete(key);
			void runBatch(key);
		}, config.debounceMs)
	);
}

async function runBatch(key: BatchKey): Promise<void> {
	if (batchInFlight.get(key)) return;
	const config = BATCH_CONFIG[kindOfBatchKey(key)];
	const queue = pendingBatchQueueForKey(key);
	const batch = queue
		.splice(0, config.maxQueries)
		.filter((entry) => !entry.signal?.aborted);
	if (batch.length === 0) {
		if (queue.length > 0) scheduleBatch(key);
		return;
	}

	batchInFlight.set(key, true);
	try {
		// Identical SQL queued by several widgets is sent once and fanned out.
		const { sources, slotOf } = dedupeBatchSources(batch.map((entry) => entry.dataSource));
		const response = await fetchBatch(sources);
		batch.forEach((entry, index) => {
			if (entry.signal?.aborted) {
				entry.reject(abortError());
				return;
			}
			const item = response.results[slotOf[index]];
			if (!item) {
				entry.reject(new Error('Live data batch response missing result'));
				return;
			}
			if (item.error) {
				entry.reject(new Error(`Live data fetch failed: ${item.error}`));
				return;
			}
			entry.resolve({
				columns: item.columns,
				rows: item.rows
			});
		});
	} catch (err) {
		batch.forEach((entry) => entry.reject(err));
	} finally {
		batchInFlight.set(key, false);
		if (queue.length > 0) scheduleBatch(key);
	}
}

/**
 * Collapse identical SQL in one batch: `sources` holds each distinct query
 * once, and `slotOf[i]` is the index in `sources` (and so in the response's
 * `results`) that answers the i-th input. Exported for unit tests.
 */
export function dedupeBatchSources(dataSources: LiveDataSource[]): {
	sources: LiveDataSource[];
	slotOf: number[];
} {
	const sources: LiveDataSource[] = [];
	const slotBySql = new Map<string, number>();
	const slotOf = dataSources.map((source) => {
		const existing = slotBySql.get(source.sql);
		if (existing !== undefined) return existing;
		slotBySql.set(source.sql, sources.length);
		sources.push(source);
		return sources.length - 1;
	});
	return { sources, slotOf };
}

function batchedFetchOnce(
	dataSource: LiveDataSource,
	signal?: AbortSignal
): Promise<QueryResponse> {
	return new Promise((resolve, reject) => {
		if (signal?.aborted) {
			reject(abortError());
			return;
		}
		const entry: PendingBatchFetch = { dataSource, resolve, reject, signal };
		const onAbort = (): void => {
			removePendingBatchFetch(entry);
			reject(abortError());
		};
		entry.abortHandler = onAbort;
		signal?.addEventListener('abort', onAbort, { once: true });

		const resolveOnce = (value: QueryResponse): void => {
			if (entry.abortHandler) signal?.removeEventListener('abort', entry.abortHandler);
			resolve(value);
		};
		const rejectOnce = (reason?: unknown): void => {
			if (entry.abortHandler) signal?.removeEventListener('abort', entry.abortHandler);
			reject(reason);
		};
		entry.resolve = resolveOnce;
		entry.reject = rejectOnce;
		pendingBatchQueueForKey(batchKeyOf(dataSource)).push(entry);
		scheduleBatch(batchKeyOf(dataSource));
	});
}

function queuedFetchOnce(dataSource: LiveDataSource, signal?: AbortSignal): Promise<QueryResponse> {
	return new Promise((resolve, reject) => {
		if (signal?.aborted) {
			reject(abortError());
			return;
		}
		const entry: QueuedLiveDataFetch = { dataSource, resolve, reject, signal };
		const queue = pendingFetches.get(dataSource.kind) ?? [];
		queue.push(entry);
		pendingFetches.set(dataSource.kind, queue);

		const onAbort = (): void => {
			const pending = pendingFetches.get(dataSource.kind);
			if (pending) {
				const index = pending.indexOf(entry);
				if (index >= 0) pending.splice(index, 1);
			}
			reject(abortError());
		};
		signal?.addEventListener('abort', onAbort, { once: true });

		const resolveOnce = (value: QueryResponse): void => {
			signal?.removeEventListener('abort', onAbort);
			resolve(value);
		};
		const rejectOnce = (reason?: unknown): void => {
			signal?.removeEventListener('abort', onAbort);
			reject(reason);
		};
		entry.resolve = resolveOnce;
		entry.reject = rejectOnce;

		scheduleQueuedFetches(dataSource.kind);
	});
}

function liveDataFetchOnce(dataSource: LiveDataSource, signal?: AbortSignal): Promise<QueryResponse> {
	return BATCH_CONFIG[dataSource.kind]
		? batchedFetchOnce(dataSource, signal)
		: queuedFetchOnce(dataSource, signal);
}

function rowsToRecords(columns: string[], rows: unknown[][]): Array<Record<string, unknown>> {
	return rows.map((row) => {
		const out: Record<string, unknown> = {};
		columns.forEach((col, i) => {
			out[col] = row[i];
		});
		return out;
	});
}

export interface SetupLiveDataSourceOptions {
	dataSource: LiveDataSource;
	onRows: LiveDataHandler;
	onError?: LiveDataErrorHandler;
	onStart?: () => void;
	onSettled?: () => void;
}

export interface LiveDataSourceController {
	refresh: () => Promise<void>;
	destroy: () => void;
}

/**
 * Wire a chart to the live data source. Returns a controller with per-widget
 * refresh and cleanup hooks.
 *
 * The chart listens for the canonical `magician:dashboard-refresh` event on
 * `window`; any DashboardChrome on the page bubbles its refresh into this
 * channel, so every bound chart re-fetches in lock-step.
 */
/**
 * Value key identifying a live data source binding: same kind + lane + sql →
 * same key. Returns null for an absent source so "unbound" never collides with
 * a real query.
 */
export function liveDataKey(source: LiveDataSource | null | undefined): string | null {
	return source ? `${source.kind}\n${source.lane ?? ''}\n${source.sql}` : null;
}

export interface LiveRewirer {
	/** The active controller, or null while no source is wired. */
	readonly controller: LiveDataSourceController | null;
	/**
	 * Compare the source's value key (`liveDataKey`) against the wired key;
	 * on change, destroy the previous controller BEFORE wiring the new
	 * source. No-ops when the key is unchanged, so stable-JSON MUIJ
	 * re-renders (new object, same kind+sql) never rewire.
	 */
	sync: (source: LiveDataSource | null | undefined) => void;
	/** Tear down the active controller (call from onDestroy). */
	destroy: () => void;
}

/**
 * Shared rewire-on-query-change machinery for `dataSource`-bound components.
 *
 * Components call `sync(dataSource)` from a mounted-gated reactive statement:
 * filter-driven pages rebuild `dataSource.sql` reactively, and a controller
 * frozen at mount would keep re-fetching the original query on every refresh
 * event. `wire` receives the non-null source and returns the controller
 * (typically `setupLiveDataSource({ dataSource: source, ... })`).
 */
export function createLiveRewirer(
	wire: (source: LiveDataSource) => LiveDataSourceController
): LiveRewirer {
	let controller: LiveDataSourceController | null = null;
	let wiredKey: string | null = null;
	return {
		get controller() {
			return controller;
		},
		sync(source: LiveDataSource | null | undefined): void {
			const key = liveDataKey(source);
			if (key === wiredKey) return;
			controller?.destroy();
			controller = null;
			wiredKey = key;
			if (!source) return;
			controller = wire(source);
		},
		destroy(): void {
			controller?.destroy();
			controller = null;
		}
	};
}

export function setupLiveDataSource(opts: SetupLiveDataSourceOptions): LiveDataSourceController {
	const { dataSource, onRows, onError, onStart, onSettled } = opts;
	let disposed = false;
	let activeRequest: AbortController | null = null;
	let requestSeq = 0;

	async function load(): Promise<void> {
		const requestId = ++requestSeq;
		activeRequest?.abort();
		activeRequest = new AbortController();
		onStart?.();
		try {
			const result = await liveDataFetchOnce(dataSource, activeRequest.signal);
			if (disposed || requestId !== requestSeq) return;
			onRows({
				columns: result.columns,
				rows: result.rows,
				records: rowsToRecords(result.columns, result.rows)
			});
		} catch (err) {
			const error = err instanceof Error ? err : new Error(String(err));
			if (disposed || error.name === 'AbortError') return;
			console.warn('[live-data]', {
				kind: dataSource.kind,
				error: error.message,
				sql: dataSource.sql.slice(0, 240)
			});
			if (onError) onError(error);
			else console.error('[live-data]', error);
		} finally {
			if (!disposed && requestId === requestSeq) {
				onSettled?.();
			}
		}
	}

	void load();

	const handler = (): void => {
		void load();
	};
	// Attach to window so any dashboard chrome's dispatch reaches us.
	// `magician:dashboard-refresh` is the canonical event name.
	window.addEventListener('magician:dashboard-refresh', handler);

	return {
		refresh: load,
		destroy: () => {
			disposed = true;
			activeRequest?.abort();
			window.removeEventListener('magician:dashboard-refresh', handler);
		}
	};
}
