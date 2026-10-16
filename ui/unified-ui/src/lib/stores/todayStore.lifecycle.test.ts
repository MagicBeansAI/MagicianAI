import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { TodayItem, TodayResponse, TodaySections } from '$lib/today/types';

const {
	todayFetch,
	realtimeSubscribers,
	realtimeSubscribe,
	connectGlobal,
	realtimeState
} = vi.hoisted(() => {
	const subscribers = new Set<(events: Array<Record<string, unknown>>) => void>();
	return {
		todayFetch: vi.fn(),
		realtimeSubscribers: subscribers,
		realtimeSubscribe: vi.fn(
			(subscriber: (events: Array<Record<string, unknown>>) => void) => {
				subscribers.add(subscriber);
				return () => subscribers.delete(subscriber);
			}
		),
		connectGlobal: vi.fn(),
		realtimeState: { connection: 'OPEN' }
	};
});

vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$lib/shared/fetch', () => ({ timedFetch: todayFetch }));
vi.mock('$lib/shared/stores/notifications', () => ({ showError: vi.fn() }));
vi.mock('$lib/realtime/v2-websocket', () => ({
	getV2EventSequence: (event: { sequence?: number }) => event.sequence ?? 0,
	v2Events: {
		subscribe: realtimeSubscribe,
		getConnectionState: () => realtimeState.connection,
		connectGlobal
	}
}));

import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { todaySectionCursorProbeUrl } from '$lib/today/sectionCursorProbe';
import { createTodayStore } from './todayStore';

function item(id: string, section: TodayItem['section'] = 'needs_you'): TodayItem {
	return {
		id,
		principal: 'anonymous',
		workspace: 'default',
		section,
		priority: 1,
		title: id,
		summary: null,
		reason: 'test',
		source_kind: 'task',
		source_id: id,
		source_url: null,
		space_ids: [],
		thread_id: null,
		task_id: null,
		agent_id: null,
		status: 'info',
		actions: [],
		evidence_refs: [],
		created_at: 1,
		updated_at: 1,
		expires_at: null,
		seen_at: null,
		dismissed_at: null,
		snoozed_until: null,
		metadata: null
	};
}

function payload(
	sections: Partial<TodaySections> = {},
	overrides: Partial<TodayResponse> = {}
): TodayResponse {
	const full: TodaySections = {
		needs_you: sections.needs_you ?? [],
		delivered: sections.delivered ?? [],
		changed: sections.changed ?? [],
		active_work: sections.active_work ?? [],
		followups: sections.followups ?? []
	};
	const counts = {
		needs_you: full.needs_you.length,
		delivered: full.delivered.length,
		changed: full.changed.length,
		active_work: full.active_work.length,
		followups: full.followups.length,
		total: Object.values(full).reduce((sum, rows) => sum + rows.length, 0)
	};
	return {
		principal: 'anonymous',
		workspace: 'default',
		generated_at: 100,
		freshness: { source: 'test', generated_at: 100 },
		headline: 'Today test',
		digest: {
			generated_at: 100,
			since: null,
			total: 0,
			limit: 7,
			offset: 0,
			bullets: []
		},
		sections: full,
		counts,
		...overrides
	};
}

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json' }
	});
}

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((next) => {
		resolve = next;
	});
	return { promise, resolve };
}

function emitRealtime(...events: Array<Record<string, unknown>>): void {
	for (const subscriber of realtimeSubscribers) subscriber(events);
}

