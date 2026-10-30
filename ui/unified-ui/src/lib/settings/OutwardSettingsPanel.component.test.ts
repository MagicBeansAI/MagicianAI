import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse, type MockFetchRoute } from '../../test/browser';
import OutwardSettingsPanel from './OutwardSettingsPanel.svelte';
import { captures, type OutwardEnvelope, type OutwardSettings } from './outwardSettings';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';

const envelope: OutwardEnvelope = {
	revision: 'revision-1', applied: true,
	settings: { capture_only: true, force_capture_only: false, contexts: { owner_chat: 'inherit', owner_tasks: 'inherit', opc: 'inherit' }, browser_submission_guard: true },
	effective: { owner_chat: true, owner_tasks: true, opc: true, unclassified: true }
};

afterEach(() => { cleanup(); scopeIdentityStore.reset(); });

/**
 * A server without standing grants. Listed first: route matching is by
 * substring, so the settings GET would otherwise answer the grants GET too.
 */
const noGrants: MockFetchRoute = {
	method: 'GET',
	match: '/outward-actions/grants',
	handle: () => jsonResponse({ message: 'Not found' }, { status: 404 })
};

describe('outward sending controls', () => {
	it('loads without writing and explains all three origins', async () => {
		const { calls } = installFetchMock([noGrants, { method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(envelope) }]);
		render(OutwardSettingsPanel);
		expect(await screen.findByRole('combobox', { name: 'Your chats and delegated work' })).toBeTruthy();
		expect(screen.getByRole('combobox', { name: 'Tasks you create in Work' })).toBeTruthy();
		expect(screen.getByRole('combobox', { name: 'OPC and autonomous work' })).toBeTruthy();
		expect(screen.getByText(/including specialists and tasks launched/)).toBeTruthy();
		expect(calls.every((call) => call.method === 'GET')).toBe(true);
		expect((screen.getByRole('button', { name: 'Save and apply' }) as HTMLButtonElement).disabled).toBe(true);
	});

	it('saves owner and OPC choices with revision protection and keeps the browser guard', async () => {
		let saved: { expected_revision: string; settings: OutwardSettings } | null = null;
		installFetchMock([noGrants,
			{ method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(envelope) },
			{ method: 'PUT', match: '/settings/outward-actions', handle: (call) => {
				saved = JSON.parse(String(call.init?.body));
				return jsonResponse({ ...envelope, revision: 'revision-2', settings: saved!.settings });
			} }
		]);
		render(OutwardSettingsPanel);
		const user = userEvent.setup();
		await user.selectOptions(await screen.findByRole('combobox', { name: 'Your chats and delegated work' }), 'live');
		await user.selectOptions(screen.getByRole('combobox', { name: 'Default sending policy' }), 'false');
		await user.selectOptions(screen.getByRole('combobox', { name: 'OPC and autonomous work' }), 'live');
		await user.click(screen.getByRole('button', { name: 'Save and apply' }));
		await screen.findByText('Saved and applied. No restart needed.');
		expect(saved).toMatchObject({ expected_revision: 'revision-1', settings: { capture_only: false, browser_submission_guard: true, contexts: { owner_chat: 'live', owner_tasks: 'inherit', opc: 'live' } } });
		await user.selectOptions(screen.getByRole('combobox', { name: 'OPC and autonomous work' }), 'capture');
		expect(screen.queryByText('Saved and applied. No restart needed.')).toBeNull();
	});

	it('lets OPC send while Envoy and public agents stay captured', async () => {
		const withPublic: OutwardEnvelope = {
			...envelope,
			settings: { ...envelope.settings, contexts: { ...envelope.settings.contexts, public_agents: 'inherit' } },
			effective: { ...envelope.effective, public_agents: true }
		};
		let saved: { expected_revision: string; settings: OutwardSettings } | null = null;
		installFetchMock([noGrants,
			{ method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(withPublic) },
			{ method: 'PUT', match: '/settings/outward-actions', handle: (call) => {
				saved = JSON.parse(String(call.init?.body));
				return jsonResponse({ ...withPublic, revision: 'revision-2', settings: saved!.settings });
			} }
		]);
		render(OutwardSettingsPanel);
		const user = userEvent.setup();
		await user.selectOptions(await screen.findByRole('combobox', { name: 'OPC and autonomous work' }), 'live');
		await user.selectOptions(screen.getByRole('combobox', { name: 'Envoy and public agents' }), 'capture');
		await user.click(screen.getByRole('button', { name: 'Save and apply' }));
		await screen.findByText('Saved and applied. No restart needed.');
		expect(saved).toMatchObject({ settings: { contexts: { opc: 'live', public_agents: 'capture' } } });
	});

	it('hides the public-agents row for a server that has not split it out of OPC', async () => {
		installFetchMock([noGrants, { method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(envelope) }]);
		render(OutwardSettingsPanel);
		await screen.findByRole('combobox', { name: 'OPC and autonomous work' });
		expect(screen.queryByRole('combobox', { name: 'Envoy and public agents' })).toBeNull();
	});

	it('keeps unsaved choices on a stale-save rejection and does not claim success', async () => {
		installFetchMock([noGrants,
			{ method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(envelope) },
			{ method: 'PUT', match: '/settings/outward-actions', handle: () => jsonResponse({ message: 'These settings changed elsewhere. Reload before saving.' }, { status: 409 }) }
		]);
		render(OutwardSettingsPanel);
		const user = userEvent.setup();
		const select = await screen.findByRole('combobox', { name: 'Tasks you create in Work' });
		await user.selectOptions(select, 'live');
		await user.click(screen.getByRole('button', { name: 'Save and apply' }));
		await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('changed elsewhere'));
		expect((select as HTMLSelectElement).value).toBe('live');
		expect(screen.queryByText('Saved and applied. No restart needed.')).toBeNull();
	});

	it('force capture overrides live choices in the preview', async () => {
		installFetchMock([noGrants, { method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(envelope) }]);
		render(OutwardSettingsPanel);
		const user = userEvent.setup();
		await user.selectOptions(await screen.findByRole('combobox', { name: 'Your chats and delegated work' }), 'live');
		expect(screen.getByText('After saving: Real sends permitted')).toBeTruthy();
		await user.click(screen.getByRole('checkbox', { name: /Capture all sending categories/ }));
		expect(screen.queryByText('After saving: Real sends permitted')).toBeNull();
		for (const select of screen.getAllByRole('combobox')) {
			expect((select as HTMLSelectElement).disabled).toBe(true);
		}
		expect((screen.getByRole('checkbox', { name: /Review browser form submissions/ }) as HTMLInputElement).disabled).toBe(false);
		await user.click(screen.getByRole('checkbox', { name: /Capture all sending categories/ }));
		for (const select of screen.getAllByRole('combobox')) {
			expect((select as HTMLSelectElement).disabled).toBe(false);
		}
		expect((screen.getByRole('combobox', { name: 'Your chats and delegated work' }) as HTMLSelectElement).value).toBe('live');
	});

	it('previews capture/live/inherit against either baseline', () => {
		const settings = structuredClone(envelope.settings);
		settings.capture_only = false;
		expect(captures(settings, 'opc')).toBe(false);
		settings.contexts.opc = 'capture';
		expect(captures(settings, 'opc')).toBe(true);
		settings.contexts.owner_chat = 'live';
		expect(captures(settings, 'owner_chat')).toBe(false);
		settings.force_capture_only = true;
		expect(captures(settings, 'owner_chat')).toBe(true);
	});

	it('rejects an invalid response instead of offering default controls to save', async () => {
		installFetchMock([noGrants, { method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse({}) }]);
		render(OutwardSettingsPanel);
		expect((await screen.findByRole('alert')).textContent).toContain('invalid outward settings');
		expect(screen.queryByRole('combobox', { name: 'Default sending policy' })).toBeNull();
		expect((screen.getByRole('button', { name: 'Save and apply' }) as HTMLButtonElement).disabled).toBe(true);
	});
});

