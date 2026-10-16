import { get } from 'svelte/store';
import { beforeEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse, textResponse } from '../../test/browser';
import {
	clearMagicianConfigError,
	magicianConfigReloadResult,
	magicianConfigStoreState,
	reloadMagicianConfig
} from './magicianConfigStore';

beforeEach(() => clearMagicianConfigError());

describe('magicianConfigStore', () => {
	it('reloads live-safe settings and records restart-required sections', async () => {
		installFetchMock([
			{
				method: 'POST',
				match: '/settings/magician-config/reload',
				handle: () =>
					jsonResponse({
						path: '/notes/magician-config.yaml',
						profile_count: 8,
						operation_mapping_count: 12,
						live_reloaded: ['routing', 'model_preferences'],
						restart_required: ['resource_authority'],
						warnings: ['restart recommended']
					})
			}
		]);

		const result = await reloadMagicianConfig();

		expect(result).toMatchObject({
			profile_count: 8,
			operation_mapping_count: 12,
			live_reloaded: ['routing', 'model_preferences'],
			restart_required: ['resource_authority']
		});
		expect(get(magicianConfigReloadResult)).toEqual(result);
		expect(get(magicianConfigStoreState)).toMatchObject({
			isReloading: false,
			error: null
		});
		expect(get(magicianConfigStoreState).lastReloadedAt).toEqual(expect.any(Number));
	});

	it('rejects malformed success payloads instead of publishing partial state', async () => {
		installFetchMock([
			{
				method: 'POST',
				match: '/settings/magician-config/reload',
				handle: () => jsonResponse({ path: '/notes/magician-config.yaml' })
			}
		]);

		await expect(reloadMagicianConfig()).rejects.toThrow(
			'Malformed magician config reload response'
		);
		expect(get(magicianConfigStoreState)).toMatchObject({
			isReloading: false,
			error: 'Malformed magician config reload response'
		});
	});

	it('surfaces structured and plain-text backend failures', async () => {
		installFetchMock([
			{
				method: 'POST',
				match: '/settings/magician-config/reload',
				handle: () =>
					textResponse(JSON.stringify({ error: 'invalid routing profile' }), {
						status: 422
					})
			}
		]);

		await expect(reloadMagicianConfig()).rejects.toThrow('invalid routing profile');
		expect(get(magicianConfigStoreState).error).toContain('invalid routing profile');
	});
});
