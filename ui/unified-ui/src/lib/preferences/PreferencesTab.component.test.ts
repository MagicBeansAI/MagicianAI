import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import { setCurrentScopeBearerToken } from '$lib/stores/scopeIdentityStore';
import PreferencesTab from './PreferencesTab.svelte';

afterEach(() => {
	cleanup();
	setCurrentScopeBearerToken(null);
});

const row = (id: string, kind: string, status: string, statement: string, revision = 'r1') => ({
	id,
	revision,
	kind,
	assertion_status: status,
	statement,
	lifecycle: 'active',
	source: { native_tier: { tier: 'preferences' } },
	editable: true,
	grounds: []
});

const VIEW = {
	intentions: [
		row('codes', 'standing_instruction', 'direct_owner_statement', 'codes: Ask me before giving out door codes'),
		row('coffee', 'taste', 'inferred', 'coffee: decaf after 3pm')
	],
	retired: [{ id: 'old', statement: 'spicy: no', lifecycle: 'superseded', superseded_by: 'x', revision: 'r0' }],
	omitted: [],
	coverage: { status: 'complete' },
	policy_revision: 0
};

describe('Preferences tab', () => {
	it('groups preferences by kind and says how strong each is', async () => {
		setCurrentScopeBearerToken('mst_test_preferences');
		installFetchMock([{ match: '/preference-alignment/intentions', handle: () => jsonResponse(VIEW) }]);
		render(PreferencesTab);
		const rules = await screen.findByTestId('preferences-standing_instruction');
		expect(rules).toHaveTextContent('Ask me before giving out door codes');
		expect(within(rules).getByTestId('preference-status')).toHaveTextContent('You said it');
		const tastes = screen.getByTestId('preferences-taste');
		expect(within(tastes).getByTestId('preference-status')).toHaveTextContent('Inferred from your activity');
		// Only an inferred preference can be confirmed.
		expect(within(tastes).getByRole('button', { name: 'Confirm' })).toBeInTheDocument();
		expect(within(rules).queryByRole('button', { name: 'Confirm' })).toBeNull();
	});

	it('reloads and says so when an edit finds the preference changed', async () => {
		setCurrentScopeBearerToken('mst_test_preferences');
		let reads = 0;
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/preference-alignment/intentions',
				handle: () => {
					reads += 1;
					return jsonResponse(VIEW);
				}
			},
			{
				method: 'PATCH',
				match: '/preference-alignment/intentions/codes',
				handle: () =>
					jsonResponse(
						{ error: 'stale_preference', current_revision: 'r2', message: 'This preference changed since you opened it.' },
						{ status: 409 }
					)
			}
		]);
		render(PreferencesTab);
		const rules = await screen.findByTestId('preferences-standing_instruction');
		await fireEvent.click(within(rules).getByRole('button', { name: 'Edit' }));
		const input = within(rules).getByTestId('preference-edit-input');
		await fireEvent.input(input, { target: { value: 'Ask me before any door code' } });
		await fireEvent.click(within(rules).getByRole('button', { name: 'Save' }));
		expect(await screen.findByTestId('preferences-notice')).toHaveTextContent(
			'This preference changed since you opened it. The current version is shown.'
		);
		const patch = calls.find((call) => call.method === 'PATCH');
		expect(JSON.parse(String(patch?.init?.body))).toMatchObject({ expected_revision: 'r1' });
		await waitFor(() => expect(reads).toBe(2));
	});

	it('asks before forgetting, then forgets with the revision on screen', async () => {
		setCurrentScopeBearerToken('mst_test_preferences');
		const { calls } = installFetchMock([
			{ method: 'GET', match: '/preference-alignment/intentions', handle: () => jsonResponse(VIEW) },
			{ method: 'DELETE', match: '/preference-alignment/intentions/coffee', handle: () => jsonResponse({ record_id: 'coffee' }) }
		]);
		render(PreferencesTab);
		const tastes = await screen.findByTestId('preferences-taste');
		await fireEvent.click(within(tastes).getByTestId('preference-forget'));
		expect(within(tastes).getByRole('alert')).toHaveTextContent('erased from the history');
		expect(calls.some((call) => call.method === 'DELETE')).toBe(false);
		await fireEvent.click(within(tastes).getByTestId('preference-forget-confirm'));
		await waitFor(() => expect(calls.some((call) => call.method === 'DELETE')).toBe(true));
		expect(calls.find((call) => call.method === 'DELETE')?.url).toContain('expected_revision=r1');
	});

	it('shows a forgotten entry in the history without its wording', async () => {
		setCurrentScopeBearerToken('mst_test_preferences');
		installFetchMock([
			{
				match: '/intentions/codes/history',
				handle: () =>
					jsonResponse({
						entries: [
							{ at_ms: 2, record_id: 'codes', op: 'edited', actor: { via: 'owner_surface', surface: 'ios' }, wording_erased: true }
						]
					})
			},
			{ method: 'GET', match: '/preference-alignment/intentions', handle: () => jsonResponse(VIEW) }
		]);
		render(PreferencesTab);
		const rules = await screen.findByTestId('preferences-standing_instruction');
		await fireEvent.click(within(rules).getByRole('button', { name: 'History' }));
		const entry = await screen.findByTestId('preference-history-entry');
		expect(entry).toHaveTextContent('Reworded');
		expect(entry).toHaveTextContent('by you on iPhone');
		expect(entry).toHaveTextContent('wording erased');
	});
});