describe('review before sending', () => {
	const reviewing: OutwardEnvelope = { ...envelope, settings: { ...envelope.settings, review_captured_sends: true } };

	it('hides the captured-send choice on a server that cannot pause for review', async () => {
		let saved: { settings: OutwardSettings } | null = null;
		installFetchMock([
			noGrants,
			{ method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(envelope) },
			{ method: 'PUT', match: '/settings/outward-actions', handle: (call) => {
				saved = JSON.parse(String(call.init?.body));
				return jsonResponse({ ...envelope, revision: 'revision-2', settings: saved!.settings });
			} }
		]);
		render(OutwardSettingsPanel);
		const user = userEvent.setup();
		await user.selectOptions(await screen.findByRole('combobox', { name: 'Default sending policy' }), 'false');
		expect(screen.queryByRole('combobox', { name: 'When a send is captured' })).toBeNull();
		await user.click(screen.getByRole('button', { name: 'Save and apply' }));
		await screen.findByText('Saved and applied. No restart needed.');
		// Never invented for a server that did not send it.
		expect(saved!.settings).not.toHaveProperty('review_captured_sends');
	});

	it('shows the choice when the server has it and saves it with the rest', async () => {
		let saved: { expected_revision: string; settings: OutwardSettings } | null = null;
		installFetchMock([
			noGrants,
			{ method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(reviewing) },
			{ method: 'PUT', match: '/settings/outward-actions', handle: (call) => {
				saved = JSON.parse(String(call.init?.body));
				return jsonResponse({ ...reviewing, revision: 'revision-2', settings: saved!.settings });
			} }
		]);
		render(OutwardSettingsPanel);
		const user = userEvent.setup();
		const select = await screen.findByRole('combobox', { name: 'When a send is captured' });
		expect((select as HTMLSelectElement).value).toBe('true');
		expect(screen.getByText(/Pauses the agent and asks you on Attention and your phones/)).toBeTruthy();
		await user.selectOptions(select, 'false');
		expect(screen.getByText('Captured sends are recorded and never sent.')).toBeTruthy();
		await user.click(screen.getByRole('button', { name: 'Save and apply' }));
		await screen.findByText('Saved and applied. No restart needed.');
		expect(saved).toMatchObject({ expected_revision: 'revision-1', settings: { review_captured_sends: false, capture_only: true } });
		expect((screen.getByRole('combobox', { name: 'When a send is captured' }) as HTMLSelectElement).value).toBe('false');
	});

	it('stays editable under capture-all and says capture-all does not ask', async () => {
		installFetchMock([noGrants, { method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(reviewing) }]);
		render(OutwardSettingsPanel);
		const user = userEvent.setup();
		const select = await screen.findByRole('combobox', { name: 'When a send is captured' });
		expect(screen.queryByText('Capture-all records sends without asking.')).toBeNull();
		await user.click(screen.getByRole('checkbox', { name: /Capture all sending categories/ }));
		expect(screen.getByText('Capture-all records sends without asking.')).toBeTruthy();
		expect((select as HTMLSelectElement).disabled).toBe(false);
	});

	it('rejects a non-boolean review flag as an invalid envelope', async () => {
		installFetchMock([noGrants, { method: 'GET', match: '/settings/outward-actions', handle: () =>
			jsonResponse({ ...envelope, settings: { ...envelope.settings, review_captured_sends: 'yes' } }) }]);
		render(OutwardSettingsPanel);
		expect((await screen.findByRole('alert')).textContent).toContain('invalid outward settings');
	});
});

