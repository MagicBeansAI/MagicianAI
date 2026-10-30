import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse, settleMicrotasks, textResponse, type MockFetchCall, type MockFetchRoute } from '../../test/browser';
import BudgetsPanel from './BudgetsPanel.svelte';
import type { ActionLimit } from './budgets';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

const LIMITS_URL = '/api/magician/v2/budgets/limits';

afterEach(() => {
	cleanup();
	scopeIdentityStore.reset();
});

const emails: ActionLimit = { scope: 'tool', id: 'agentmail-send', commodity: 'EMAIL_SENDS', ceiling: '50', period: 'daily' };
const whatsapp: ActionLimit = { scope: 'tool', id: 'whatsapp-send', commodity: 'WHATSAPP_SENDS', ceiling: '20', period: 'daily' };
const rupees: ActionLimit = { scope: 'principal', id: 'shop-a', commodity: 'INR', ceiling: '2000', period: 'daily' };

function limitsBody(limits: ActionLimit[]) {
	return { enabled: true, limits, applies: 'on_restart' };
}

/** Model spend answers; listed before the limits routes. */
const modelSpendRoutes: MockFetchRoute[] = [
	{
		method: 'GET',
		match: '/budgets/defaults',
		handle: () => jsonResponse({ native_max_cost_usd: 100, harness_max_cost_usd: 100, native_env_override: false })
	},
	{
		method: 'GET',
		match: '/budgets/envelope',
		handle: () =>
			jsonResponse({
				settings: { enabled: true, daily_usd: 250, total_usd: 'unlimited', window: 'daily', reset_offset_minutes: 0, warn_at_pct: 80, unpriced: 'estimate' },
				installed: false,
				status: null
			})
	}
];

function limitsGet(limits: ActionLimit[]): MockFetchRoute {
	return { method: 'GET', match: LIMITS_URL, handle: () => jsonResponse(limitsBody(limits)) };
}

function body(call: MockFetchCall): unknown {
	return JSON.parse(String(call.init?.body));
}

describe('Spending: model spend', () => {
	it('keeps the model spend controls, explains the ledger and links to it', async () => {
		installFetchMock([...modelSpendRoutes, limitsGet([])]);
		render(BudgetsPanel);
		expect(await screen.findByRole('button', { name: 'Save per run limit' })).toBeTruthy();
		expect(screen.getByRole('heading', { name: 'Model spend' })).toBeTruthy();
		expect(screen.getByText(/posted to its ledger every 15 minutes/)).toBeTruthy();
		expect(screen.getByText('Spending')).toBeTruthy();
		expect(screen.getByRole('link', { name: 'Open the spend ledger' }).getAttribute('href')).toBe('/budget');
		expect(document.getElementById('budgets')).toBeTruthy();
	});
});

