import { beforeEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import {
	backfillTaskNotes,
	fetchTaskNotes,
	promoteTaskNoteToMemory,
	publishTaskToNotes
} from './taskNotes';

const note = {
	projection_id: 'task:task-1',
	schema: 'magician.task-note.v1',
	task_id: 'task-1',
	title: 'Launch brief',
	status: 'completed',
	agent_id: 'researcher',
	thread_id: 'thread-1',
	mode: 'standard',
	requested_provider: 'silverbullet',
	provider: 'silverbullet',
	used_fallback: false,
	task_created_at: '2026-08-01T09:00:00Z',
	task_due_date: '2026-08-03',
	task_completed_at: '2026-08-01T10:00:00Z',
	source_updated_at: '2026-08-01T10:00:00Z',
	published_at: '2026-08-01T10:01:00Z',
	date: '2026-08-01',
	tags: ['magician/task'],
	note_path: 'Tasks/2026-08-01/task-1-launch-brief.md',
	assets: []
};

beforeEach(() => scopeIdentityStore.observe('alice', 'work'));

describe('task Notes API', () => {
	it('uses server pagination and search without URL scope selectors', async () => {
		const { calls } = installFetchMock([
			{
				match: '/notes/published-tasks?',
				handle: () => jsonResponse({ items: [note], offset: 10, limit: 5, total: 16, has_more: true })
			}
		]);

		const page = await fetchTaskNotes({ offset: 10, limit: 5, query: 'launch' });

		expect(page.total).toBe(16);
		expect(calls[0]?.url).not.toContain('principal=');
		expect(calls[0]?.url).not.toContain('workspace=');
		expect(calls[0]?.url).toContain('offset=10');
		expect(calls[0]?.url).toContain('limit=5');
		expect(calls[0]?.url).toContain('q=launch');
	});

	it('publishes one task with explicit projection controls', async () => {
		const { calls } = installFetchMock([
			{ method: 'POST', match: '/notes/publish/task/task%2F1', handle: () => jsonResponse(note) }
		]);

		await publishTaskToNotes('task/1', { mode: 'compact', includeAssets: false });

		expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({
			mode: 'compact',
			include_assets: false
		});
	});

	it('requests bounded unpublished backfill', async () => {
		const { calls } = installFetchMock([
			{
				method: 'POST',
				match: '/notes/publish/tasks/backfill',
				handle: () => jsonResponse({
					published: [note], skipped_task_ids: [], errors: [],
					pagination: { total: 1, offset: 0, limit: 25, has_more: false, next_offset: null }
				})
			}
		]);

		const result = await backfillTaskNotes(25);

		expect(result.published).toHaveLength(1);
		expect(JSON.parse(String(calls[0]?.init?.body))).toMatchObject({
			limit: 25,
			only_unpublished: true
		});
	});

	it('creates a review candidate instead of writing memory directly', async () => {
		const { calls } = installFetchMock([
			{
				method: 'POST',
				match: '/notes/published-tasks/task-1/promote-memory',
				handle: () => jsonResponse({ candidate: { id: 'lc_note_1', state: 'proposed' }, note })
			}
		]);

		const result = await promoteTaskNoteToMemory('task-1', 'Durable launch fact');

		expect(result.candidate.state).toBe('proposed');
		expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({
			summary: 'Durable launch fact'
		});
	});

	it('surfaces backend failures', async () => {
		installFetchMock([
			{
				method: 'POST',
				match: '/notes/publish/task/task-1',
				handle: () => jsonResponse({ message: 'provider unavailable' }, { status: 503 })
			}
		]);

		await expect(publishTaskToNotes('task-1')).rejects.toThrow('provider unavailable');
	});
});
