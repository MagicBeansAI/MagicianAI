import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { LiveDataSource, LiveDataSourceController } from './useLiveDataSource';

const timedFetch = vi.hoisted(() => vi.fn());
vi.mock('$lib/shared/fetch', () => ({ timedFetch }));

const BATCH_URL = '/api/magician/v2/analytics/llm_calls/query_batch';

function queriesOf(init: RequestInit | undefined): string[] {
	return (JSON.parse(String(init?.body)) as { queries: string[] }).queries;
}

/**
 * Each result echoes its SQL so a test can see which answer reached which
 * widget. A plain object, not a `Response`, so body parsing never depends on
 * the faked timers.
 */
function batchResponse(queries: string[]) {
	const body = {
		results: queries.map((sql) => ({ columns: ['sql'], rows: [[sql]], error: null }))
	};
	return { ok: true, status: 200, json: async () => body, text: async () => '' };
}

async function loadModule() {
	// Batch queues are module state; a fresh module per test keeps them isolated.
	vi.resetModules();
	return import('./useLiveDataSource');
}

describe('dedupeBatchSources', () => {
	it('sends each distinct SQL once and maps every input to its result slot', async () => {
		const { dedupeBatchSources } = await loadModule();
		const a: LiveDataSource = { kind: 'llm_calls_sql', sql: 'SELECT 1' };
		const b: LiveDataSource = { kind: 'llm_calls_sql', sql: 'SELECT 2' };
		const { sources, slotOf } = dedupeBatchSources([a, b, { ...a }, b]);
		expect(sources.map((s) => s.sql)).toEqual(['SELECT 1', 'SELECT 2']);
		expect(slotOf).toEqual([0, 1, 0, 1]);
	});
});

describe('liveDataKey', () => {
	it('keys on kind, lane and sql', async () => {
		const { liveDataKey } = await loadModule();
		const base: LiveDataSource = { kind: 'llm_calls_sql', sql: 'SELECT 1' };
		expect(liveDataKey(base)).toBe(liveDataKey({ ...base }));
		expect(liveDataKey(base)).not.toBe(liveDataKey({ ...base, lane: 'short-window' }));
		expect(liveDataKey(null)).toBeNull();
	});
});

describe('setupLiveDataSource batching', () => {
	let controllers: LiveDataSourceController[] = [];
	let received: Record<string, string> = {};

	beforeEach(() => {
		vi.useFakeTimers();
		vi.stubGlobal('window', new EventTarget());
		timedFetch.mockReset();
		controllers = [];
		received = {};
	});

	afterEach(() => {
		for (const controller of controllers) controller.destroy();
		vi.useRealTimers();
		vi.unstubAllGlobals();
	});

	async function wire(sources: LiveDataSource[]): Promise<void> {
		const { setupLiveDataSource } = await import('./useLiveDataSource');
		for (const source of sources) {
			controllers.push(
				setupLiveDataSource({
					dataSource: source,
					onRows: ({ rows }) => {
						received[source.sql] = String(rows[0]?.[0]);
					},
					onError: () => {}
				})
			);
		}
	}

	it('caps an llm_calls batch at the server limit of 16', async () => {
		const { LLM_CALLS_BATCH_MAX_QUERIES } = await loadModule();
		expect(LLM_CALLS_BATCH_MAX_QUERIES).toBe(16);
	});

	it('sends a whole tab of default-lane queries as one request, no waterfall', async () => {
		await loadModule();
		timedFetch.mockImplementation(async (_url: string, init: RequestInit) =>
			batchResponse(queriesOf(init))
		);
		const sources = Array.from({ length: 12 }, (_, i) => ({
			kind: 'llm_calls_sql' as const,
			sql: `SELECT wide ${i}`
		}));
		await wire(sources);
		await vi.advanceTimersByTimeAsync(50);

		expect(timedFetch).toHaveBeenCalledTimes(1);
		expect(timedFetch.mock.calls[0][0]).toBe(BATCH_URL);
		expect(queriesOf(timedFetch.mock.calls[0][1])).toHaveLength(12);
		for (const source of sources) expect(received[source.sql]).toBe(source.sql);
	});

	it('splits past the cap: the overflow waits for the batch in flight', async () => {
		await loadModule();
		timedFetch.mockImplementation(async (_url: string, init: RequestInit) =>
			batchResponse(queriesOf(init))
		);
		await wire(
			Array.from({ length: 17 }, (_, i) => ({ kind: 'llm_calls_sql' as const, sql: `SELECT q ${i}` }))
		);
		await vi.advanceTimersByTimeAsync(200);

		expect(timedFetch).toHaveBeenCalledTimes(2);
		expect(queriesOf(timedFetch.mock.calls[0][1])).toHaveLength(16);
		expect(queriesOf(timedFetch.mock.calls[1][1])).toEqual(['SELECT q 16']);
	});

	it('runs the short-window lane as its own batch, independent of the wide one', async () => {
		await loadModule();
		let releaseWide: () => void = () => {};
		const wideGate = new Promise<void>((resolve) => {
			releaseWide = resolve;
		});
		timedFetch.mockImplementation(async (_url: string, init: RequestInit) => {
			const queries = queriesOf(init);
			if (queries.some((sql) => sql.includes('wide'))) await wideGate;
			return batchResponse(queries);
		});
		await wire([
			{ kind: 'llm_calls_sql', sql: 'SELECT wide 30d' },
			{ kind: 'llm_calls_sql', sql: 'SELECT short today', lane: 'short-window' },
			{ kind: 'llm_calls_sql', sql: 'SELECT wide 7d' },
			{ kind: 'llm_calls_sql', sql: 'SELECT short yesterday', lane: 'short-window' }
		]);
		await vi.advanceTimersByTimeAsync(50);

		// Two concurrent requests, one per lane, each carrying only its lane.
		expect(timedFetch).toHaveBeenCalledTimes(2);
		const batches = timedFetch.mock.calls.map((call) => queriesOf(call[1]).sort());
		expect(batches).toContainEqual(['SELECT short today', 'SELECT short yesterday']);
		expect(batches).toContainEqual(['SELECT wide 30d', 'SELECT wide 7d']);
		// The short lane rendered while the wide batch is still in flight.
		expect(received['SELECT short today']).toBe('SELECT short today');
		expect(received['SELECT wide 30d']).toBeUndefined();

		releaseWide();
		await vi.advanceTimersByTimeAsync(50);
		expect(received['SELECT wide 30d']).toBe('SELECT wide 30d');
		expect(received['SELECT wide 7d']).toBe('SELECT wide 7d');
	});

	it('sends identical SQL once and answers every widget that asked for it', async () => {
		await loadModule();
		timedFetch.mockImplementation(async (_url: string, init: RequestInit) =>
			batchResponse(queriesOf(init))
		);
		const answers: string[] = [];
		const { setupLiveDataSource } = await import('./useLiveDataSource');
		for (let i = 0; i < 2; i++) {
			controllers.push(
				setupLiveDataSource({
					dataSource: { kind: 'llm_calls_sql', sql: 'SELECT same' },
					onRows: ({ rows }) => answers.push(String(rows[0]?.[0]))
				})
			);
		}
		await wire([{ kind: 'llm_calls_sql', sql: 'SELECT other' }]);
		await vi.advanceTimersByTimeAsync(50);

		expect(timedFetch).toHaveBeenCalledTimes(1);
		expect(queriesOf(timedFetch.mock.calls[0][1])).toEqual(['SELECT same', 'SELECT other']);
		expect(answers).toEqual(['SELECT same', 'SELECT same']);
		expect(received['SELECT other']).toBe('SELECT other');
	});
});
