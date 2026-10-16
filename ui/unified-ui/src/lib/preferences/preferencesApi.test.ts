import { afterEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import { setCurrentScopeBearerToken } from '$lib/stores/scopeIdentityStore';
import { editPreference, forgetPreference, parseHistory, parseView } from './preferencesApi';

afterEach(() => setCurrentScopeBearerToken(null));

describe('preferences API', () => {
	it('reads the view as checks see it, with the tier as the source', () => {
		const view = parseView({
			intentions: [
				{
					id: 'a',
					revision: 'r1',
					kind: 'standing_instruction',
					assertion_status: 'direct_owner_statement',
					statement: 'codes: Ask me before giving out door codes',
					lifecycle: 'active',
					source: { native_tier: { tier: 'preferences' } },
					editable: true,
					last_checked_at_ms: 5,
					grounds: [{ role: 'owner', source_id: 'turn-1' }]
				}
			],
			retired: [{ id: 'z', statement: 'old', lifecycle: 'superseded', superseded_by: 'a', revision: 'r0' }],
			omitted: [{ id: 'w', reason: 'unreadable_validity' }],
			coverage: { status: 'partial' },
			policy_revision: 3
		});
		expect(view.preferences[0]).toMatchObject({
			id: 'a',
			kind: 'standing_instruction',
			status: 'direct_owner_statement',
			source: 'preferences',
			editable: true,
			lastCheckedAtMs: 5
		});
		expect(view.retired[0].supersededBy).toBe('a');
		expect(view.omitted[0].reason).toBe('unreadable_validity');
		expect(view.coverage).toBe('partial');
	});

	it('keeps an erased entry erased', () => {
		const [entry] = parseHistory({
			entries: [{ at_ms: 9, record_id: 'a', op: 'forgotten', actor: { via: 'owner_surface', surface: 'web' }, wording_erased: true }]
		});
		expect(entry).toMatchObject({ op: 'forgotten', surface: 'web', wordingErased: true, wordingAfter: null });
	});

	it('sends the revision on screen and reports a stale one', async () => {
		setCurrentScopeBearerToken('mst_test_preferences');
		const { calls } = installFetchMock([
			{
				method: 'PATCH',
				match: '/preference-alignment/intentions/a',
				handle: () =>
					jsonResponse(
						{ error: 'stale_preference', current_revision: 'r2', message: 'This preference changed.' },
						{ status: 409 }
					)
			},
			{
				method: 'DELETE',
				match: '/preference-alignment/intentions/a',
				handle: () => jsonResponse({ record_id: 'a' })
			}
		]);
		const outcome = await editPreference('a', 'r1', 'No red meat');
		expect(outcome).toEqual({ status: 'stale', currentRevision: 'r2', message: 'This preference changed.' });
		expect(JSON.parse(String(calls[0].init?.body))).toEqual({
			action: 'edit',
			expected_revision: 'r1',
			wording: 'No red meat'
		});
		expect(calls[0].url).toContain('surface=web');
		expect(await forgetPreference('a', 'r1')).toEqual({ status: 'applied', recordId: 'a' });
		expect(calls[1].url).toContain('expected_revision=r1');
	});
});
