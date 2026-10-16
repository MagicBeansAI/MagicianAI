import { get } from 'svelte/store';
import { beforeEach, describe, expect, it, vi } from 'vitest';

// The store guards every network path behind `browser`; force it on so the
// optimistic visibility logic actually runs in the node test env. (vi.mock is
// hoisted above the imports below.)
vi.mock('$app/environment', () => ({ browser: true }));
// The realtime bridge is irrelevant here — stub it so importing the store
// doesn't drag in a live websocket module.
vi.mock('$lib/realtime/v2-websocket', () => ({
	v2Events: {
		subscribe: vi.fn(() => () => {}),
		getConnectionState: vi.fn(() => 'OPEN'),
		connectGlobal: vi.fn()
	},
	getV2EventSequence: vi.fn(() => 0)
}));
vi.mock('$lib/shared/stores/notifications', () => ({
	showError: vi.fn()
}));
vi.mock('$lib/shared/fetch', () => ({
	timedFetch: vi.fn()
}));

import { timedFetch } from '$lib/shared/fetch';
import { showError } from '$lib/shared/stores/notifications';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import type {
	TodayItem,
	TodayResponse,
	TodaySectionId,
	TodaySections
} from '$lib/today/types';
import { createTodayStore, snoozeMinutesFor } from './todayStore';

const timedFetchMock = vi.mocked(timedFetch);
const showErrorMock = vi.mocked(showError);

type TodayStoreInstance = ReturnType<typeof createTodayStore>;

function makeItem(id: string, section: TodaySectionId): TodayItem {
	return {
		id,
		principal: 'anonymous',
		workspace: 'default',
		section,
		priority: 1,
		title: `Item ${id}`,
		summary: null,
		reason: 'test reason',
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

function makePayload(sections: Partial<TodaySections>): TodayResponse {
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
		total: Object.values(full).reduce((sum, items) => sum + items.length, 0)
	};
	return {
		principal: 'anonymous',
		workspace: 'default',
		generated_at: 1,
		freshness: { source: 'test', generated_at: 1 },
		headline: 'Test headline',
		digest: { generated_at: 1, since: null, total: 0, limit: 7, offset: 0, bullets: [] },
		sections: full,
		counts
	};
}

function okJson(payload: unknown): Response {
	return {
		ok: true,
		status: 200,
		json: async () => payload,
		text: async () => JSON.stringify(payload)
	} as unknown as Response;
}

function errorResponse(body = 'boom', status = 500): Response {
	return {
		ok: false,
		status,
		json: async () => ({}),
		text: async () => body
	} as unknown as Response;
}

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}

async function flushAsync(): Promise<void> {
	for (let i = 0; i < 4; i += 1) {
		await new Promise((resolve) => setTimeout(resolve, 0));
	}
}

// Mutable routing state for the fetch mock; each test tweaks these.
let todayPayload: TodayResponse;
let visibilityPost: (url: string, init?: RequestInit) => Promise<Response>;
let todayGetCount = 0;
const postedActions: Array<{ id: string; action: string; snooze_minutes?: number }> = [];

function sectionIds(store: TodayStoreInstance, section: TodaySectionId): string[] {
	return get(store).sections[section].map((item) => item.id);
}

async function seedStore(sections: Partial<TodaySections>): Promise<TodayStoreInstance> {
	const store = createTodayStore();
	todayPayload = makePayload(sections);
	await store.refresh();
	await flushAsync();
	return store;
}

beforeEach(() => {
	todayGetCount = 0;
	postedActions.length = 0;
	todayPayload = makePayload({});
	visibilityPost = () => Promise.resolve(okJson({}));
	showErrorMock.mockClear();
	scopeIdentityStore.reset();
	timedFetchMock.mockReset();
	timedFetchMock.mockImplementation((input, init) => {
		const url = String(input);
		if (url.includes('/today/items/')) {
			return recordedVisibilityPost(url, init as RequestInit | undefined);
		}
		if (url.includes('/today/visibility')) {
			return Promise.resolve(okJson({ items: [] }));
		}
		if (url.includes('/v2/today')) {
			todayGetCount += 1;
			return Promise.resolve(okJson(todayPayload));
		}
		return Promise.reject(new Error(`unexpected fetch: ${url}`));
	});
});

