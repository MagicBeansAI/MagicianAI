import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import WorkspaceStoragePanel from './WorkspaceStoragePanel.svelte';

vi.mock('$lib/shared/stores/notifications', () => ({
	showError: vi.fn(),
	showSuccess: vi.fn()
}));

const ENVELOPE = {
	settings_path: '/Users/me/MagicianNotes/magician-config.yaml',
	settings: { provider: 'local_file' },
	resolved: {
		provider: 'local_file',
		local_root: '/Users/me/MagicianNotes',
		active_runtime_root: '/Users/me/MagicianNotes'
	},
	warnings: []
};

afterEach(() => {
	cleanup();
	vi.clearAllMocks();
});

describe('WorkspaceStoragePanel', () => {
	it('shows where runtime state lives without offering a notes-folder backend', async () => {
		const { calls } = installFetchMock([
			{ method: 'GET', match: '/workspace-storage/settings', handle: () => jsonResponse(ENVELOPE) }
		]);
		render(WorkspaceStoragePanel);

		expect(await screen.findByRole('heading', { name: 'Workspace storage' })).toBeTruthy();
		expect(await screen.findByText('/Users/me/MagicianNotes/magician-config.yaml')).toBeTruthy();
		expect(screen.getByText('Runtime root')).toBeTruthy();
		expect(screen.queryByRole('radio')).toBeNull();
		expect(screen.queryByLabelText('Notes folder path')).toBeNull();
		expect(screen.queryByRole('button', { name: 'Save workspace storage' })).toBeNull();
		expect(calls.every((call) => call.method === 'GET')).toBe(true);
	});

	it('surfaces server warnings', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/workspace-storage/settings',
				handle: () => jsonResponse({ ...ENVELOPE, warnings: ['The runtime root is not writable.'] })
			}
		]);
		render(WorkspaceStoragePanel);

		expect(await screen.findByText('The runtime root is not writable.')).toBeTruthy();
	});
});
