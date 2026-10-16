import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import TerminalGrantsPanel from './TerminalGrantsPanel.svelte';

afterEach(() => cleanup());

describe('TerminalGrantsPanel theme shell', () => {
	it('renders the plane card without the engine pickers, which live in EnginesPanel', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/plane/grants',
				handle: () => jsonResponse({ grants: [] })
			},
			{
				method: 'GET',
				match: '/plane/engines',
				handle: () =>
					jsonResponse({
						engines: [{ name: 'magician', installed: true, models: ['default'] }],
						current: 'magician',
						chat_current: 'magician',
						run_model: 'default',
						chat_model: 'default'
					})
			}
		]);

		const { container } = render(TerminalGrantsPanel);

		expect(await screen.findByRole('heading', { name: 'Terminal grants' })).toBeInTheDocument();
		expect(container.querySelector('.settings-card')).not.toBeNull();
		expect(screen.queryByTestId('chat-engine-picker')).toBeNull();
		expect(screen.queryByTestId('run-engine-select')).toBeNull();
		await waitFor(() => expect(screen.getByText('No terminal grants yet.')).toBeInTheDocument());
	});
});
