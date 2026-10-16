import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { taskStore, type Task, type VibeDevRunsPage } from '$lib/stores/taskStore';
import type { VibeDevProject } from '$lib/stores/vibeDevProjectStore';
import StudioRail from './StudioRail.svelte';

afterEach(() => {
	cleanup();
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
});

const project = {
	project_id: 'proj',
	name: 'Trusted rehearsal',
	chat_thread_id: 'vibedev',
	chat_session_id: 'sess',
	deployments: [],
	deploy_targets: [],
	created_at_ms: 1
} as unknown as VibeDevProject;

function archivedPage(offset: number, total: number, query: string): VibeDevRunsPage {
	const count = Math.min(20, total - offset);
	return {
		tasks: Array.from({ length: count }, (_, index) => ({
			id: `run_${offset + index}`,
			title: query ? `Match ${offset + index}` : `Archived run ${offset + index}`,
			description: '',
			status: 'archived',
			updatedAt: '2026-10-01T00:00:00Z'
		})) as unknown as Task[],
		runs: count,
		totalRuns: total,
		offset,
		hasMore: offset + count < total,
		nextCursor: null
	};
}

describe('StudioRail archived runs', () => {
	it('pages archived runs with a bottom pager and searches them on the server', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response('{}', { status: 200, headers: { 'Content-Type': 'application/json' } }))
		);
		const calls: Array<Record<string, unknown>> = [];
		vi.spyOn(taskStore, 'fetchVibeDevProjectRuns').mockImplementation(async (_id, options = {}) => {
			calls.push(options);
			const query = String(options.query ?? '');
			return archivedPage(Number(options.offset ?? 0), query ? 3 : 45, query);
		});
		const user = userEvent.setup();
		render(StudioRail, { props: { projects: [project], activeProject: project } });

		await user.click(screen.getByRole('button', { name: 'Archived' }));
		expect(await screen.findByText('Archived run 0')).toBeInTheDocument();
		expect(calls.at(-1)).toMatchObject({ archived: 'only', offset: 0, query: '' });
		expect(screen.getByText('1-20 of 45')).toBeInTheDocument();
		expect(screen.queryByText(/Load earlier/)).not.toBeInTheDocument();

		await user.click(screen.getByRole('button', { name: 'Next page' }));
		expect(await screen.findByText('Archived run 20')).toBeInTheDocument();
		expect(calls.at(-1)).toMatchObject({ offset: 20 });
		expect(screen.getByText('21-40 of 45')).toBeInTheDocument();

		await user.type(screen.getByRole('searchbox', { name: 'Search archived runs' }), 'login');
		expect(await screen.findByText('Match 0')).toBeInTheDocument();
		await waitFor(() => expect(calls.at(-1)).toMatchObject({ offset: 0, query: 'login' }));
		// Three matches fit on one page: no pager.
		expect(screen.queryByLabelText('Archived run pages')).not.toBeInTheDocument();
	});

	it('labels a crew lane on its run row', async () => {
		vi.spyOn(taskStore, 'fetchVibeDevProjectRuns').mockResolvedValue(null);
		const runs = [
			{ id: 'run_lane', title: 'Build the API', description: '', status: 'running', updatedAt: '2026-10-12T00:00:00Z' },
			{ id: 'run_solo', title: 'Fix the footer', description: '', status: 'running', updatedAt: '2026-10-12T00:00:00Z' }
		] as unknown as Task[];
		render(StudioRail, {
			props: {
				projects: [project],
				activeProject: project,
				runs,
				totalRuns: 2,
				runLinks: { run_lane: { parentTaskId: null, crewId: 'crew-1', lane: 'API' } }
			}
		});
		const chip = await screen.findByText('API');
		expect(chip).toHaveAttribute('title', 'Crew crew-1 · lane API');
		expect(screen.getAllByText(/^API$/)).toHaveLength(1);
	});
});
