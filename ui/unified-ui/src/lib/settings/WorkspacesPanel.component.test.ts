import { cleanup, render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse, type MockFetchCall } from '../../test/browser';
import WorkspacesPanel from './WorkspacesPanel.svelte';

const WORKSPACES = [
	{ id: 'default', display_name: 'Personal', is_default: true, agent_count: 3, active_task_count: 0, frozen: false },
	{ id: 'research', display_name: 'Research', is_default: false, agent_count: 1, active_task_count: 0, frozen: false },
	{ id: 'scratch', display_name: 'Scratch', is_default: false, agent_count: 0, active_task_count: 0, frozen: false },
	{ id: 'busy', display_name: 'Busy', is_default: false, agent_count: 2, active_task_count: 2, frozen: false }
];

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

function listRoute() {
	return { method: 'GET', match: /\/api\/magician\/v2\/workspaces$/, handle: () => jsonResponse(WORKSPACES) };
}

function deletes(calls: MockFetchCall[]): string[] {
	return calls.filter((c) => c.method === 'DELETE').map((c) => c.url);
}

describe('WorkspacesPanel', () => {
	it('never offers to delete the default workspace or the one you are in', async () => {
		installFetchMock([listRoute()]);
		render(WorkspacesPanel, { props: { currentWorkspace: 'research' } });

		const defaultDelete = await screen.findByRole('button', { name: 'Delete Personal' });
		expect(defaultDelete).toBeDisabled();
		expect(defaultDelete).toHaveAttribute('title', 'The default workspace cannot be deleted.');

		const currentDelete = screen.getByRole('button', { name: 'Delete Research' });
		expect(currentDelete).toBeDisabled();
		expect(currentDelete.getAttribute('title')).toMatch(/Switch to another one/);

		expect(screen.getByRole('button', { name: 'Delete Scratch' })).toBeEnabled();
	});

	it('deletes an empty workspace after one confirmation, without purging', async () => {
		const user = userEvent.setup();
		const onChange = vi.fn();
		const { calls } = installFetchMock([
			listRoute(),
			{ method: 'DELETE', match: '/workspaces/scratch', handle: () => new Response(null, { status: 204 }) }
		]);
		render(WorkspacesPanel, { props: { currentWorkspace: 'default', onChange } });

		await user.click(await screen.findByRole('button', { name: 'Delete Scratch' }));
		const confirm = screen.getByRole('group', { name: 'Delete Scratch' });
		await user.click(confirm.querySelector('button.danger') as HTMLElement);

		await waitFor(() => expect(onChange).toHaveBeenCalled());
		expect(deletes(calls)).toEqual(['/api/magician/v2/workspaces/scratch']);
	});

	it('asks again, and requires the id typed out, before deleting a workspace that holds data', async () => {
		const user = userEvent.setup();
		const { calls } = installFetchMock([
			listRoute(),
			{
				method: 'DELETE',
				match: /\/workspaces\/busy$/,
				handle: () =>
					jsonResponse(
						{ error: 'workspace_has_live_state', message: 'still has live state' },
						{ status: 409 }
					)
			},
			{ method: 'DELETE', match: '/workspaces/busy?purge=true', handle: () => jsonResponse({}, { status: 202 }) }
		]);
		render(WorkspacesPanel, { props: { currentWorkspace: 'default' } });

		await user.click(await screen.findByRole('button', { name: 'Delete Busy' }));
		const group = screen.getByRole('group', { name: 'Delete Busy' });
		await user.click(group.querySelector('button.danger') as HTMLElement);

		// The refusal becomes a second, explicit confirmation — not an error.
		const withData = await screen.findByRole('button', { name: 'Delete with data' });
		expect(within(group).getByText(/still holds data/)).toBeInTheDocument();
		expect(within(group).getByText(/2 tasks are still/)).toBeInTheDocument();
		expect(withData).toBeDisabled();

		const typed = screen.getByRole('textbox', { name: 'Type busy to confirm' });
		await user.type(typed, 'bus');
		expect(withData).toBeDisabled();
		// Only the plain delete has gone out so far — no purge without the id.
		expect(deletes(calls)).toEqual(['/api/magician/v2/workspaces/busy']);

		await user.type(typed, 'y');
		expect(withData).toBeEnabled();
		await user.click(withData);
		await waitFor(() =>
			expect(deletes(calls)).toEqual([
				'/api/magician/v2/workspaces/busy',
				'/api/magician/v2/workspaces/busy?purge=true'
			])
		);
	});

	it('suggests an id from the name and explains a name still waiting to be purged', async () => {
		const user = userEvent.setup();
		const { calls } = installFetchMock([
			listRoute(),
			{
				method: 'POST',
				match: /\/api\/magician\/v2\/workspaces$/,
				handle: () =>
					jsonResponse({ error: 'workspace_pending_purge', message: 'x' }, { status: 409 })
			}
		]);
		render(WorkspacesPanel, { props: { currentWorkspace: 'default' } });

		await user.click(await screen.findByRole('button', { name: 'New workspace' }));
		await user.type(screen.getByRole('textbox', { name: 'Name' }), 'Recipes Eval');
		expect(screen.getByRole('textbox', { name: 'ID' })).toHaveValue('recipes-eval');

		await user.click(screen.getByRole('button', { name: 'Create workspace' }));
		expect(await screen.findByText(/free again after Magician restarts/)).toBeInTheDocument();
		const post = calls.find((c) => c.method === 'POST');
		expect(JSON.parse(String(post?.init?.body))).toMatchObject({ slug: 'recipes-eval', display_name: 'Recipes Eval' });
	});

	it('refuses to create a workspace whose id already exists', async () => {
		const user = userEvent.setup();
		installFetchMock([listRoute()]);
		render(WorkspacesPanel, { props: { currentWorkspace: 'default' } });

		await user.click(await screen.findByRole('button', { name: 'New workspace' }));
		await user.type(screen.getByRole('textbox', { name: 'Name' }), 'Research');
		expect(screen.getByText('A workspace with this ID already exists.')).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Create workspace' })).toBeDisabled();
	});
});