describe('todayStore lifecycle', () => {
	beforeEach(() => {
		vi.useRealTimers();
		todayFetch.mockReset();
		realtimeSubscribers.clear();
		realtimeSubscribe.mockClear();
		connectGlobal.mockClear();
		realtimeState.connection = 'OPEN';
		scopeIdentityStore.reset();
	});

	it('normalizes bounded section and digest query values before fetching', async () => {
		const store = createTodayStore({
			per_section: 999,
			section: 'followups',
			limit: 999,
			cursor: 'cursor-2',
			digest_limit: -4,
			digest_offset: 5_000
		});
		todayFetch.mockImplementation((input: RequestInfo | URL) => {
			const url = String(input);
			if (url.includes('/today/visibility')) return Promise.resolve(jsonResponse({ items: [] }));
			return Promise.resolve(jsonResponse(payload()));
		});

		await store.refresh();

		expect(get(store).query).toEqual({
			per_section: 20,
			section: 'followups',
			limit: 50,
			cursor: 'cursor-2',
			digest_limit: 1,
			digest_offset: 1_000
		});
		expect(String(todayFetch.mock.calls[0][0])).toContain(
			'per_section=20&section=followups&limit=50&cursor=cursor-2&digest_limit=1&digest_offset=1000'
		);
	});

	it('normalizes malformed item collections and digest fields at the boundary', async () => {
		const store = createTodayStore();
		const malformed = item('normalized') as unknown as Record<string, unknown>;
		malformed.space_ids = [' space-a ', 7, '', 'space-b'];
		malformed.actions = 'not-an-array';
		malformed.evidence_refs = null;
		malformed.metadata = undefined;
		const body = payload(
			{ needs_you: [malformed as unknown as TodayItem] },
			{
				digest: {
					generated_at: Number.NaN,
					since: Number.NaN,
					total: Number.NaN,
					limit: Number.NaN,
					offset: -5,
					bullets: [
						{
							id: '',
							text: 'Changed item',
							source_kind: 'task',
							source_id: 'task-1',
							source_url: null,
							space_ids: [' team ', ''],
							updated_at: Number.NaN
						}
					]
				}
			}
		);
		todayFetch.mockResolvedValueOnce(jsonResponse(body));

		await store.refresh();

		expect(get(store).sections.needs_you[0]).toMatchObject({
			space_ids: ['space-a', 'space-b'],
			actions: [],
			evidence_refs: [],
			metadata: null
		});
		expect(get(store).digest).toMatchObject({
			generated_at: 100,
			since: null,
			total: 1,
			limit: 7,
			offset: 0,
			bullets: [
				expect.objectContaining({
					id: 'today:digest:0',
					space_ids: ['team'],
					updated_at: 100
				})
			]
		});
	});

	it('preserves the last good page and extracts JSON backend errors', async () => {
		const store = createTodayStore();
		todayFetch.mockResolvedValueOnce(
			jsonResponse(payload({ needs_you: [item('kept')] }))
		);
		await store.refresh();
		todayFetch.mockResolvedValueOnce(
			new Response(JSON.stringify({ message: 'Today index is rebuilding' }), { status: 503 })
		);

		await store.refresh();

		expect(get(store).sections.needs_you.map(({ id }) => id)).toEqual(['kept']);
		expect(get(store).error).toBe('Today index is rebuilding');
		store.clearError();
		expect(get(store).error).toBeNull();
	});

	it('ignores a response whose query was replaced while it was loading', async () => {
		const store = createTodayStore({ section: 'needs_you', limit: 8 });
		const oldResponse = deferred<Response>();
		todayFetch.mockReturnValueOnce(oldResponse.promise);
		const loading = store.refresh();
		store.setQuery({ section: 'delivered', limit: 5 });
		oldResponse.resolve(
			jsonResponse(payload({ needs_you: [item('stale')] }))
		);
		await loading;

		expect(get(store).query).toMatchObject({ section: 'delivered', limit: 5 });
		expect(get(store).sections.needs_you).toEqual([]);
		expect(get(store).lastLoadedAt).toBeNull();
	});

	it('coalesces overlapping refreshes and retains a queued digest refresh', async () => {
		const store = createTodayStore();
		const firstResponse = deferred<Response>();
		todayFetch.mockReturnValueOnce(firstResponse.promise);
		todayFetch.mockResolvedValueOnce(jsonResponse(payload()));

		const first = store.refresh();
		const second = store.refreshDigest();
		expect(todayFetch).toHaveBeenCalledTimes(1);
		firstResponse.resolve(jsonResponse(payload()));
		await Promise.all([first, second]);
		await vi.waitFor(() => expect(todayFetch).toHaveBeenCalledTimes(3));

		const todayUrls = todayFetch.mock.calls
			.map(([input]) => String(input))
			.filter((url) => !url.includes('/today/visibility'));
		expect(todayUrls).toHaveLength(2);
		expect(todayUrls[1]).toContain('digest_refresh=true');
	});

	it('debounces matching realtime events and ignores another scope', async () => {
		vi.useFakeTimers();
		const store = createTodayStore();
		todayFetch.mockImplementation((input: RequestInfo | URL) => {
			const url = String(input);
			if (url.includes('/today/visibility')) return Promise.resolve(jsonResponse({ items: [] }));
			return Promise.resolve(jsonResponse(payload()));
		});
		store.start();
		await vi.runAllTicks();
		await Promise.resolve();
		const initialTodayCalls = todayFetch.mock.calls.filter(([input]) =>
			String(input).includes('/v2/today?')
		).length;

		emitRealtime({
			sequence: 1,
			event_type: 'FeedItemUpdated',
			data: { principal: 'other', workspace: 'default', item_id: 'ignored' }
		});
		await vi.advanceTimersByTimeAsync(750);
		expect(
			todayFetch.mock.calls.filter(([input]) => String(input).includes('/v2/today?')).length
		).toBe(initialTodayCalls);

		emitRealtime(
			{
				sequence: 2,
				event_type: 'FeedItemUpdated',
				data: { principal: 'anonymous', workspace: 'default', item_id: 'one' }
			},
			{
				sequence: 3,
				event_type: 'HitlResolved',
				data: { correlation_id: 'two' }
			}
		);
		await vi.advanceTimersByTimeAsync(749);
		expect(
			todayFetch.mock.calls.filter(([input]) => String(input).includes('/v2/today?')).length
		).toBe(initialTodayCalls);
		await vi.advanceTimersByTimeAsync(1);
		await vi.runAllTicks();
		expect(
			todayFetch.mock.calls.filter(([input]) => String(input).includes('/v2/today?')).length
		).toBe(initialTodayCalls + 1);
		store.stop();
	});

	it('resets and refetches active state when scope changes', async () => {
		const store = createTodayStore();
		let currentPayload = payload({ needs_you: [item('anonymous-item')] });
		todayFetch.mockImplementation((input: RequestInfo | URL) => {
			const url = String(input);
			if (url.includes('/today/visibility')) return Promise.resolve(jsonResponse({ items: [] }));
			return Promise.resolve(jsonResponse(currentPayload));
		});
		store.start();
		await vi.waitFor(() => expect(get(store).sections.needs_you[0]?.id).toBe('anonymous-item'));

		currentPayload = payload(
			{ needs_you: [item('owner-item')] },
			{ principal: 'owner', workspace: 'project' }
		);
		scopeIdentityStore.observe('owner', 'project');
		await vi.waitFor(() => expect(get(store).sections.needs_you[0]?.id).toBe('owner-item'));
		expect(todayFetch.mock.calls.every(([input]) => !String(input).includes('principal='))).toBe(true);
		store.stop();
	});

	it('reference-counts consumers and connects a closed realtime transport once', async () => {
		const store = createTodayStore();
		realtimeState.connection = 'CLOSED';
		todayFetch.mockImplementation((input: RequestInfo | URL) => {
			const url = String(input);
			if (url.includes('/today/visibility')) return Promise.resolve(jsonResponse({ items: [] }));
			return Promise.resolve(jsonResponse(payload()));
		});

		store.start();
		store.start();
		await vi.waitFor(() => expect(realtimeSubscribe).toHaveBeenCalledTimes(1));
		expect(connectGlobal).toHaveBeenCalledTimes(1);
		store.stop();
		expect(realtimeSubscribers.size).toBe(1);
		store.stop();
		expect(realtimeSubscribers.size).toBe(0);
	});

	// Pinned in a positive-offset zone at an hour where the reader's calendar
	// date and the UTC date are different days. Every `/today` predicate is a
	// date comparison, so a request carrying the server's date is a wrong
	// answer that looks entirely right — and only for part of every day.
	describe('the date the request carries', () => {
		const ambientTimeZone = process.env.TZ;

		beforeEach(() => {
			process.env.TZ = 'Asia/Kolkata';
			vi.useFakeTimers();
			// 01:30 on the 31st in IST. In UTC it is still the 30th.
			vi.setSystemTime(new Date('2026-07-30T20:00:00.000Z'));
		});

		afterEach(() => {
			vi.useRealTimers();
			if (ambientTimeZone === undefined) delete process.env.TZ;
			else process.env.TZ = ambientTimeZone;
		});

		it('sends the local date the reader is on, never its UTC rendering', async () => {
			const store = createTodayStore();
			todayFetch.mockImplementation((input: RequestInfo | URL) => {
				const url = String(input);
				if (url.includes('/today/visibility')) return Promise.resolve(jsonResponse({ items: [] }));
				return Promise.resolve(jsonResponse(payload()));
			});

			await store.refresh();

			const requested = new URL(String(todayFetch.mock.calls[0][0]), 'http://localhost');
			expect(requested.searchParams.get('today')).toBe('2026-07-31');
			// The value is what matters, not the parameter's presence. At this
			// instant every UTC-flavoured derivation — `toISOString()` on now,
			// or the original bug of `setHours(0,0,0,0)` then `toISOString()`,
			// which renders local midnight in UTC — names the 30th. An
			// assertion that only checked `today` was set would pass against
			// the exact defect this closes.
			expect(new Date().toISOString().slice(0, 10)).toBe('2026-07-30');
			expect(requested.searchParams.get('today')).not.toBe(
				new Date().toISOString().slice(0, 10)
			);
		});

		it('agrees with the probe that mints the cursors it seeks with', async () => {
			// Two requests, one render, one day. The page walks a section forward
			// with the probe and hands the resulting cursor to this store; a
			// Follow-ups cursor's leading band is derived from the date, so if the
			// two disagree the seek lands in a projection the cursor was never cut
			// from and rows are skipped or repeated. That disagreement is what made
			// the probe's missing `today=` worse than a missing parameter.
			const store = createTodayStore();
			todayFetch.mockImplementation((input: RequestInfo | URL) => {
				const url = String(input);
				if (url.includes('/today/visibility')) return Promise.resolve(jsonResponse({ items: [] }));
				return Promise.resolve(jsonResponse(payload()));
			});

			await store.refresh();

			// Read back off the wire rather than recomputed: that the two callers
			// land on one value is the assertion, and recomputing it here would
			// assume it.
			const requested = new URL(String(todayFetch.mock.calls[0][0]), 'http://localhost');
			const probed = new URL(
				todaySectionCursorProbeUrl({
					sectionId: 'followups',
					cursor: null,
					sectionPageSize: 8,
					digestPageSize: 7
				}),
				'http://localhost'
			);
			expect(probed.searchParams.get('today')).toBe(requested.searchParams.get('today'));
			expect(probed.searchParams.get('today')).toBe('2026-07-31');
			store.stop();
		});
	});
});
