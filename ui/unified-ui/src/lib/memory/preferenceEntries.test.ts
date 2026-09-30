import { describe, expect, it, vi } from 'vitest';

import {
	MEMORY_ENTRIES_ENDPOINT,
	MEMORY_EFFECT_REVIEW_ENDPOINT,
	PREFERENCE_ENTRY_TIERS,
	PREFERENCE_PAGE_SIZE,
	PreferenceEntriesApiError,
	clampPreferencePageIndex,
	fetchPreferenceEntriesPage,
	memoryTabFromQuery,
	parsePreferenceEntriesPage,
	preferenceConfirmPath,
	preferenceEntriesRequest,
	preferenceKeepConflictPath,
	preferencePageCount,
	preferencePagerView,
	preferenceScopePath,
	memoryEffectAdvanceLabel,
	memoryEffectReviewCanAdvance,
	memoryEffectReviewShowsBanner
} from './preferenceEntries';

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json' }
	});
}

describe('preferenceEntriesRequest', () => {
	it('asks for the owner-facing confirmable tiers on the first page', () => {
		const request = preferenceEntriesRequest(0);
		const params = new URL(request.url, 'http://localhost').searchParams;
		expect(request.url.startsWith(`${MEMORY_ENTRIES_ENDPOINT}?`)).toBe(true);
		expect(params.get('offset')).toBe('0');
		expect(params.get('limit')).toBe(String(PREFERENCE_PAGE_SIZE));
		expect(params.get('tiers')).toBe(PREFERENCE_ENTRY_TIERS.join(','));
		expect(request.tiers).toBe('preferences,research_findings');
	});

	it('advances offset by page size without changing the tier allowlist', () => {
		const request = preferenceEntriesRequest(2);
		const params = new URL(request.url, 'http://localhost').searchParams;
		expect(params.get('offset')).toBe(String(2 * PREFERENCE_PAGE_SIZE));
		expect(params.get('limit')).toBe('20');
		expect(params.get('tiers')).toBe('preferences,research_findings');
	});
});

describe('preference pager math', () => {
	it('hides the pager for a single page even when the store is empty', () => {
		expect(preferencePageCount(0)).toBe(1);
		expect(preferencePagerView(0, 0, 0)).toEqual({
			page: 0,
			pageCount: 1,
			startItem: 0,
			endItem: 0
		});
	});

	it('shows a second page once the confirmable list exceeds the page size', () => {
		expect(preferencePageCount(21)).toBe(2);
		expect(preferencePagerView(1, 21, 1)).toEqual({
			page: 1,
			pageCount: 2,
			startItem: 21,
			endItem: 21
		});
	});

	it('clamps a stale page after the list shrinks', () => {
		expect(clampPreferencePageIndex(4, 21)).toBe(1);
		expect(clampPreferencePageIndex(-1, 21)).toBe(0);
	});
});

describe('parsePreferenceEntriesPage', () => {
	it('keeps well-formed confirmable rows and falls back to the loaded count', () => {
		const parsed = parsePreferenceEntriesPage({
			entries: [
				{
					tier: 'preferences',
					key: 'avoid_vendor_calls',
					source_type: 'insight',
					trust: 'inferred',
					kind: 'normative',
					value: 'Avoid vendor calls',
					scope: { topics: ['vendor'], entities: [], applies_to: [] }
				},
				{ tier: 'accounts', key: 'skip-me' }
			]
		});
		expect(parsed.entries).toHaveLength(1);
		expect(parsed.entries[0]).toMatchObject({
			key: 'avoid_vendor_calls',
			scope: { topics: ['vendor'] }
		});
		expect(parsed.total).toBe(1);
	});

	it('trusts the server total when it is a finite number', () => {
		expect(parsePreferenceEntriesPage({ entries: [], total: 41 }).total).toBe(41);
	});

	it('keeps conflict counts only when the list JSON sent finite numbers', () => {
		const parsed = parsePreferenceEntriesPage({
			entries: [
				{
					tier: 'preferences',
					key: 'avoid_vendor_calls',
					source_type: 'insight',
					trust: 'stated',
					kind: 'normative',
					conflict: 'still applying preferences: avoid_vendor_calls',
					conflict_agree: 1,
					conflict_disagree: 4
				}
			]
		});
		expect(parsed.entries[0]).toMatchObject({
			conflict: 'still applying preferences: avoid_vendor_calls',
			conflict_agree: 1,
			conflict_disagree: 4
		});
	});
});

