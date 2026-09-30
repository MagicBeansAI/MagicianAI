import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	deleteObservationSubscription,
	fetchObservableSources,
	fetchObservationRunHistory,
	fetchObservationSourceObservability,
	fetchObservationSubscriptions,
	putObservationSubscription,
	runObservationSubscription
} from './sourceApi';

afterEach(() => vi.unstubAllGlobals());

describe('observable source API', () => {
	it('uses server filters and cursor pagination for source offers', async () => {
		const fetchMock = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => ({ items: [], total: 0, catalog_revision: 'r', manifest_issues: [] })
		});
		vi.stubGlobal('fetch', fetchMock);
		await fetchObservableSources({
			readiness: 'eligible',
			requiredAction: 'rss.discover',
			subscribed: false,
			cursor: 'source:profile',
			limit: 5
		});
		const url = String(fetchMock.mock.calls[0][0]);
		expect(url).toContain('readiness=eligible');
		expect(url).toContain('required_action=rss.discover');
		expect(url).toContain('subscribed=false');
		expect(url).toContain('cursor=source%3Aprofile');
	});

	it('keeps subscription listing and mutations on the scoped server contract', async () => {
		const fetchMock = vi.fn().mockImplementation(async (url: string, init?: RequestInit) => ({
			ok: true,
			status: init?.method === 'DELETE' ? 204 : 200,
			json: async () =>
				url.includes('/subscriptions?')
					? { items: [], total: 0 }
					: { subscription_id: 'obs_1' }
		}));
		vi.stubGlobal('fetch', fetchMock);
		await fetchObservationSubscriptions({ enabled: true, cursor: 'obs_0', limit: 5 });
		await putObservationSubscription('auto', {
			source_id: 'arxiv-ai',
			profile_id: 'observe-rss',
			enabled: true,
			cadence: 'hourly'
		});
		await runObservationSubscription('obs_1');
		await deleteObservationSubscription('obs_1');

		expect(String(fetchMock.mock.calls[0][0])).toContain('enabled=true');
		expect(fetchMock.mock.calls[1][1]?.method).toBe('PUT');
		expect(fetchMock.mock.calls[2][1]?.method).toBe('POST');
		expect(fetchMock.mock.calls[3][1]?.method).toBe('DELETE');
	});

	it('preserves stale revision errors for refresh recovery', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue({
				ok: false,
				status: 409,
				json: async () => ({ error: 'stale_source', message: 'refresh and retry' })
			})
		);
		await expect(
			putObservationSubscription('obs_1', {
				source_id: 'arxiv-ai',
				profile_id: 'observe-rss',
				enabled: true,
				cadence: 'daily'
			})
		).rejects.toMatchObject({ code: 'stale_source', status: 409 });
	});

	it('uses independent server cursors for observability summaries and run history', async () => {
		const fetchMock = vi.fn().mockResolvedValue({
			ok: true,
			status: 200,
			json: async () => ({ items: [], total: 0 })
		});
		vi.stubGlobal('fetch', fetchMock);

		await fetchObservationSourceObservability({ cursor: 'source-page', limit: 5 });
		await fetchObservationRunHistory('obs/source 1', { cursor: 'run-page', limit: 5 });

		expect(String(fetchMock.mock.calls[0][0])).toContain(
			'/observe/sources/observability?cursor=source-page&limit=5'
		);
		expect(String(fetchMock.mock.calls[1][0])).toContain(
			'/observe/subscriptions/obs%2Fsource%201/runs?cursor=run-page&limit=5'
		);
	});
});
