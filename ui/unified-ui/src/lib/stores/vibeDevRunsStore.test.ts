import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { scopeIdentityStore } from './scopeIdentityStore';
import { installFetchMock, jsonResponse } from '../../test/browser';
import { vibeDevRunsStore } from './vibeDevRunsStore';

function row(id: string, updatedAt: string, status = 'completed') {
	return {
		id,
		title: id,
		description: 'VibeDev coding request:\nVibeDev project: proj\n',
		status,
		agent_id: 'cto',
		ui_thread_id: 'vibedev',
		tags: [],
		created_at: updatedAt,
		updated_at: updatedAt
	};
}

beforeEach(() => {
	vibeDevRunsStore.reset();
	scopeIdentityStore.observe('alice', 'workspace-a');
});

afterEach(() => {
	vibeDevRunsStore.reset();
});

describe('vibeDevRunsStore', () => {
	it('loads the first page, then appends older runs from the cursor', async () => {
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/vibedev/projects/proj/runs',
				handle: (request) => {
					const cursor = new URL(request.url, 'http://x').searchParams.get('cursor');
					return cursor
						? jsonResponse({
								tasks: [row('a', '2026-10-01T00:00:00Z')],
								pagination: { runs: 1, total_runs: 2, has_more: false, next_cursor: null }
							})
						: jsonResponse({
								tasks: [row('b', '2026-10-02T00:00:00Z')],
								pagination: { runs: 1, total_runs: 2, has_more: true, next_cursor: 'c1' }
							});
				}
			}
		]);

		await vibeDevRunsStore.load('proj');
		expect(get(vibeDevRunsStore)).toMatchObject({
			projectId: 'proj',
			loaded: true,
			runs: 1,
			totalRuns: 2,
			hasMore: true,
			nextCursor: 'c1'
		});
		expect(calls[0]?.url).toContain('limit=10');

		await vibeDevRunsStore.loadMore();
		const state = get(vibeDevRunsStore);
		expect(state.tasks.map((task) => task.id)).toEqual(['b', 'a']);
		expect(state).toMatchObject({ runs: 2, hasMore: false, nextCursor: null });
		expect(calls[1]?.url).toContain('cursor=c1');
	});

	it('shows nothing loaded until the first page arrives, and resets on no project', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/vibedev/projects/proj/runs',
				handle: () =>
					jsonResponse({
						tasks: [],
						pagination: { runs: 0, total_runs: 0, has_more: false, next_cursor: null }
					})
			}
		]);
		const pending = vibeDevRunsStore.load('proj');
		expect(get(vibeDevRunsStore)).toMatchObject({ loading: true, loaded: false });
		await pending;
		expect(get(vibeDevRunsStore)).toMatchObject({ loading: false, loaded: true, runs: 0 });

		await vibeDevRunsStore.load(null);
		expect(get(vibeDevRunsStore)).toMatchObject({ projectId: null, loaded: false });
	});
});