describe('Spending: action limits', () => {
	it('hides the block on a server without action limits', async () => {
		const { calls } = installFetchMock([...modelSpendRoutes, { method: 'GET', match: LIMITS_URL, handle: () => textResponse('', { status: 404 }) }]);
		render(BudgetsPanel);
		await screen.findByRole('button', { name: 'Save per run limit' });
		await waitFor(() => expect(calls.some((call) => call.url === LIMITS_URL)).toBe(true));
		await settleMicrotasks();
		expect(screen.queryByRole('heading', { name: 'Action limits' })).toBeNull();
		expect(screen.queryByText('Loading action limits…')).toBeNull();
		expect(screen.getByRole('link', { name: 'Open the spend ledger' })).toBeTruthy();
	});

	it('lists each limit with a readable label, unit and period, and the restart note', async () => {
		installFetchMock([...modelSpendRoutes, limitsGet([emails, whatsapp, rupees])]);
		render(BudgetsPanel);
		expect(await screen.findByText('Emails (agentmail-send)')).toBeTruthy();
		expect(screen.getByText('50 emails per day')).toBeTruthy();
		expect(screen.getByText('WhatsApp sends (whatsapp-send)')).toBeTruthy();
		expect(screen.getByText('Spend in ₹ (shop-a)')).toBeTruthy();
		expect(screen.getByText('₹2,000 per day')).toBeTruthy();
		expect(screen.getByText('Changes apply when Magician restarts.')).toBeTruthy();
		expect((screen.getByRole('textbox', { name: 'Limit for Emails (agentmail-send)' }) as HTMLInputElement).value).toBe('50');
	});

	it('saves a row inline with the new ceiling and period', async () => {
		const { calls } = installFetchMock([
			...modelSpendRoutes,
			limitsGet([emails, whatsapp]),
			{ method: 'PUT', match: LIMITS_URL, handle: () => jsonResponse(limitsBody([{ ...emails, ceiling: '75', period: 'weekly' }, whatsapp])) }
		]);
		render(BudgetsPanel);
		const user = userEvent.setup();
		const input = await screen.findByRole('textbox', { name: 'Limit for Emails (agentmail-send)' });
		await user.clear(input);
		await user.type(input, '$75');
		await user.selectOptions(screen.getByRole('combobox', { name: 'Period for Emails (agentmail-send)' }), 'weekly');
		await user.click(screen.getByRole('button', { name: 'Save Emails (agentmail-send)' }));
		expect(await screen.findByText('75 emails per week')).toBeTruthy();
		const puts = calls.filter((call) => call.method === 'PUT');
		expect(puts).toHaveLength(1);
		expect(puts[0].url).toBe(LIMITS_URL);
		expect(body(puts[0])).toEqual({ scope: 'tool', id: 'agentmail-send', commodity: 'EMAIL_SENDS', ceiling: '75', period: 'weekly' });
	});

	it('refuses an invalid ceiling without asking the server', async () => {
		const { calls } = installFetchMock([...modelSpendRoutes, limitsGet([emails])]);
		render(BudgetsPanel);
		const user = userEvent.setup();
		const input = await screen.findByRole('textbox', { name: 'Limit for Emails (agentmail-send)' });
		await user.clear(input);
		await user.type(input, 'lots');
		await user.click(screen.getByRole('button', { name: 'Save Emails (agentmail-send)' }));
		expect(await screen.findByText('Use a plain number, like 50 or 2000.')).toBeTruthy();
		expect(calls.some((call) => call.method === 'PUT')).toBe(false);
	});

	it("shows the server's message when a save is refused", async () => {
		installFetchMock([
			...modelSpendRoutes,
			limitsGet([emails]),
			{
				method: 'PUT',
				match: LIMITS_URL,
				handle: () => jsonResponse({ error: 'invalid_action_limit', message: 'ceiling must be a positive number, got "x"' }, { status: 400 })
			}
		]);
		render(BudgetsPanel);
		const user = userEvent.setup();
		await screen.findByRole('textbox', { name: 'Limit for Emails (agentmail-send)' });
		await user.click(screen.getByRole('button', { name: 'Save Emails (agentmail-send)' }));
		expect((await screen.findAllByText('ceiling must be a positive number, got "x"')).length).toBeGreaterThan(0);
	});

	it('removes a row', async () => {
		const { calls } = installFetchMock([
			...modelSpendRoutes,
			limitsGet([emails, whatsapp]),
			{ method: 'DELETE', match: LIMITS_URL, handle: () => jsonResponse(limitsBody([whatsapp])) }
		]);
		render(BudgetsPanel);
		const user = userEvent.setup();
		await user.click(await screen.findByRole('button', { name: 'Remove Emails (agentmail-send)' }));
		await waitFor(() => expect(screen.queryByText('Emails (agentmail-send)')).toBeNull());
		expect(screen.getByText('WhatsApp sends (whatsapp-send)')).toBeTruthy();
		const deletes = calls.filter((call) => call.method === 'DELETE');
		expect(deletes).toHaveLength(1);
		expect(deletes[0].url).toBe(`${LIMITS_URL}?scope=tool&id=agentmail-send&commodity=EMAIL_SENDS`);
	});

	it('adds a limit from the form', async () => {
		const added: ActionLimit = { scope: 'principal', id: 'shop-b', commodity: 'INR', ceiling: '1500', period: 'monthly' };
		const { calls } = installFetchMock([
			...modelSpendRoutes,
			limitsGet([]),
			{ method: 'PUT', match: LIMITS_URL, handle: () => jsonResponse(limitsBody([added])) }
		]);
		render(BudgetsPanel);
		const user = userEvent.setup();
		expect(await screen.findByText('No action limits yet.')).toBeTruthy();
		await user.selectOptions(screen.getByRole('combobox', { name: 'New limit applies to' }), 'principal');
		await user.type(screen.getByRole('combobox', { name: 'New limit counts' }), 'inr');
		await user.type(screen.getByRole('textbox', { name: 'New limit id' }), 'shop-b');
		await user.type(screen.getByRole('textbox', { name: 'New limit amount' }), '₹1,500');
		await user.selectOptions(screen.getByRole('combobox', { name: 'New limit period' }), 'monthly');
		await user.click(screen.getByRole('button', { name: 'Add limit' }));
		expect(await screen.findByText('Spend in ₹ (shop-b)')).toBeTruthy();
		expect(screen.getByText('₹1,500 per month')).toBeTruthy();
		const puts = calls.filter((call) => call.method === 'PUT');
		expect(puts).toHaveLength(1);
		expect(body(puts[0])).toEqual(added);
		expect((screen.getByRole('textbox', { name: 'New limit id' }) as HTMLInputElement).value).toBe('');
	});

	it('asks for an id before adding', async () => {
		const { calls } = installFetchMock([...modelSpendRoutes, limitsGet([])]);
		render(BudgetsPanel);
		const user = userEvent.setup();
		await screen.findByText('No action limits yet.');
		await user.click(screen.getByRole('button', { name: 'Add limit' }));
		expect(await screen.findByText('Name the tool, agent or person this limit is for.')).toBeTruthy();
		expect(calls.some((call) => call.method === 'PUT')).toBe(false);
	});
});
