import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import ArchivedTasksList from './ArchivedTasksList.svelte';

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

function page(offset: number, total: number) {
	return {
		tasks: [
			{
				id: `task_${offset}`,
				title: `Archived job ${offset}`,
				description: `Why job ${offset} existed`,
				agent_id: 'cto',
				updated_at: 1_700_000_000_000
			}
		],
		pagination: { total, limit: 25, offset, has_more: offset + 25 < total, next_cursor: null }
	};
}

describe('ArchivedTasksList', () => {
	it('reads archived tasks from the server a page at a time', async () => {
		const requested: URL[] = [];
		vi.stubGlobal(
			'fetch',
			vi.fn(async (request: string | URL | Request) => {
				const url = new URL(String(request), 'http://localhost');
				requested.push(url);
				const offset = Number(url.searchParams.get('offset'));
				return new Response(JSON.stringify(page(offset, 30)), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				});
			})
		);
		const user = userEvent.setup();
		render(ArchivedTasksList);

		expect(await screen.findByText('Archived job 0')).toBeInTheDocument();
		expect(requested[0].pathname).toBe('/api/magician/v3/tasks');
		expect(requested[0].searchParams.get('archived')).toBe('only');
		expect(requested[0].searchParams.get('offset')).toBe('0');

		await user.click(screen.getByRole('button', { name: /Archived job 0/ }));
		expect(screen.getByText('Why job 0 existed')).toBeInTheDocument();

		// The shared server pager, above and below the list, with a last-page jump.
		expect(screen.getAllByText(/Page 1 of 2/)).toHaveLength(2);
		await user.click(screen.getAllByRole('button', { name: 'Last page' })[0]);
		expect(await screen.findByText('Archived job 25')).toBeInTheDocument();
		await waitFor(() => expect(requested.at(-1)!.searchParams.get('offset')).toBe('25'));
		expect(screen.getAllByText(/Page 2 of 2/)).toHaveLength(2);
	});

	it('says so when nothing is archived', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () =>
				new Response(JSON.stringify({ tasks: [], pagination: { total: 0 } }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				})
			)
		);
		render(ArchivedTasksList);
		expect(await screen.findByText('No archived tasks.')).toBeInTheDocument();
	});
});
