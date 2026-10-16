import { get } from 'svelte/store';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { FeedCounts, FeedItem } from '$lib/feed/types';

const {
	feedFetch,
	mutationFetch,
	realtimeSubscribers,
	realtimeSubscribe,
	connectGlobal,
	realtimeState
} = vi.hoisted(() => {
	const subscribers = new Set<(events: Array<Record<string, unknown>>) => void>();
	return {
		feedFetch: vi.fn(),
		mutationFetch: vi.fn(),
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
vi.mock('$lib/shared/fetch', () => ({ timedFetch: mutationFetch }));
vi.mock('$lib/realtime/v2-websocket', () => ({
	getV2EventSequence: (event: { sequence?: number }) => event.sequence ?? 0,
	v2Events: {
		subscribe: realtimeSubscribe,
		getConnectionState: () => realtimeState.connection,
		connectGlobal
	}
}));

import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { createFeedStore } from './feedStore';

function item(id: string, overrides: Partial<FeedItem> = {}): FeedItem {
	return {
		id,
		principal: 'anonymous',
		workspace: 'default',
		item_type: 'task',
		task_id: `task-${id}`,
		ui_thread_id: 'general',
		agent_id: 'presto',
		title: id,
		summary: null,
		status: 'info',
		created_at: 1,
		updated_at: 1,
		actions: [],
		metadata: {},
		...overrides
	};
}

function counts(items: FeedItem[]): FeedCounts {
	const result: FeedCounts = {
		total: items.length,
		running: 0,
		needs_action: 0,
		failed: 0,
		done: 0,
		info: 0
	};
	for (const entry of items) result[entry.status] += 1;
	return result;
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

describe('feedStore', () => {
	let backendItems: FeedItem[];
	let backendCounts: FeedCounts;

	beforeEach(() => {
		vi.useRealTimers();
		backendItems = [];
		backendCounts = counts(backendItems);
		feedFetch.mockReset();
		mutationFetch.mockReset();
		realtimeSubscribers.clear();
		realtimeSubscribe.mockClear();
		connectGlobal.mockClear();
		realtimeState.connection = 'OPEN';
		scopeIdentityStore.reset();
		feedFetch.mockImplementation((input: RequestInfo | URL) => {
			const url = String(input);
			if (url.includes('/feed/counts')) return Promise.resolve(jsonResponse({ counts: backendCounts }));
			if (url.includes('/feed')) return Promise.resolve(jsonResponse({ items: backendItems }));
			return Promise.reject(new Error(`unexpected feed fetch: ${url}`));
		});
		mutationFetch.mockResolvedValue(jsonResponse({}));
		vi.stubGlobal('fetch', feedFetch);
	});

	it('loads scoped items and counts with a trimmed thread filter', async () => {
		scopeIdentityStore.observe('owner', 'project');
		backendItems = [
			item('newer', {
				principal: 'owner',
				workspace: 'project',
				ui_thread_id: 'team',
				status: 'running',
				updated_at: 5
			})
		];
		backendCounts = counts(backendItems);
		const store = createFeedStore({ ui_thread_id: ' team ', limit: 10 });

		await store.refresh();

		expect(get(store)).toMatchObject({
			isLoading: false,
			error: null,
			items: [expect.objectContaining({ id: 'newer' })],
			counts: { total: 1, running: 1 },
			query: { ui_thread_id: ' team ', limit: 10 }
		});
		for (const [input] of feedFetch.mock.calls) {
			expect(String(input)).toContain('ui_thread_id=team&limit=10');
			expect(String(input)).not.toContain('principal=');
		}
	});

	it.each([
		['items', '/feed/counts', '/feed?', 'Feed rows are rebuilding'],
		['counts', '/feed?', '/feed/counts', 'Feed counts are rebuilding']
	])('preserves rows and extracts JSON errors when %s loading fails', async (_kind, good, bad, message) => {
		backendItems = [item('kept')];
		backendCounts = counts(backendItems);
		const store = createFeedStore();
		await store.refresh();
		feedFetch.mockImplementation((input: RequestInfo | URL) => {
			const url = String(input);
			if (url.includes(bad)) {
				return Promise.resolve(jsonResponse({ message }, 503));
			}
			if (url.includes(good)) {
				return Promise.resolve(
					url.includes('/counts')
						? jsonResponse({ counts: backendCounts })
						: jsonResponse({ items: backendItems })
				);
			}
			return Promise.reject(new Error(`unexpected feed fetch: ${url}`));
		});

		await store.refresh();

		expect(get(store).items.map(({ id }) => id)).toEqual(['kept']);
		expect(get(store).error).toBe(message);
		store.clearError();
		expect(get(store).error).toBeNull();
	});

	it('ignores stale query responses and runs the queued replacement query', async () => {
		const oldItems = deferred<Response>();
		const oldCounts = deferred<Response>();
		feedFetch.mockImplementationOnce(() => oldItems.promise);
		feedFetch.mockImplementationOnce(() => oldCounts.promise);
		backendItems = [item('thread-b', { ui_thread_id: 'thread-b' })];
		backendCounts = counts(backendItems);
		const store = createFeedStore({ ui_thread_id: 'thread-a' });
		store.start();
		await vi.waitFor(() => expect(feedFetch).toHaveBeenCalledTimes(2));

		store.setQuery({ ui_thread_id: 'thread-b', limit: 5 });
		oldItems.resolve(jsonResponse({ items: [item('stale', { ui_thread_id: 'thread-a' })] }));
		oldCounts.resolve(jsonResponse({ counts: counts([item('stale')]) }));
		await vi.waitFor(() => expect(get(store).items[0]?.id).toBe('thread-b'));

		expect(get(store).query).toEqual({ ui_thread_id: 'thread-b', limit: 5 });
		expect(
			feedFetch.mock.calls.slice(2).every(([input]) =>
				String(input).includes('ui_thread_id=thread-b&limit=5')
			)
		).toBe(true);
		store.stop();
	});

	it('applies created, status-updated, and removed realtime rows with stable counts', async () => {
		backendItems = [item('existing', { updated_at: 2 })];
		backendCounts = counts(backendItems);
		const store = createFeedStore({ limit: 10 });
		store.start();
		await vi.waitFor(() => expect(get(store).items).toHaveLength(1));

		emitRealtime({
			sequence: 1,
			event_type: 'FeedItemCreated',
			data: { item: item('live', { status: 'running', created_at: 3, updated_at: 3 }) }
		});
		expect(get(store).items.map(({ id }) => id)).toEqual(['live', 'existing']);
		expect(get(store).counts).toMatchObject({ total: 2, running: 1, info: 1 });

		emitRealtime({
			sequence: 2,
			event_type: 'FeedItemUpdated',
			data: {
				principal: 'anonymous',
				workspace: 'default',
				id: 'live',
				patch: { status: 'done', title: 'Finished live item', updated_at: 4 }
			}
		});
		expect(get(store).items[0]).toMatchObject({
			id: 'live',
			status: 'done',
			title: 'Finished live item'
		});
		expect(get(store).counts).toMatchObject({ total: 2, running: 0, done: 1, info: 1 });

		emitRealtime({
			sequence: 3,
			event_type: 'FeedItemRemoved',
			data: { principal: 'anonymous', workspace: 'default', id: 'live' }
		});
		expect(get(store).items.map(({ id }) => id)).toEqual(['existing']);
		expect(get(store).counts).toMatchObject({ total: 1, done: 0, info: 1 });
		store.stop();
	});

	it('honors thread filters for realtime creates and updates that leave the query', async () => {
		backendItems = [item('thread-a', { ui_thread_id: 'thread-a' })];
		backendCounts = counts(backendItems);
		const store = createFeedStore({ ui_thread_id: 'thread-a', limit: 10 });
		store.start();
		await vi.waitFor(() => expect(get(store).items).toHaveLength(1));

		emitRealtime({
			sequence: 1,
			event_type: 'FeedItemCreated',
			data: { item: item('other', { ui_thread_id: 'thread-b', status: 'running' }) }
		});
		expect(get(store).items.map(({ id }) => id)).toEqual(['thread-a']);
		expect(get(store).counts).toEqual(counts(backendItems));

		emitRealtime({
			sequence: 2,
			event_type: 'FeedItemUpdated',
			data: {
				principal: 'anonymous',
				workspace: 'default',
				id: 'thread-a',
				ui_thread_id: 'thread-b',
				patch: { ui_thread_id: 'thread-b' }
			}
		});
		expect(get(store).items).toEqual([]);
		expect(get(store).counts.total).toBe(0);
		store.stop();
	});

	it('debounces unknown matching updates into one authoritative refresh', async () => {
		vi.useFakeTimers();
		const store = createFeedStore({ ui_thread_id: 'thread-a' });
		store.start();
		await vi.runAllTicks();
		await Promise.resolve();
		const initialCalls = feedFetch.mock.calls.length;

		emitRealtime(
			{
				sequence: 1,
				event_type: 'FeedItemUpdated',
				data: {
					principal: 'anonymous',
					workspace: 'default',
					id: 'unknown-a',
					ui_thread_id: 'thread-a',
					patch: { title: 'one' }
				}
			},
			{
				sequence: 2,
				event_type: 'FeedItemRemoved',
				data: {
					principal: 'anonymous',
					workspace: 'default',
					id: 'unknown-b',
					ui_thread_id: 'thread-a'
				}
			}
		);
		await vi.advanceTimersByTimeAsync(599);
		expect(feedFetch).toHaveBeenCalledTimes(initialCalls);
		await vi.advanceTimersByTimeAsync(1);
		await vi.runAllTicks();
		expect(feedFetch).toHaveBeenCalledTimes(initialCalls + 2);
		store.stop();
	});

	it('ignores another scope and resets/refetches when the active scope changes', async () => {
		backendItems = [item('anonymous-item')];
		backendCounts = counts(backendItems);
		const store = createFeedStore();
		store.start();
		await vi.waitFor(() => expect(get(store).items[0]?.id).toBe('anonymous-item'));

		emitRealtime({
			sequence: 1,
			event_type: 'FeedItemCreated',
			data: {
				item: item('ignored', { principal: 'other', workspace: 'project' })
			}
		});
		expect(get(store).items.map(({ id }) => id)).toEqual(['anonymous-item']);

		backendItems = [item('owner-item', { principal: 'owner', workspace: 'project' })];
		backendCounts = counts(backendItems);
		scopeIdentityStore.observe('owner', 'project');
		await vi.waitFor(() => expect(get(store).items[0]?.id).toBe('owner-item'));
		expect(feedFetch.mock.calls.every(([input]) => !String(input).includes('principal='))).toBe(true);
		store.stop();
	});

	it('reference-counts consumers and connects a closed transport once', async () => {
		const store = createFeedStore();
		realtimeState.connection = 'CLOSED';
		store.start();
		store.start();
		await vi.waitFor(() => expect(realtimeSubscribe).toHaveBeenCalledTimes(1));
		expect(connectGlobal).toHaveBeenCalledTimes(1);
		store.stop();
		expect(realtimeSubscribers.size).toBe(1);
		store.stop();
		expect(realtimeSubscribers.size).toBe(0);
	});

	it('deletes one item optimistically with scoped query parameters', async () => {
		backendItems = [item('delete me', { status: 'failed', ui_thread_id: 'thread-a' })];
		backendCounts = counts(backendItems);
		const store = createFeedStore({ ui_thread_id: 'thread-a', limit: 5 });
		await store.refresh();
		mutationFetch.mockResolvedValueOnce(jsonResponse({}));

		expect(await store.deleteItem(' delete me ')).toBe(true);

		expect(get(store).items).toEqual([]);
		expect(get(store).counts).toMatchObject({ total: 0, failed: 0 });
		expect(String(mutationFetch.mock.calls[0][0])).toContain(
			'/feed/items/delete%20me?ui_thread_id=thread-a&limit=5'
		);
		expect(await store.deleteItem('   ')).toBe(false);
	});

	it('clears loaded items and returns the server removal count', async () => {
		backendItems = [item('one'), item('two', { status: 'done' })];
		backendCounts = counts(backendItems);
		const store = createFeedStore();
		await store.refresh();
		mutationFetch.mockResolvedValueOnce(jsonResponse({ removed_count: 2 }));

		expect(await store.clearItems()).toBe(2);
		expect(get(store).items).toEqual([]);
		expect(get(store).counts).toEqual(counts([]));
		expect(mutationFetch.mock.calls[0][1]).toMatchObject({ method: 'DELETE' });
	});

	it('purges orphans and schedules an authoritative count refresh', async () => {
		const store = createFeedStore();
		mutationFetch.mockResolvedValueOnce(jsonResponse({ removed_count: 4 }));
		const callsBefore = feedFetch.mock.calls.length;

		expect(await store.purgeOrphans()).toBe(4);
		await vi.waitFor(() => expect(feedFetch.mock.calls.length).toBe(callsBefore + 2));
		expect(String(mutationFetch.mock.calls[0][0])).toContain(
			'/feed/purge-orphans'
		);
		expect(mutationFetch.mock.calls[0][1]).toMatchObject({ method: 'POST' });
	});

	it('posts learning candidate edits and removes the projected row', async () => {
		backendItems = [
			item('learning_candidate:candidate/1', {
				item_type: 'learning_candidate',
				status: 'needs_action'
			})
		];
		backendCounts = counts(backendItems);
		const store = createFeedStore();
		await store.refresh();
		backendItems = [];
		backendCounts = counts([]);
		mutationFetch.mockResolvedValueOnce(jsonResponse({}));

		await store.editConfirmLearningCandidate(' candidate/1 ', 'revised value');

		expect(get(store).items).toEqual([]);
		expect(String(mutationFetch.mock.calls[0][0])).toContain(
			'/feed/learnings/candidate%2F1/edit-confirm'
		);
		expect(JSON.parse(String(mutationFetch.mock.calls[0][1]?.body))).toEqual({
			revised_value: 'revised value'
		});
	});

	it('returns insight action payloads and removes the insight projection', async () => {
		backendItems = [
			item('insight/1', { item_type: 'learning_insight', status: 'needs_action' })
		];
		backendCounts = counts(backendItems);
		const store = createFeedStore();
		await store.refresh();
		backendItems = [];
		backendCounts = counts([]);
		mutationFetch.mockResolvedValueOnce(jsonResponse({ task_id: 'follow-up-1' }));

		const result = await store.createLearningInsightFollowUp(
			' insight/1 ',
			'Check finding',
			'Review the evidence'
		);

		expect(result).toEqual({ task_id: 'follow-up-1' });
		expect(get(store).items).toEqual([]);
		expect(String(mutationFetch.mock.calls[0][0])).toContain(
			'/feed/insights/insight%2F1/create-follow-up'
		);
		const body = JSON.parse(String(mutationFetch.mock.calls[0][1]?.body));
		expect(body).toMatchObject({
			task_title: 'Check finding',
			task_description: 'Review the evidence'
		});
		expect(body).not.toHaveProperty('principal');
		expect(body).not.toHaveProperty('workspace');
	});
});
