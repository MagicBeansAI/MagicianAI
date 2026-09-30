import { beforeEach, describe, expect, it } from 'vitest';

import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { installFetchMock, jsonResponse } from '../../test/browser';
import {
	fetchImpactDashboard,
	fetchImpactReviewArtifact,
	fetchImpactReviewUtility,
	generateImpactReview,
	listImpactReviews,
	publishImpactDashboard,
	recordImpactReviewFeedback,
	reviewRegenerationDefaults
} from './api';

beforeEach(() => {
	scopeIdentityStore.observe('alice', 'work');
});

describe('impact review API', () => {
	it('loads and normalizes the durable review inventory', async () => {
		installFetchMock([
			{
				match: '/evidence/reviews',
				handle: () =>
					jsonResponse({
						reviews: [
							{
								namespace: 'evidence-reviews',
								name: 'work-7d-2026-07-11.md',
								agent: 'analyst',
								last_updated: '2026-07-11T10:00:00Z'
							}
						]
					})
			}
		]);

		expect(await listImpactReviews()).toHaveLength(1);
		installFetchMock([
			{ match: '/evidence/reviews', handle: () => jsonResponse({ reviews: {} }) }
		]);
		expect(await listImpactReviews()).toEqual([]);
	});

	it('generates a scoped review with verification and artifact identity', async () => {
		const { calls } = installFetchMock([
			{
				method: 'POST',
				match: '/evidence/review',
				handle: () =>
					jsonResponse({
						markdown: '# Impact',
						artifact_name: 'work-30d-2026-07-11.md',
						evidence_count: 14,
						verification: {
							grounded: true,
							ungrounded_claims: [],
							citation_coverage: 1,
							uncited_bullets: 0,
							total_bullets: 4,
							cited_ids: ['evidence-1'],
							notes: ''
						}
					})
			}
		]);

		const result = await generateImpactReview({
			agent: 'analyst',
			days: 30,
			facet: 'work'
		});

		expect(result).toMatchObject({
			markdown: '# Impact',
			artifact_name: 'work-30d-2026-07-11.md',
			evidence_count: 14
		});
		expect(result.verification?.grounded).toBe(true);
		expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({
			agent: 'analyst',
			days: 30,
			facet: 'work'
		});
	});

	it('encodes durable artifact names before opening review content', async () => {
		const { calls } = installFetchMock([
			{
				match: '/artifacts/durable/evidence-reviews/',
				handle: () => jsonResponse({ content: '# Stored review' })
			}
		]);

		expect(await fetchImpactReviewArtifact('work/7 day.md')).toBe('# Stored review');
		expect(calls[0]?.url).toContain('work%2F7%20day.md');
	});

	it('records explicit utility verdicts and reads the updated rate', async () => {
		const { calls } = installFetchMock([
			{
				method: 'POST',
				match: '/review/feedback',
				handle: () => jsonResponse({ ok: true })
			},
			{
				method: 'GET',
				match: '/evidence/utility',
				handle: () => jsonResponse({ utility_rate: 0.75 })
			}
		]);

		await recordImpactReviewFeedback('work-7d.md', 'edited');
		expect(await fetchImpactReviewUtility()).toBe(0.75);
		expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({
			review: 'work-7d.md',
			verdict: 'edited'
		});
	});

	it('loads dashboard filters through the query string', async () => {
		const { calls } = installFetchMock([
			{
				match: '/evidence/dashboard?',
				handle: () =>
					jsonResponse({
						total_evidence: 12,
						active_entities: 4,
						facets_tracked: 2,
						reviews_generated: 3,
						facet_coverage: [],
						entity_types: [],
						top_entities: [],
						weekly_activity: [],
						recent_evidence: [],
						visibility_gaps: []
					})
			}
		]);

		expect(
			await fetchImpactDashboard({ agent: 'analyst', facet: 'business', days: 90 })
		).toMatchObject({ total_evidence: 12 });
		expect(calls[0]?.url).toContain('agent=analyst');
		expect(calls[0]?.url).toContain('facet=business');
		expect(calls[0]?.url).toContain('days=90');
	});

	it('publishes dashboards and defaults missing routes to briefing', async () => {
		installFetchMock([
			{
				method: 'POST',
				match: '/dashboard/publish',
				handle: () => jsonResponse({ surface_id: 'surface-1' })
			}
		]);

		expect(
			await publishImpactDashboard({ agent: 'analyst', facet: 'all', days: 7 })
		).toEqual({ surface_id: 'surface-1', route: '/briefing' });
	});

	it('preserves actionable backend errors', async () => {
		installFetchMock([
			{
				match: '/evidence/reviews',
				handle: () => jsonResponse({ message: 'evidence index unavailable' }, { status: 503 })
			}
		]);

		await expect(listImpactReviews()).rejects.toThrow('evidence index unavailable');
	});

	it('recovers facet, window, and agent when regenerating a stored review', () => {
		expect(
			reviewRegenerationDefaults(
				{ name: 'business-30d-2026-07-11.md', agent: 'analyst' },
				7
			)
		).toEqual({ facet: 'business', days: 30, agent: 'analyst' });
		expect(
			reviewRegenerationDefaults({ name: 'unparseable.md', agent: null }, 14)
		).toEqual({ facet: undefined, days: 14, agent: undefined });
	});
});