describe('always allowed', () => {
	const grant = { id: 'g-1', agent_id: 'ambassador', capability: 'gmail', recipient: 'a@b.com', granted_at_ms: 1_790_000_000_000 };

	it('reloads permissions after a workspace switch and ignores an old revoke response', async () => {
		scopeIdentityStore.observe('owner', 'first');
		let workspace = 'first';
		let finishDelete!: (response: Response) => void;
		const pendingDelete = new Promise<Response>((resolve) => { finishDelete = resolve; });
		installFetchMock([
			{ method: 'GET', match: '/outward-actions/grants', handle: () => jsonResponse({
				grants: [{ ...grant, recipient: workspace === 'first' ? 'first@example.com' : 'second@example.com' }]
			}) },
			{ method: 'DELETE', match: '/outward-actions/grants/g-1', handle: () => pendingDelete },
			{ method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(envelope) }
		]);
		render(OutwardSettingsPanel);
		const user = userEvent.setup();
		await user.click(await screen.findByRole('button', { name: 'Revoke ambassador sending to first@example.com' }));
		workspace = 'second';
		scopeIdentityStore.observe('owner', 'second');
		await screen.findByRole('button', { name: 'Revoke ambassador sending to second@example.com' });
		expect(screen.queryByText(/first@example.com/)).toBeNull();
		finishDelete(new Response(null, { status: 204 }));
		await waitFor(() => expect(screen.getByTestId('outward-grant').textContent).toContain('second@example.com'));
	});

	it('ignores settings and permissions loaded for a previous workspace', async () => {
		scopeIdentityStore.observe('owner', 'first');
		let settingsCalls = 0;
		let grantsCalls = 0;
		let finishSettings!: (response: Response) => void;
		let finishGrants!: (response: Response) => void;
		const oldSettings = new Promise<Response>((resolve) => { finishSettings = resolve; });
		const oldGrants = new Promise<Response>((resolve) => { finishGrants = resolve; });
		installFetchMock([
			{ method: 'GET', match: '/outward-actions/grants', handle: () => ++grantsCalls === 1
				? oldGrants : jsonResponse({ grants: [{ ...grant, recipient: 'second@example.com' }] }) },
			{ method: 'GET', match: '/settings/outward-actions', handle: () => ++settingsCalls === 1
				? oldSettings : jsonResponse(envelope) }
		]);
		render(OutwardSettingsPanel);
		await waitFor(() => expect(settingsCalls).toBe(1));
		scopeIdentityStore.observe('owner', 'second');
		await screen.findByRole('button', { name: 'Revoke ambassador sending to second@example.com' });
		finishSettings(jsonResponse({ ...envelope, settings: { ...envelope.settings, force_capture_only: true } }));
		finishGrants(jsonResponse({ grants: [{ ...grant, recipient: 'first@example.com' }] }));
		await new Promise((resolve) => setTimeout(resolve, 0));
		expect((screen.getByRole('checkbox', { name: /Capture all sending categories/ }) as HTMLInputElement).checked).toBe(false);
		expect(screen.getByTestId('outward-grant').textContent).toContain('second@example.com');
	});

	it('lists standing permissions and revokes one', async () => {
		const { calls } = installFetchMock([
			{ method: 'GET', match: '/outward-actions/grants', handle: () => jsonResponse({ grants: [grant] }) },
			{ method: 'DELETE', match: '/outward-actions/grants/g-1', handle: () => new Response(null, { status: 204 }) },
			{ method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(envelope) }
		]);
		render(OutwardSettingsPanel);
		const user = userEvent.setup();
		const row = await screen.findByTestId('outward-grant');
		expect(row.textContent).toContain('ambassador → a@b.com');
		expect(row.textContent).toContain('gmail');
		await user.click(screen.getByRole('button', { name: 'Revoke ambassador sending to a@b.com' }));
		await waitFor(() => expect(screen.queryByTestId('outward-grant')).toBeNull());
		expect(calls.some((call) => call.method === 'DELETE' && call.url.endsWith('/settings/outward-actions/grants/g-1'))).toBe(true);
		expect(screen.getByText('No standing permissions. Choosing Always allow on a captured send adds one here.')).toBeTruthy();
	});

	it('keeps the row and says so when a revoke fails', async () => {
		installFetchMock([
			{ method: 'GET', match: '/outward-actions/grants', handle: () => jsonResponse({ grants: [grant] }) },
			{ method: 'DELETE', match: '/outward-actions/grants/g-1', handle: () => jsonResponse({ message: 'Revoke failed.' }, { status: 500 }) },
			{ method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(envelope) }
		]);
		render(OutwardSettingsPanel);
		const user = userEvent.setup();
		await user.click(await screen.findByRole('button', { name: 'Revoke ambassador sending to a@b.com' }));
		expect((await screen.findByRole('alert')).textContent).toContain('Revoke failed.');
		expect(screen.getByTestId('outward-grant')).toBeTruthy();
	});

	it('shows the empty state when there are none', async () => {
		installFetchMock([
			{ method: 'GET', match: '/outward-actions/grants', handle: () => jsonResponse({ grants: [] }) },
			{ method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(envelope) }
		]);
		render(OutwardSettingsPanel);
		expect(await screen.findByText('No standing permissions. Choosing Always allow on a captured send adds one here.')).toBeTruthy();
		expect(screen.getByRole('heading', { name: 'Always allowed' })).toBeTruthy();
	});

	it('hides the list on a server without grants (404)', async () => {
		installFetchMock([noGrants, { method: 'GET', match: '/settings/outward-actions', handle: () => jsonResponse(envelope) }]);
		render(OutwardSettingsPanel);
		await screen.findByRole('combobox', { name: 'Default sending policy' });
		await waitFor(() => expect(screen.queryByTestId('outward-grants')).toBeNull());
		expect(screen.queryByRole('heading', { name: 'Always allowed' })).toBeNull();
	});
});
