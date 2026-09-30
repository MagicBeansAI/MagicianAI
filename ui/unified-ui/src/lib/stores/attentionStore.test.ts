import { get } from 'svelte/store';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { FeedAttentionResponse, FeedItem } from '$lib/feed/types';

const { attentionFetch, realtimeSubscribers, realtimeSubscribe, connectGlobal } = vi.hoisted(() => {
	const subscribers = new Set<(events: Array<Record<string, unknown>>) => void>();
	return {
		attentionFetch: vi.fn(),
		realtimeSubscribers: subscribers,
		realtimeSubscribe: vi.fn(
			(subscriber: (events: Array<Record<string, unknown>>) => void) => {
				subscribers.add(subscriber);
				return () => subscribers.delete(subscriber);
			}
		),
		connectGlobal: vi.fn()
	};
});

vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$lib/shared/fetch', () => ({
	timedFetch: (input: RequestInfo | URL, init?: RequestInit) =>
		String(input) === '/api/magician/v2/bots/auth'
			? Promise.resolve(new Response('{}', { status: 200 }))
			: attentionFetch(input, init)
}));
vi.mock('$lib/realtime/v2-websocket', () => ({
	getV2EventSequence: (event: { sequence?: number }) => event.sequence ?? 0,
	v2Events: {
		subscribe: realtimeSubscribe,
		getConnectionState: vi.fn(() => 'OPEN'),
		connectGlobal
	}
}));

import { createAttentionStore } from './attentionStore';
import { scopeIdentityStore } from './scopeIdentityStore';

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((next) => {
		resolve = next;
	});
	return { promise, resolve };
}

function item(id: string, overrides: Partial<FeedItem> = {}): FeedItem {
	return {
		id,
		principal: 'anonymous',
		workspace: 'default',
		item_type: 'task',
		title: id,
		status: 'needs_action',
		created_at: 1,
		updated_at: 1,
		actions: [],
		metadata: {},
		...overrides
	};
}

function emitRealtime(...events: Array<Record<string, unknown>>): void {
	for (const subscriber of realtimeSubscribers) subscriber(events);
}

function payload(
	requests: FeedItem[],
	nextCursor: string | null,
	cursor: string | null = null,
	total = requests.length,
	limit = 25
): FeedAttentionResponse {
	const emptyPage = { total: 0, limit, cursor: null, next_cursor: null, has_more: false };
	return {
		counts: {
			requests: total,
			approvals: 0,
			escalations: 0,
			needs_action: total,
			failed: 0,
			running: 0
		},
		totals: { requests: total, approvals: 0, escalations: 0, failed: 0, running: 0 },
		pages: {
			requests: {
				total,
				limit,
				cursor,
				next_cursor: nextCursor,
				has_more: nextCursor !== null
			},
			approvals: { ...emptyPage },
			escalations: { ...emptyPage },
			failed: { ...emptyPage },
			running: { ...emptyPage }
		},
		requests,
		approvals: [],
		escalations: [],
		failed: [],
		running: []
	};
}

function response(body: FeedAttentionResponse): Response {
	return new Response(JSON.stringify(body), {
		status: 200,
		headers: { 'Content-Type': 'application/json' }
	});
}

