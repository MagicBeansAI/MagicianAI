import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import { setCurrentScopeBearerToken } from '$lib/stores/scopeIdentityStore';
import EnginesPanel from './EnginesPanel.svelte';

afterEach(() => { cleanup(); setCurrentScopeBearerToken(null); });

describe('EnginesPanel', () => {
	it('renders the chat and background-run engine pickers in their own card', async () => {
		installFetchMock([
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

		const { container } = render(EnginesPanel);

		expect(await screen.findByRole('heading', { name: 'Engines' })).toBeInTheDocument();
		expect(screen.getByTestId('chat-current-engine')).toHaveTextContent('Chat thinks with');
		expect(screen.getByTestId('current-engine')).toHaveTextContent('Background runs think with');
		expect(screen.getByTestId('chat-engine-picker')).toBeInTheDocument();
		expect(screen.getByTestId('run-engine-picker')).toBeInTheDocument();
		expect(screen.getByRole('heading', { name: 'Background runs' })).toBeInTheDocument();
		expect(screen.getByTestId('run-engine-select')).toBeInTheDocument();
		expect(screen.getByTestId('chat-default-for-all')).toHaveTextContent('Make this the default for all clients');
		expect(screen.getByTestId('chat-server-default')).toHaveTextContent('Default for all clients');
		expect(container.querySelector('.settings-card')).not.toBeNull();
	});
});


describe('Decision Engine server setting', () => {
	function routes(fail = false, mode: string | undefined = 'magician_only') {
		setCurrentScopeBearerToken('mst_test_decision_mode');
		return installFetchMock([
			{ match: '/plane/engines', handle: () => jsonResponse({ engines: [{ name: 'magician', installed: true }, { name: 'pi', installed: true }], current: 'pi', chat_current: 'magician', decision_mode: mode }) },
			{ match: '/chat/profiles', handle: () => jsonResponse({ profiles: [] }) },
			{ method: 'PUT', match: '/plane/decision-mode', handle: ({ init }) => fail
				? jsonResponse({ message: 'Save failed' }, { status: 500 })
				: jsonResponse({ decision_mode: JSON.parse(String(init?.body)).mode }) }
		]);
	}

	it('loads the server mode and saves each choice through the shared endpoint', async () => {
		const { calls } = routes();
		render(EnginesPanel);
		const select = screen.getByLabelText('Use Decision Engine');
		await waitFor(() => expect(select).toHaveValue('magician_only'));
		for (const mode of ['off', 'all_engines', 'magician_only']) {
			await fireEvent.change(select, { target: { value: mode } });
			const save = screen.getByRole('button', { name: 'Save Decision Engine setting' });
			await fireEvent.click(save);
			await waitFor(() => expect(select).toBeEnabled());
			await waitFor(() => expect(save).toBeDisabled());
			expect(calls.filter((call) => call.method === 'PUT').at(-1)?.init?.body).toBe(JSON.stringify({ mode }));
		}
	});

	it('keeps the previous saved mode when persistence fails', async () => {
		routes(true);
		render(EnginesPanel);
		const select = screen.getByLabelText('Use Decision Engine');
		await waitFor(() => expect(select).toHaveValue('magician_only'));
		await fireEvent.change(select, { target: { value: 'off' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Save Decision Engine setting' }));
		await waitFor(() => expect(screen.getByRole('button', { name: 'Save Decision Engine setting' })).toBeEnabled());
		await fireEvent.change(select, { target: { value: 'magician_only' } });
		expect(screen.getByRole('button', { name: 'Save Decision Engine setting' })).toBeDisabled();
	});

	it('does not invent a policy when an older server omits it', async () => {
		routes(false, 'unknown');
		render(EnginesPanel);
		await waitFor(() => expect(screen.getByTestId('current-engine')).toHaveTextContent('pi'));
		expect(screen.getByLabelText('Use Decision Engine')).toBeDisabled();
		expect(screen.getByRole('button', { name: 'Save Decision Engine setting' })).toBeDisabled();
	});
});
