import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import { setCurrentScopeBearerToken } from '$lib/stores/scopeIdentityStore';
import PreferenceChecksPanel from './PreferenceChecksPanel.svelte';

afterEach(() => {
	cleanup();
	setCurrentScopeBearerToken(null);
});

const effective = { mode: 'shadow', configured_mode: 'shadow', mode_for_you: 'shadow' };
const NO_NOTES = { private: true, open: 0, notices: [] };

describe('Preference checks', () => {
	it('shows the mode in force beside a rejected save, with its code and property', async () => {
		setCurrentScopeBearerToken('mst_test_preference_checks');
		installFetchMock([
			{
				match: '/preference-alignment/status',
				handle: () =>
					jsonResponse({
						effective,
						saved: {
							status: 'rejected',
							error: {
								code: 'alignment_enforce_route_required',
								property: 'llm.router.operation_mapping.action_alignment_judge',
								reason: 'is required because preference_alignment.mode enforces'
							}
						},
						in_sync: false
					})
			},
			{
				match: '/preference-alignment/policy',
				handle: () => jsonResponse({ status: 'ok', revision: 0, global: null, intentions: {} })
			},
			{ match: '/preference-alignment/notices', handle: () => jsonResponse(NO_NOTES) }
		]);
		render(PreferenceChecksPanel);
		expect(await screen.findByTestId('preference-checks-effective')).toHaveTextContent(
			'Shadow — checks are recorded, nothing is held'
		);
		const rejected = screen.getByTestId('preference-checks-rejected');
		expect(rejected).toHaveTextContent('Not applied.');
		expect(rejected).toHaveTextContent('alignment_enforce_route_required');
		expect(rejected).toHaveTextContent('llm.router.operation_mapping.action_alignment_judge');
	});

	it('saves the policy against the revision it read, and shows a newer one on conflict', async () => {
		setCurrentScopeBearerToken('mst_test_preference_checks');
		let writes = 0;
		const { calls } = installFetchMock([
			{
				match: '/preference-alignment/status',
				handle: () => jsonResponse({ effective, saved: { status: 'accepted' }, in_sync: true })
			},
			{
				method: 'GET',
				match: '/preference-alignment/policy',
				handle: () => jsonResponse({ status: 'ok', revision: 4, global: null, intentions: {} })
			},
			{ match: '/preference-alignment/notices', handle: () => jsonResponse(NO_NOTES) },
			{
				method: 'PUT',
				match: '/preference-alignment/policy',
				handle: () => {
					writes += 1;
					return writes === 1
						? jsonResponse(
								{
									error: 'stale_policy',
									message: 'Your policy changed on another device.',
									current: {
										status: 'ok',
										revision: 5,
										global: 'confirm_before_exception',
										intentions: {}
									}
								},
								{ status: 409 }
							)
						: jsonResponse({ status: 'ok', revision: 6, global: 'advisory', intentions: {} });
				}
			}
		]);
		render(PreferenceChecksPanel);
		const select = await screen.findByTestId('preference-policy-select');
		// The select stays disabled until the saved policy has loaded; a change
		// made before that is overwritten by the load.
		await waitFor(() => expect(select).toBeEnabled());
		expect(select).toHaveValue('');
		await fireEvent.change(select, { target: { value: 'advisory' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Save check policy' }));

		await screen.findByText(/changed on another device/);
		await waitFor(() => expect(select).toHaveValue('confirm_before_exception'));
		const firstPut = calls.find((call) => call.method === 'PUT');
		expect(JSON.parse(String(firstPut?.init?.body))).toMatchObject({
			expected_revision: 4,
			global: 'advisory',
			surface: 'web'
		});

		await fireEvent.change(select, { target: { value: 'advisory' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Save check policy' }));
		await screen.findByText('Saved. The next check uses it.');
		const lastPut = calls.filter((call) => call.method === 'PUT').at(-1);
		expect(JSON.parse(String(lastPut?.init?.body)).expected_revision).toBe(5);
	});

	it('saves a nudge’s remember offer with its token, and only the shown wording', async () => {
		setCurrentScopeBearerToken('mst_test_preference_checks');
		const { calls } = installFetchMock([
			{
				match: '/preference-alignment/status',
				handle: () => jsonResponse({ effective, saved: { status: 'accepted' }, in_sync: true })
			},
			{
				match: '/preference-alignment/policy',
				handle: () => jsonResponse({ status: 'ok', revision: 0, global: null, intentions: {} })
			},
			{
				method: 'GET',
				match: '/preference-alignment/notices',
				handle: () =>
					jsonResponse({
						private: true,
						open: 1,
						notices: [
							{
								id: 'n-1',
								created_at_ms: 1,
								act_summary: 'swiggy order',
								explanation: 'You usually order mild food.',
								intentions: [],
								remember: { statement: 'spice: mild', wording: 'spice: mild', token: 'tok-9' },
								state: 'open'
							}
						]
					})
			},
			{
				method: 'POST',
				match: '/preference-alignment/notices/n-1/answer',
				handle: () => jsonResponse({ id: 'n-1', state: 'remembered' })
			}
		]);
		render(PreferenceChecksPanel);
		expect(await screen.findByText('You usually order mild food.')).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Save this wording' }));
		await waitFor(() => expect(screen.queryByTestId('preference-note')).not.toBeInTheDocument());
		const post = calls.find((call) => call.method === 'POST');
		expect(JSON.parse(String(post?.init?.body))).toEqual({ answer: 'remember', token: 'tok-9' });
	});
});
