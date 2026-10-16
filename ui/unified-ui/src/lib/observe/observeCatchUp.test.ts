import { afterEach, describe, expect, it, vi } from 'vitest';
import { fetchObserveCatchUp, saveObserveCatchUp, type ObserveCatchUpPolicy } from './observeCatchUp';

afterEach(() => vi.unstubAllGlobals());

const policy: ObserveCatchUpPolicy = {
	schema_version: 1,
	revision: 4,
	enabled: true,
	lookback_days: 7,
	max_items_per_source: 50,
	max_total_items: 200,
	max_duration_minutes: 15
};

describe('Observe startup catch-up API', () => {
	it('loads the scoped status and fixed choices from one endpoint', async () => {
		const fetchMock = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => ({ status: { policy, phase: 'waiting', sources: [] }, options: {} })
		});
		vi.stubGlobal('fetch', fetchMock);

		await fetchObserveCatchUp();
		expect(fetchMock).toHaveBeenCalledWith(
			'/api/magician/v2/observe/catch-up',
			expect.objectContaining({ headers: expect.any(Headers) })
		);
		const headers = fetchMock.mock.calls[0][1].headers as Headers;
		expect(headers.get('X-Principal')).toBeNull();
		expect(headers.get('X-Workspace')).toBeNull();
	});

	it('uses the observed revision and sends only policy fields', async () => {
		const fetchMock = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => ({ status: { policy: { ...policy, revision: 5 } }, options: {} })
		});
		vi.stubGlobal('fetch', fetchMock);

		await saveObserveCatchUp(policy);
		const init = fetchMock.mock.calls[0][1] as RequestInit;
		expect(init.method).toBe('PUT');
		expect(JSON.parse(String(init.body))).toEqual({
			expected_revision: 4,
			enabled: true,
			lookback_days: 7,
			max_items_per_source: 50,
			max_total_items: 200,
			max_duration_minutes: 15
		});
	});

	it('surfaces revision conflicts instead of silently overwriting', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue({
				ok: false,
				status: 409,
				json: async () => ({ error: 'Observe catch-up policy changed' })
			})
		);

		await expect(saveObserveCatchUp(policy)).rejects.toThrow('Observe catch-up policy changed');
	});

	it('reports an unreachable backend as Magician being offline', async () => {
		vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('Failed to fetch')));

		await expect(fetchObserveCatchUp()).rejects.toThrow(
			'Magician is offline. Start it and try again.'
		);
		await expect(saveObserveCatchUp(policy)).rejects.toThrow(
			'Magician is offline. Start it and try again.'
		);
	});
});
