import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import WorkspaceStoragePanel from './WorkspaceStoragePanel.svelte';

vi.mock('$lib/stores/confirmationStore', () => ({
	requestConfirmation: vi.fn(async () => true)
}));

vi.mock('$lib/shared/stores/notifications', () => ({
	showError: vi.fn(),
	showSuccess: vi.fn()
}));

const ENVELOPE = {
	settings_path: '/Users/me/MagicianNotes/magician-config.yaml',
	settings: {
		provider: 'local_file',
		silverbullet: { space_path: null }
	},
	resolved: {
		provider: 'local_file',
		local_root: '/Users/me/MagicianNotes',
		silverbullet_space_path: '/Users/me/MagicianNotes',
		active_runtime_root: '/Users/me/MagicianNotes'
	},
	warnings: []
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('WorkspaceStoragePanel', () => {
	it('shows the current provider and resolved roots', async () => {
		installFetchMock([
			{ method: 'GET', match: '/workspace-storage/settings', handle: () => jsonResponse(ENVELOPE) }
		]);
		render(WorkspaceStoragePanel);

		expect(await screen.findByRole('heading', { name: 'Workspace storage' })).toBeTruthy();
		expect(await screen.findByText('/Users/me/MagicianNotes/magician-config.yaml')).toBeTruthy();
		expect(screen.getByRole('radio', { name: 'Local backend data' })).toBeChecked();
		expect(screen.getByRole('button', { name: 'Save workspace storage' })).toBeDisabled();
	});

	it('confirms a provider switch and saves only provider plus space path', async () => {
		const { requestConfirmation } = await import('$lib/stores/confirmationStore');
		const { calls } = installFetchMock([
			{ method: 'GET', match: '/workspace-storage/settings', handle: () => jsonResponse(ENVELOPE) },
			{
				method: 'PUT',
				match: '/workspace-storage/settings',
				handle: () =>
					jsonResponse({
						...ENVELOPE,
						settings: { provider: 'silverbullet_space', silverbullet: { space_path: '/notes/space' } },
						resolved: { ...ENVELOPE.resolved, provider: 'silverbullet_space', active_runtime_root: '/notes/space' },
						warnings: ['silverbullet_space workspace storage is experimental']
					})
			}
		]);
		const user = userEvent.setup();
		render(WorkspaceStoragePanel);

		await user.click(await screen.findByRole('radio', { name: 'Notes folder' }));
		await user.clear(screen.getByLabelText('Notes folder path'));
		await user.type(screen.getByLabelText('Notes folder path'), '/notes/space');
		await user.click(screen.getByRole('button', { name: 'Save workspace storage' }));

		await waitFor(() => expect(requestConfirmation).toHaveBeenCalled());
		await waitFor(() => expect(calls.some((call) => call.method === 'PUT')).toBe(true));
		const save = calls.find((call) => call.method === 'PUT');
		expect(JSON.parse(String(save?.init?.body))).toEqual({
			provider: 'silverbullet_space',
			silverbullet: { space_path: '/notes/space' }
		});
		expect(await screen.findByText(/experimental/)).toBeTruthy();
	});

	it('does not write when the provider switch is cancelled', async () => {
		const { requestConfirmation } = await import('$lib/stores/confirmationStore');
		vi.mocked(requestConfirmation).mockResolvedValueOnce(false);
		const { calls } = installFetchMock([
			{ method: 'GET', match: '/workspace-storage/settings', handle: () => jsonResponse(ENVELOPE) }
		]);
		const user = userEvent.setup();
		render(WorkspaceStoragePanel);

		await user.click(await screen.findByRole('radio', { name: 'Notes folder' }));
		await user.click(screen.getByRole('button', { name: 'Save workspace storage' }));

		await waitFor(() => expect(requestConfirmation).toHaveBeenCalled());
		expect(calls.some((call) => call.method === 'PUT')).toBe(false);
	});
});
