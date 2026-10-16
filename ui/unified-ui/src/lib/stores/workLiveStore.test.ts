import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { parseWorkItem, workLiveStore, WORK_FOLLOW_LIMIT } from './workLiveStore';

const item = (id: string, kind: string, state: string, extra: Record<string, unknown> = {}) => ({
	id,
	kind,
	state,
	title: id,
	context: 'Shop',
	step: null,
	by_owner: true,
	from_phone: false,
	followed: false,
	started_at: '2026-10-15T07:00:00Z',
	updated_at: '2026-10-15T07:01:00Z',
	waiting_since: null,
	spent_usd: null,
	budget_usd: null,
	action: null,
	deep_link: 'magican://vibe',
	web_path: '/vibe',
	...extra
});

const json = (body: unknown, status = 200) =>
	new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

const live = {
	featured: item('crew:crew_1', 'crew', 'needs_you'),
	next: [item('run:task_a', 'coding_run', 'working')],
	counts: { needs_you: 1, working: 2, failed: 0, done: 0 },
	items: [
		item('crew:crew_1', 'crew', 'needs_you', { action: { kind: 'answer', id: 'req_1' }, spent_usd: 0.04 }),
		item('run:task_a', 'coding_run', 'working'),
		item('task:task_9', 'work_task', 'paused', { context: '' }),
		item('odd:1', 'robot', 'working')
	],
	followed: [],
	generated_at: '2026-10-15T07:02:00Z'
};

let calls: { url: string; init?: RequestInit }[] = [];
let followReply: () => Response = () => json({ followed: ['crew:crew_1'] });

beforeEach(() => {
	calls = [];
	followReply = () => json({ followed: ['crew:crew_1'] });
	vi.stubGlobal(
		'fetch',
		vi.fn(async (request: string | URL | Request, init?: RequestInit) => {
			const url = String(request);
			calls.push({ url, init });
			if (url.includes('/work/follow')) return followReply();
			return json(live);
		})
	);
});

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('workLiveStore', () => {
	it('parses items and drops kinds and states it cannot draw', () => {
		expect(parseWorkItem({ id: 'run:a', kind: 'coding_run', state: 'sleeping' })).toBeNull();
		expect(parseWorkItem({ id: 'run:a', kind: 'robot', state: 'working' })).toBeNull();
		expect(parseWorkItem({ kind: 'crew', state: 'working' })).toBeNull();
		expect(
			parseWorkItem({ id: 'task:t', kind: 'work_task', state: 'working', title: ' ', action: { kind: 'x', id: 'y' } })
		).toMatchObject({ id: 'task:t', title: 'Untitled', context: '', action: null, webPath: '' });
	});

	it('reads the summary: items, featured, counts', async () => {
		await workLiveStore.refresh();
		const state = get(workLiveStore);
		expect(calls[0].url).toContain('/api/magician/v2/work/live');
		expect(state.loaded).toBe(true);
		expect(state.items.map((row) => row.id)).toEqual(['crew:crew_1', 'run:task_a', 'task:task_9']);
		expect(state.items[0]).toMatchObject({ action: { kind: 'answer', id: 'req_1' }, spentUsd: 0.04 });
		expect(state.featured?.id).toBe('crew:crew_1');
		expect(state.counts).toEqual({ needsYou: 1, working: 2, failed: 0, done: 0 });
	});

	it('follows an item and marks it from the server list', async () => {
		await workLiveStore.refresh();
		const result = await workLiveStore.follow('crew:crew_1');
		expect(result).toEqual({ ok: true });
		const post = calls.find((call) => call.init?.method === 'POST');
		expect(post?.url).toContain('/api/magician/v2/work/follow');
		expect(JSON.parse(String(post?.init?.body))).toEqual({ item_id: 'crew:crew_1' });
		const state = get(workLiveStore);
		expect(state.followed).toEqual(['crew:crew_1']);
		expect(state.items.find((row) => row.id === 'crew:crew_1')?.followed).toBe(true);
		expect(state.featured?.followed).toBe(true);
	});

	it('says why a fourth follow is refused', async () => {
		followReply = () => json({ error: 'follow_limit' }, 409);
		const result = await workLiveStore.follow('task:task_9');
		expect(result.ok).toBe(false);
		expect(get(workLiveStore).followError).toContain(`at most ${WORK_FOLLOW_LIMIT}`);
		workLiveStore.clearFollowError();
		expect(get(workLiveStore).followError).toBeNull();
	});

	it('unfollows by the encoded item id', async () => {
		followReply = () => json({ followed: [] });
		await workLiveStore.unfollow('crew:crew_1');
		const remove = calls.find((call) => call.init?.method === 'DELETE');
		expect(remove?.url).toContain('/api/magician/v2/work/follow/crew%3Acrew_1');
		expect(get(workLiveStore).followed).toEqual([]);
	});
});