function recordedVisibilityPost(url: string, init?: RequestInit): Promise<Response> {
	const body = JSON.parse(String(init?.body ?? '{}')) as {
		action?: string;
		snooze_minutes?: number;
	};
	const id = decodeURIComponent(url.split('/items/')[1]?.split('/')[0] ?? '');
	postedActions.push({
		id,
		action: String(body.action),
		...(typeof body.snooze_minutes === 'number' ? { snooze_minutes: body.snooze_minutes } : {})
	});
	return visibilityPost(url, init);
}

describe('todayStore optimistic dismiss/snooze', () => {
	it('tracks the fetched page key per section for first-load skeleton gating', async () => {
		const store = createTodayStore({
			per_section: 8,
			section: 'delivered',
			limit: 8,
			cursor: null
		});
		todayPayload = makePayload({ delivered: [makeItem('done-a', 'delivered')] });

		await store.refresh();
		await flushAsync();

		expect(get(store).loadedSectionPageKeys.delivered).toBe('8:');
		expect(get(store).loadedSectionPageKeys.followups).toBeNull();

		store.setQuery({
			per_section: 8,
			section: 'followups',
			limit: 8,
			cursor: 'cursor-followups-page-2'
		});
		todayPayload = makePayload({ followups: [] });
		await store.refresh();
		await flushAsync();

		expect(get(store).loadedSectionPageKeys.delivered).toBe('8:');
		expect(get(store).loadedSectionPageKeys.followups).toBe('8:cursor-followups-page-2');
	});

	it('removes the item from local state before the POST resolves and keeps it out on success', async () => {
		const store = await seedStore({
			needs_you: [makeItem('item-a', 'needs_you'), makeItem('item-b', 'needs_you')]
		});
		const hidePost = deferred<Response>();
		visibilityPost = () => hidePost.promise;

		await store.updateVisibility('item-b', 'dismiss');

		// POST still pending — the row is already gone (optimistic).
		expect(sectionIds(store, 'needs_you')).toEqual(['item-a']);
		expect(get(store).counts.needs_you).toBe(1);
		expect(get(store).counts.total).toBe(1);

		// The backend applies the dismissal before the success response;
		// the store then refetches the active cursor page to fill any gap.
		todayPayload = makePayload({ needs_you: [makeItem('item-a', 'needs_you')] });
		hidePost.resolve(okJson({}));
		await flushAsync();

		expect(sectionIds(store, 'needs_you')).toEqual(['item-a']);
		expect(showErrorMock).not.toHaveBeenCalled();
	});

	it('refetches the active server page after a successful dismiss', async () => {
		const store = await seedStore({ needs_you: [makeItem('item-a', 'needs_you')] });
		const getsAfterSeed = todayGetCount;
		todayPayload = makePayload({ needs_you: [] });

		await store.updateVisibility('item-a', 'dismiss');
		await flushAsync();

		expect(todayGetCount).toBe(getsAfterSeed + 1);
		expect(sectionIds(store, 'needs_you')).toEqual([]);
	});

	it('a refresh racing an in-flight hide keeps the row out and the counts in step', async () => {
		const store = await seedStore({
			needs_you: [makeItem('item-a', 'needs_you'), makeItem('item-b', 'needs_you')]
		});
		const hidePost = deferred<Response>();
		visibilityPost = () => hidePost.promise;

		await store.updateVisibility('item-b', 'dismiss');
		expect(sectionIds(store, 'needs_you')).toEqual(['item-a']);

		// A poll/WS-style refetch lands while the hide POST is still pending
		// and the backend hasn't applied the dismiss yet — the payload still
		// contains the hidden item (and counts it).
		await store.refresh();
		await flushAsync();

		// The pendingHiddenItemIds filter keeps the row out, and the applied
		// counts match the visible rows (no transient overcount).
		expect(sectionIds(store, 'needs_you')).toEqual(['item-a']);
		expect(get(store).counts.needs_you).toBe(1);
		expect(get(store).counts.total).toBe(1);

		// POST succeeds after the backend has applied the hide. The store's
		// automatic page reconciliation also receives the row-free payload.
		todayPayload = makePayload({ needs_you: [makeItem('item-a', 'needs_you')] });
		hidePost.resolve(okJson({}));
		await flushAsync();
		expect(sectionIds(store, 'needs_you')).toEqual(['item-a']);
		expect(get(store).counts.needs_you).toBe(1);
		expect(get(store).counts.total).toBe(1);
		expect(showErrorMock).not.toHaveBeenCalled();
	});

	it('mark_seen stays non-optimistic: the row remains and the store refetches', async () => {
		const store = await seedStore({ delivered: [makeItem('item-a', 'delivered')] });
		const getsAfterSeed = todayGetCount;

		await store.updateVisibility('item-a', 'mark_seen');
		await flushAsync();

		expect(sectionIds(store, 'delivered')).toEqual(['item-a']);
		expect(postedActions.map((post) => post.action)).toEqual(['mark_seen']);
		expect(todayGetCount).toBe(getsAfterSeed + 1);
	});

	it('restores the item to its original section and index when the POST fails', async () => {
		const store = await seedStore({
			needs_you: [
				makeItem('item-a', 'needs_you'),
				makeItem('item-b', 'needs_you'),
				makeItem('item-c', 'needs_you')
			]
		});
		visibilityPost = () => Promise.resolve(errorResponse('backend down'));

		await store.updateVisibility('item-b', 'snooze', { snoozeMinutes: 60 });
		expect(sectionIds(store, 'needs_you')).toEqual(['item-a', 'item-c']);

		await flushAsync();

		expect(sectionIds(store, 'needs_you')).toEqual(['item-a', 'item-b', 'item-c']);
		expect(get(store).counts.needs_you).toBe(3);
		expect(get(store).counts.total).toBe(3);
		expect(showErrorMock).toHaveBeenCalledTimes(1);
		expect(showErrorMock).toHaveBeenCalledWith('Failed to snooze Today item', 'backend down');
	});

	it('never restores a failed hide into a different scope state (scope guard)', async () => {
		const store = await seedStore({
			needs_you: [makeItem('item-a', 'needs_you'), makeItem('item-b', 'needs_you')]
		});
		const hidePost = deferred<Response>();
		visibilityPost = () => hidePost.promise;

		await store.updateVisibility('item-a', 'dismiss');
		expect(sectionIds(store, 'needs_you')).toEqual(['item-b']);

		// Principal/workspace changes while the POST is in flight.
		scopeIdentityStore.observe('someone-else', 'other-workspace');
		hidePost.resolve(errorResponse('too late'));
		await flushAsync();

		// The late failure must not inject the row back (nor toast for it).
		expect(sectionIds(store, 'needs_you')).toEqual(['item-b']);
		expect(showErrorMock).not.toHaveBeenCalled();

		scopeIdentityStore.reset();
	});

	it('undoLastHidden waits for the in-flight hide POST, then restores via the restore action', async () => {
		const store = await seedStore({
			needs_you: [makeItem('item-a', 'needs_you'), makeItem('item-b', 'needs_you')]
		});
		const hidePost = deferred<Response>();
		visibilityPost = (url, init) => {
			const body = JSON.parse(String(init?.body ?? '{}')) as { action?: string };
			return body.action === 'dismiss' ? hidePost.promise : Promise.resolve(okJson({}));
		};

		await store.updateVisibility('item-a', 'dismiss');
		expect(sectionIds(store, 'needs_you')).toEqual(['item-b']);

		// Undo fires before the hide POST completes.
		const undoPromise = store.undoLastHidden();
		await flushAsync();
		expect(postedActions.map((post) => post.action)).toEqual(['dismiss']);

		hidePost.resolve(okJson({}));
		const undone = await undoPromise;
		await flushAsync();

		expect(undone).toBe(true);
		expect(postedActions.map((post) => post.action)).toEqual(['dismiss', 'restore']);
		// The restore path refetches, bringing the row back.
		expect(sectionIds(store, 'needs_you')).toEqual(['item-a', 'item-b']);
	});

	it('undoLastHidden bails without a restore POST when the scope changes mid-undo', async () => {
		const store = await seedStore({ needs_you: [makeItem('item-a', 'needs_you')] });
		const hidePost = deferred<Response>();
		visibilityPost = (url, init) => {
			const body = JSON.parse(String(init?.body ?? '{}')) as { action?: string };
			return body.action === 'dismiss' ? hidePost.promise : Promise.resolve(okJson({}));
		};

		await store.updateVisibility('item-a', 'dismiss');
		const undoPromise = store.undoLastHidden();

		// Principal/workspace switches while undo awaits the in-flight hide
		// POST — the restore must not fire into the new scope.
		scopeIdentityStore.observe('someone-else', 'other-workspace');
		hidePost.resolve(okJson({}));

		expect(await undoPromise).toBe(false);
		await flushAsync();
		expect(postedActions.map((post) => post.action)).toEqual(['dismiss']);

		scopeIdentityStore.reset();
	});

	it('undoLastHidden is a no-op without a prior hide, and after a failed hide', async () => {
		const store = await seedStore({ needs_you: [makeItem('item-a', 'needs_you')] });
		expect(await store.undoLastHidden()).toBe(false);

		visibilityPost = () => Promise.resolve(errorResponse('nope'));
		await store.updateVisibility('item-a', 'dismiss');
		await flushAsync();
		// The failure path restored the row itself — nothing left to undo.
		expect(sectionIds(store, 'needs_you')).toEqual(['item-a']);
		expect(await store.undoLastHidden()).toBe(false);
		expect(postedActions.map((post) => post.action)).toEqual(['dismiss']);
	});
});

