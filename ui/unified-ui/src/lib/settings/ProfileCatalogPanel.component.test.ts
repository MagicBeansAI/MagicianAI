import { render, screen } from '@testing-library/svelte';
import { expect, it } from 'vitest';
import { installFetchMock, jsonResponse } from '../../test/browser';
import ProfileCatalogPanel from './ProfileCatalogPanel.svelte';

it('lists each model once and combines operation usage across thinking variants and old aliases', async () => {
	installFetchMock([{ method: 'GET', match: '/llm/routing', handle: () => jsonResponse({
		models: [{ id: 'sol', label: 'GPT Sol', provider: 'openai', connection: 'openai', model: 'gpt-sol',
			limits: { context_window_tokens: 1_000_000 }, thinking: { mode: 'effort', levels: ['low', 'high'], default: 'low' },
			choices: { auto: 'opaque-auto', low: 'opaque-low', high: 'opaque-high' } }],
		profiles: [
			{ name: 'old-out64k', model_selection: { model_id: 'sol', thinking: 'low' } },
			{ name: 'opaque-high', model_selection: { model_id: 'sol', thinking: 'high' } }
		],
		operations: [
			{ operation: 'a', effective_profile: 'old-out64k', description: '', group: 'Other', default_profile: 'old-out64k' },
			{ operation: 'b', effective_profile: 'opaque-high', description: '', group: 'Other', default_profile: 'opaque-high' }
		], overrides: {}
	}) }]);
	render(ProfileCatalogPanel);
	expect(await screen.findByText('GPT Sol')).toBeTruthy();
	expect(screen.getByText('1 of 1 models')).toBeTruthy();
	expect(screen.getByText('2 operations')).toBeTruthy();
	expect(screen.getByText('1M context · Thinking: Low / High')).toBeTruthy();
	expect(screen.queryByText('old-out64k')).toBeNull();
});

it('does not present dispatch rows as model choices when catalog metadata is unavailable', async () => {
    installFetchMock([{ method: 'GET', match: '/llm/routing', handle: () => jsonResponse({
        profiles: [{ name: 'retired-output-profile', provider: 'openai', model: 'physical-model',
            class: 'api', installed: true }], operations: [], overrides: {}
    }) }]);
    render(ProfileCatalogPanel);
    expect(await screen.findByText('Model choices are unavailable.')).toBeTruthy();
    expect(screen.getByText('0 of 0 models')).toBeTruthy();
    expect(screen.queryByText('retired-output-profile')).toBeNull();
});
