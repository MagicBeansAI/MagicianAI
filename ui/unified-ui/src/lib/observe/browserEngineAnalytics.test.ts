import { afterEach, describe, expect, it, vi } from 'vitest';

import { fetchBrowserEngineUsage } from './browserEngineAnalytics';

describe('browser engine analytics API', () => {
	afterEach(() => vi.unstubAllGlobals());

	it('requests an actual server page with filters and page size', async () => {
		const fetchMock = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => ({
				items: [],
				total_count: 0,
				limit: 50,
				offset: 100,
				has_more: false,
				summary: []
			})
		});
		vi.stubGlobal('fetch', fetchMock);

		await fetchBrowserEngineUsage({
			page: 2,
			pageSize: 50,
			engine: 'lightpanda',
			outcome: 'failure'
		});

		const url = new URL(fetchMock.mock.calls[0][0], 'http://localhost');
		expect(url.pathname).toBe('/api/magician/v2/browser/engine-usage');
		expect(url.searchParams.get('limit')).toBe('50');
		expect(url.searchParams.get('offset')).toBe('100');
		expect(url.searchParams.get('engine')).toBe('lightpanda');
		expect(url.searchParams.get('outcome')).toBe('failure');
	});

	it('surfaces the backend error', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue({
				ok: false,
				status: 503,
				json: async () => ({ error: 'analytics unavailable' })
			})
		);
		await expect(fetchBrowserEngineUsage({ page: 0, pageSize: 25 })).rejects.toThrow(
			'analytics unavailable'
		);
	});
});