describe('snoozeMinutesFor', () => {
	// All dates are local-time constructions in July, safely away from DST
	// transitions. 2026-07-01 is a Wednesday; 2026-07-06 is a Monday.

	it('tonight before 18:00 snoozes until 18:00 today', () => {
		expect(snoozeMinutesFor('tonight', new Date(2026, 6, 1, 15, 0))).toBe(180);
		expect(snoozeMinutesFor('tonight', new Date(2026, 6, 1, 17, 30))).toBe(30);
	});

	it('tonight at/after 18:00 falls back to now + 3h', () => {
		expect(snoozeMinutesFor('tonight', new Date(2026, 6, 1, 18, 0))).toBe(180);
		expect(snoozeMinutesFor('tonight', new Date(2026, 6, 1, 21, 45))).toBe(180);
	});

	it('tomorrow morning crosses midnight to the next 08:00', () => {
		expect(snoozeMinutesFor('tomorrow_morning', new Date(2026, 6, 1, 23, 30))).toBe(510);
	});

	it('tomorrow morning targets the next 08:00 from any time of day', () => {
		expect(snoozeMinutesFor('tomorrow_morning', new Date(2026, 6, 1, 9, 0))).toBe(23 * 60);
		expect(snoozeMinutesFor('tomorrow_morning', new Date(2026, 6, 1, 7, 0))).toBe(60);
		expect(snoozeMinutesFor('tomorrow_morning', new Date(2026, 6, 1, 8, 0))).toBe(24 * 60);
	});

	it('next week lands on the next Monday 08:00 from every weekday', () => {
		const cases: Array<[Date, number]> = [
			[new Date(2026, 6, 5, 9, 0), 23 * 60], // Sunday
			[new Date(2026, 6, 6, 9, 0), 7 * 1440 - 60], // Monday → a full week out
			[new Date(2026, 6, 7, 9, 0), 6 * 1440 - 60], // Tuesday
			[new Date(2026, 6, 8, 9, 0), 5 * 1440 - 60], // Wednesday
			[new Date(2026, 6, 9, 9, 0), 4 * 1440 - 60], // Thursday
			[new Date(2026, 6, 10, 9, 0), 3 * 1440 - 60], // Friday
			[new Date(2026, 6, 11, 9, 0), 2 * 1440 - 60] // Saturday
		];
		for (const [now, expected] of cases) {
			const minutes = snoozeMinutesFor('next_week', now);
			expect(minutes).toBe(expected);
			const target = new Date(now.getTime() + minutes * 60_000);
			expect(target.getDay()).toBe(1);
			expect(target.getHours()).toBe(8);
			expect(target.getMinutes()).toBe(0);
		}
	});
});