describe('fetchPreferenceEntriesPage', () => {
	it('loads the requested page without a second fetch when the page is in range', async () => {
		const fetchImpl = vi.fn().mockResolvedValue(
			jsonResponse({
				entries: [
					{
						tier: 'research_findings',
						key: 'r1',
						source_type: 'insight',
						trust: 'stated',
						kind: 'factual'
					}
				],
				total: 1
			})
		);
		const page = await fetchPreferenceEntriesPage(0, { fetchImpl });
		expect(fetchImpl).toHaveBeenCalledTimes(1);
		expect(String(fetchImpl.mock.calls[0][0])).toContain('tiers=preferences%2Cresearch_findings');
		expect(page).toMatchObject({ page: 0, total: 1, entries: [{ key: 'r1' }] });
	});

	it('refetches the last valid page when the current index is past the end', async () => {
		const fetchImpl = vi
			.fn()
			.mockResolvedValueOnce(jsonResponse({ entries: [], total: 21 }))
			.mockResolvedValueOnce(
				jsonResponse({
					entries: [
						{
							tier: 'preferences',
							key: 'last',
							source_type: 'insight',
							trust: 'inferred',
							kind: 'normative'
						}
					],
					total: 21
				})
			);
		const page = await fetchPreferenceEntriesPage(4, { fetchImpl });
		expect(fetchImpl).toHaveBeenCalledTimes(2);
		expect(String(fetchImpl.mock.calls[0][0])).toContain('offset=80');
		expect(String(fetchImpl.mock.calls[1][0])).toContain('offset=20');
		expect(page.page).toBe(1);
		expect(page.entries[0]?.key).toBe('last');
	});

	it('surfaces a failed list read without inventing entries', async () => {
		const fetchImpl = vi.fn().mockResolvedValue(jsonResponse({ error: 'nope' }, 503));
		await expect(
			fetchPreferenceEntriesPage(0, {
				fetchImpl,
				readError: async () => 'list unavailable'
			})
		).rejects.toMatchObject({
			name: 'PreferenceEntriesApiError',
			message: 'list unavailable',
			status: 503
		} satisfies Partial<PreferenceEntriesApiError>);
	});
});

describe('memoryTabFromQuery', () => {
	it('opens the Preferences pager from the user-memory tab query', () => {
		expect(memoryTabFromQuery('user-memory')).toBe('user-memory');
	});

	it('falls back to overview for a missing or unknown tab', () => {
		expect(memoryTabFromQuery(null)).toBe('overview');
		expect(memoryTabFromQuery('accounts')).toBe('overview');
	});
});

describe('preference mutation paths', () => {
	it('encodes tier and key on confirm, keep-conflict, and scope routes', () => {
		expect(preferenceConfirmPath('preferences', 'avoid/vendor')).toBe(
			'/api/magician/v2/memory/entries/preferences/avoid%2Fvendor/confirm'
		);
		expect(preferenceKeepConflictPath('preferences', 'avoid/vendor')).toBe(
			'/api/magician/v2/memory/entries/preferences/avoid%2Fvendor/keep-conflict'
		);
		expect(preferenceScopePath('research_findings', 'r 1')).toBe(
			'/api/magician/v2/memory/entries/research_findings/r%201/scope'
		);
	});
});

describe('memory effect review banner', () => {
	it('hides while collecting shadow evidence', () => {
		expect(
			memoryEffectReviewShowsBanner({
				compiled_mode: 'shadow',
				effective_mode: 'shadow',
				pending_hitl: false,
				advice: 'collect_shadow_evidence',
				reason: 'none',
				next_step: 'wait'
			})
		).toBe(false);
		expect(MEMORY_EFFECT_REVIEW_ENDPOINT).toBe('/api/magician/v2/memory/effect-review');
	});

	it('offers Canary/Enforced only on advance advice', () => {
		expect(memoryEffectReviewCanAdvance('advance_to_canary')).toBe(true);
		expect(memoryEffectAdvanceLabel('advance_to_canary')).toBe('Switch to Canary');
		expect(memoryEffectReviewCanAdvance('investigate_attachment')).toBe(false);
		expect(
			memoryEffectReviewShowsBanner({
				compiled_mode: 'shadow',
				effective_mode: 'shadow',
				pending_hitl: true,
				advice: 'advance_to_canary',
				reason: 'ready',
				next_step: 'accept HITL'
			})
		).toBe(true);
	});
});