describe('attentionStore refresh/loadMore serialization', () => {
	beforeEach(() => {
		attentionFetch.mockReset();
		realtimeSubscribers.clear();
		realtimeSubscribe.mockClear();
		connectGlobal.mockClear();
		scopeIdentityStore.reset();
		const values = new Map<string, string>();
		vi.stubGlobal('localStorage', {
			getItem: vi.fn((key: string) => values.get(key) ?? null),
			setItem: vi.fn((key: string, value: string) => values.set(key, value)),
			removeItem: vi.fn((key: string) => values.delete(key)),
			clear: vi.fn(() => values.clear())
		});
	});

	it('publishes all server lanes, totals, pages, and observed scope', async () => {
		const store = createAttentionStore();
		const body = payload([item('request')], null);
		body.approvals = [item('approval', { item_type: 'approval' })];
		body.escalations = [item('escalation', { item_type: 'escalation' })];
		body.failed = [item('failed', { status: 'failed' })];
		body.running = [item('running', { status: 'running' })];
		body.counts = {
			requests: 1,
			approvals: 1,
			escalations: 1,
			needs_action: 3,
			failed: 1,
			running: 1
		};
		body.totals = { requests: 7, approvals: 5, escalations: 3, failed: 2, running: 1 };
		body.pages!.approvals = {
			total: 5,
			limit: 25,
			cursor: null,
			next_cursor: 'approval-next',
			has_more: true
		};
		attentionFetch.mockResolvedValueOnce(response(body));

		await store.refresh();

		const state = get(store);
		expect(state.requests.map(({ id }) => id)).toEqual(['request']);
		expect(state.approvals.map(({ id }) => id)).toEqual(['approval']);
		expect(state.escalations.map(({ id }) => id)).toEqual(['escalation']);
		expect(state.failed.map(({ id }) => id)).toEqual(['failed']);
		expect(state.running.map(({ id }) => id)).toEqual(['running']);
		expect(state.counts.needs_action).toBe(3);
		expect(state.totals.approvals).toBe(5);
		expect(state.pages.approvals.next_cursor).toBe('approval-next');
		expect(state.lastLoadedAt).toEqual(expect.any(Number));
		expect(String(attentionFetch.mock.calls[0][0])).toContain('limit=25');
		expect(String(attentionFetch.mock.calls[0][0])).not.toContain('principal=');
	});

	it('fetches one exact scoped item with a URL-encoded alias', async () => {
		const store = createAttentionStore();
		scopeIdentityStore.observe('owner/team', 'project space');
		const exact = item('v3:attention:outside-page', {
			principal: 'owner/team',
			workspace: 'project space',
			metadata: { pause_state_id: 'pause/id with spaces' }
		});
		attentionFetch.mockResolvedValueOnce(
			new Response(JSON.stringify(exact), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			})
		);

		await expect(store.fetchItem('pause/id with spaces')).resolves.toEqual(exact);
		expect(String(attentionFetch.mock.calls[0][0])).toBe(
			'/api/magician/v2/feed/attention/pause%2Fid%20with%20spaces'
		);
	});

	it('returns null when an exact attention item is not found', async () => {
		const store = createAttentionStore();
		attentionFetch.mockResolvedValueOnce(
			new Response(JSON.stringify({ error: 'attention_item_not_found' }), { status: 404 })
		);

		await expect(store.fetchItem('missing')).resolves.toBeNull();
	});

	it('preserves the last good rows when a refresh fails', async () => {
		const store = createAttentionStore();
		attentionFetch.mockResolvedValueOnce(response(payload([item('kept')], null)));
		await store.refresh();
		attentionFetch.mockResolvedValueOnce(new Response('attention backend unavailable', { status: 503 }));

		await store.refresh();

		expect(get(store)).toMatchObject({
			isLoading: false,
			error: 'attention backend unavailable',
			requests: [expect.objectContaining({ id: 'kept' })]
		});
	});

	it('makes loadMore use cursors from an in-flight refresh', async () => {
		const store = createAttentionStore();
		attentionFetch.mockResolvedValueOnce(response(payload([item('first')], 'cursor-1')));
		await store.refresh();

		const refreshResponse = deferred<Response>();
		attentionFetch.mockReturnValueOnce(refreshResponse.promise);
		attentionFetch.mockResolvedValueOnce(
			response(payload([item('second')], null, 'cursor-2', 2))
		);

		const refreshing = store.refresh();
		await vi.waitFor(() => expect(attentionFetch).toHaveBeenCalledTimes(2));
		const loadingMore = store.loadMore();
		expect(attentionFetch).toHaveBeenCalledTimes(2);

		refreshResponse.resolve(response(payload([item('first-refreshed')], 'cursor-2', null, 2)));
		await vi.waitFor(() => expect(attentionFetch).toHaveBeenCalledTimes(3));

		expect(String(attentionFetch.mock.calls[2][0])).toContain('requests_cursor=cursor-2');
		await Promise.all([refreshing, loadingMore]);
		expect(get(store).requests.map((entry) => entry.id)).toEqual([
			'first-refreshed',
			'second'
		]);
	});

	it('makes refresh wait for append and refetch the expanded window', async () => {
		const store = createAttentionStore();
		attentionFetch.mockResolvedValueOnce(response(payload([item('first')], 'cursor-1', null, 2)));
		await store.refresh();

		const appendResponse = deferred<Response>();
		attentionFetch.mockReturnValueOnce(appendResponse.promise);
		attentionFetch.mockResolvedValueOnce(
			response(payload([item('first'), item('second')], null, null, 2, 50))
		);

		const loadingMore = store.loadMore();
		await vi.waitFor(() => expect(attentionFetch).toHaveBeenCalledTimes(2));
		const refreshing = store.refresh();
		expect(attentionFetch).toHaveBeenCalledTimes(2);

		appendResponse.resolve(response(payload([item('second')], null, 'cursor-1', 2)));
		await vi.waitFor(() => expect(attentionFetch).toHaveBeenCalledTimes(3));

		const refreshUrl = String(attentionFetch.mock.calls[2][0]);
		expect(refreshUrl).toContain('limit=50');
		expect(refreshUrl).not.toContain('_cursor=');
		await Promise.all([loadingMore, refreshing]);
		expect(get(store).requests.map((entry) => entry.id)).toEqual(['first', 'second']);
	});

	it('appends and deduplicates only lanes whose cursor was requested', async () => {
		const store = createAttentionStore();
		const first = payload([item('request-1')], 'request-next', null, 2);
		first.approvals = [item('approval-1', { item_type: 'approval' })];
		first.counts.approvals = 1;
		first.totals!.approvals = 1;
		attentionFetch.mockResolvedValueOnce(response(first));
		await store.refresh();

		const next = payload([item('request-1'), item('request-2')], null, 'request-next', 2);
		next.approvals = [item('approval-should-not-append', { item_type: 'approval' })];
		next.counts.approvals = 1;
		next.totals!.approvals = 1;
		attentionFetch.mockResolvedValueOnce(response(next));
		await store.loadMore();

		expect(get(store).requests.map(({ id }) => id)).toEqual(['request-1', 'request-2']);
		expect(get(store).approvals.map(({ id }) => id)).toEqual(['approval-1']);
	});

	it('drops resolved rows by metadata alias and adjusts actionable counts', async () => {
		const store = createAttentionStore();
		const body = payload(
			[item('request', { metadata: { request_id: 'shared-correlation' } })],
			null
		);
		body.approvals = [
			item('approval', {
				item_type: 'approval',
				metadata: { correlation_id: 'shared-correlation' }
			})
		];
		body.counts.approvals = 1;
		body.counts.needs_action = 2;
		body.totals!.approvals = 1;
		body.pages!.approvals.total = 1;
		attentionFetch.mockResolvedValueOnce(response(body));
		await store.refresh();

		store.dropResolved('shared-correlation');

		expect(get(store)).toMatchObject({
			requests: [],
			approvals: [],
			counts: { requests: 0, approvals: 0, needs_action: 0 },
			totals: { requests: 0, approvals: 0 },
			pages: { requests: { total: 0 }, approvals: { total: 0 } }
		});
	});

	it('durably dismisses failed rows by alias while posting the raw feed id', async () => {
		const store = createAttentionStore();
		const failed = item('raw-failed-id', {
			status: 'failed',
			metadata: { correlation_id: 'failure-correlation' }
		});
		const body = payload([], null);
		body.failed = [failed];
		body.counts.failed = 1;
		body.totals!.failed = 1;
		attentionFetch.mockResolvedValueOnce(response(body));
		await store.refresh();
		attentionFetch.mockResolvedValueOnce(new Response('{}', { status: 200 }));

		store.dismissFailed('failure-correlation', 'raw-failed-id');

		expect(get(store).failed).toEqual([]);
		expect(get(store).counts.failed).toBe(0);
		// Tab counts, the pager denominator, and the header all read `totals`;
		// a dismissal that only moved `counts` made them claim rows the list no
		// longer rendered (badge 2 vs tab 7).
		expect(get(store).totals.failed).toBe(0);
		expect(get(store).pages.failed.total).toBe(0);
		await vi.waitFor(() => expect(attentionFetch).toHaveBeenCalledTimes(2));
		expect(String(attentionFetch.mock.calls[1][0])).toContain('/feed/attention/dismiss');
		expect(String(attentionFetch.mock.calls[1][0])).not.toContain('?');
		expect(attentionFetch.mock.calls[1][1]).toMatchObject({
			method: 'POST',
			body: JSON.stringify({ item_id: 'raw-failed-id', dismissed: true })
		});
		expect(localStorage.setItem).toHaveBeenCalledWith(
			'attention:dismissed-failed',
			JSON.stringify(['failure-correlation'])
		);

		const reloaded = createAttentionStore();
		attentionFetch.mockResolvedValueOnce(response(body));
		await reloaded.refresh();
		expect(get(reloaded).failed).toEqual([]);
	});

	it('re-posts the dismissal when the server still returns a locally dismissed item', async () => {
		// The server filters dismissed rows from every response, so an item it
		// still returns is one whose dismissal POST never landed (fire-and-forget).
		// The store re-posts on every payload that still carries the row, which
		// converges both sides without a reconciliation endpoint.
		const store = createAttentionStore();
		const failed = item('raw-failed-id', { status: 'failed' });
		const body = payload([], null);
		body.failed = [failed];
		body.counts.failed = 1;
		body.totals!.failed = 1;
		body.pages!.failed.total = 1;
		attentionFetch.mockResolvedValueOnce(response(body));
		await store.refresh();
		expect(get(store).totals.failed).toBe(1);

		// Seed the local dismissal as if a previous session recorded it and its
		// server POST failed.
		const values = new Map<string, string>([
			['attention:dismissed-failed', JSON.stringify(['raw-failed-id'])]
		]);
		vi.stubGlobal('localStorage', {
			getItem: vi.fn((key: string) => values.get(key) ?? null),
			setItem: vi.fn((key: string, value: string) => values.set(key, value)),
			removeItem: vi.fn((key: string) => values.delete(key)),
			clear: vi.fn(() => values.clear())
		});
		const reloaded = createAttentionStore();
		attentionFetch.mockResolvedValueOnce(response(body));
		attentionFetch.mockResolvedValueOnce(new Response('{}', { status: 200 }));
		attentionFetch.mockResolvedValueOnce(new Response('{}', { status: 200 }));
		await reloaded.refresh();

		const state = get(reloaded);
		expect(state.failed).toEqual([]);
		expect(state.counts.failed).toBe(0);
		expect(state.totals.failed).toBe(0);
		await vi.waitFor(() => expect(attentionFetch).toHaveBeenCalledTimes(3));
		const dismissCall = attentionFetch.mock.calls.find(([url]) =>
			String(url).includes('/feed/attention/dismiss')
		);
		expect(dismissCall).toBeDefined();
		expect(dismissCall![1]).toMatchObject({
			method: 'POST',
			body: JSON.stringify({ item_id: 'raw-failed-id', dismissed: true })
		});
	});

	it('removes a resolved realtime item immediately and ignores replayed sequences', async () => {
		const store = createAttentionStore();
		const body = payload(
			[item('request', { metadata: { pause_state_id: 'pause-1' } })],
			null
		);
		attentionFetch.mockResolvedValueOnce(response(body));
		store.start();
		await vi.waitFor(() => expect(get(store).requests).toHaveLength(1));
		expect(realtimeSubscribe).toHaveBeenCalledTimes(1);

		emitRealtime({
			sequence: 7,
			event_type: 'HitlResolved',
			data: { correlation_id: 'pause-1' }
		});
		expect(get(store).requests).toEqual([]);
		expect(get(store.resolutions)).toEqual({
			revision: 1,
			notices: [{ revision: 1, correlationId: 'pause-1' }]
		});

		store.dropResolved('missing');
		expect(get(store.resolutions)).toMatchObject({
			revision: 2,
			notices: [
				{ revision: 1, correlationId: 'pause-1' },
				{ revision: 2, correlationId: 'missing' }
			]
		});
		emitRealtime({
			sequence: 7,
			event_type: 'HitlResolved',
			data: { correlation_id: 'missing' }
		});
		expect(get(store).counts.needs_action).toBe(0);
		expect(get(store.resolutions).revision).toBe(2);
		store.stop();
		expect(realtimeSubscribers.size).toBe(0);
	});

	it('resets and refetches when the active scope changes', async () => {
		const store = createAttentionStore();
		attentionFetch.mockResolvedValueOnce(response(payload([item('anonymous-item')], null)));
		store.start();
		await vi.waitFor(() => expect(get(store).requests[0]?.id).toBe('anonymous-item'));

		attentionFetch.mockResolvedValueOnce(
			response(
				payload(
					[item('owner-item', { principal: 'owner', workspace: 'project' })],
					null
				)
			)
		);
		scopeIdentityStore.observe('owner', 'project');
		await vi.waitFor(() => expect(get(store).requests[0]?.id).toBe('owner-item'));

		expect(String(attentionFetch.mock.calls[1][0])).not.toContain('principal=');
		store.stop();
	});
});
