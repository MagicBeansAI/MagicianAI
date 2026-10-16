import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import ObserveNotesPanel from './ObserveNotesPanel.svelte';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

describe('ObserveNotesPanel', () => {
	it('paginates published notes on the server and creates review-gated memory candidates', async () => {
		scopeIdentityStore.observe('alice', 'research');
		const calls: Array<{ url: URL; method: string; body?: Record<string, unknown> }> = [];
		vi.stubGlobal(
			'fetch',
			vi.fn(async (request: string | URL | Request, init?: RequestInit) => {
				const url = new URL(String(request), 'http://localhost');
				const method = init?.method ?? 'GET';
				const body = typeof init?.body === 'string'
					? JSON.parse(init.body) as Record<string, unknown>
					: undefined;
				calls.push({ url, method, body });

				if (url.pathname.endsWith('/promote-memory')) {
					return {
						ok: true,
						status: 201,
						json: async () => ({ candidate: { id: 'lc_note_1', state: 'proposed' } })
					};
				}
				if (url.pathname.endsWith('/publish/tasks/backfill')) {
					return {
						ok: true,
						status: 200,
						json: async () => ({
							published: [],
							skipped_task_ids: [],
							errors: [],
							pagination: { total: 0, offset: 0, limit: 25, has_more: false }
						})
					};
				}

				const offset = Number(url.searchParams.get('offset') ?? 0);
				const limit = Number(url.searchParams.get('limit') ?? 5);
				return {
					ok: true,
					status: 200,
					json: async () => ({
						items: [{
							projection_id: `task:task-${offset}`,
							schema: 'magician.task-note.v1',
							task_id: `task-${offset}`,
							title: `Launch note ${offset}`,
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
							tags: ['magician/task', 'date/2026-08-01'],
							note_path: `Tasks/2026-08-01/task-${offset}.md`,
							open_url: `https://notes.example.test/Tasks/2026-08-01/task-${offset}`,
							assets: []
						}],
						offset,
						limit,
						total: 12,
						has_more: offset + limit < 12
					})
				};
			})
		);

		const user = userEvent.setup();
		render(ObserveNotesPanel);
		expect(await screen.findByText('Launch note 0')).toBeInTheDocument();
		expect(screen.getByText('date/2026-08-01')).toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Next page' }));
		await waitFor(() => expect(calls.at(-1)?.url.searchParams.get('offset')).toBe('5'));
		expect(await screen.findByText('Launch note 5')).toBeInTheDocument();

		await user.selectOptions(screen.getByLabelText('Page size'), '10');
		await waitFor(() => {
			expect(calls.at(-1)?.url.searchParams.get('limit')).toBe('10');
			expect(calls.at(-1)?.url.searchParams.get('offset')).toBe('0');
		});

		await user.click(screen.getByRole('button', { name: 'Promote to memory' }));
		await waitFor(() => expect(calls.some((call) => call.url.pathname.endsWith('/promote-memory'))).toBe(true));
		const promotion = calls.find((call) => call.url.pathname.endsWith('/promote-memory'));
		expect(promotion?.body).not.toHaveProperty('principal');
		expect(promotion?.body).not.toHaveProperty('workspace');

		await user.click(screen.getByRole('button', { name: 'Publish next 25' }));
		await waitFor(() => expect(calls.some((call) => call.url.pathname.endsWith('/publish/tasks/backfill'))).toBe(true));
		const backfill = calls.find((call) => call.url.pathname.endsWith('/publish/tasks/backfill'));
		expect(backfill?.body).toMatchObject({
			limit: 25,
			only_unpublished: true
		});
		expect(backfill?.body).not.toHaveProperty('principal');
		expect(backfill?.body).not.toHaveProperty('workspace');
	});
});
