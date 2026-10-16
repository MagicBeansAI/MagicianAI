import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../../../test/browser';
import Page from './+page.svelte';

afterEach(() => cleanup());

describe('model-routing settings page', () => {
	it('renders the routing order and operation controls on a dedicated page', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/llm/routing',
				handle: () =>
					jsonResponse({
						affinity: null,
						affinity_profile: null,
						affinity_scope: 'process',
						locality: 'cloud',
						rule: 'override > engine > config',
						overrides: {},
						profiles: [
							{
								name: 'gpt6luna-responses-toolsany',
								provider: 'openai',
								model: 'gpt-6-luna',
								class: 'api',
								installed: true,
								selectable: true
							}
						],
						operations: [
							{
								operation: 'query_analysis',
								group: 'Planning & execution',
								description: 'Analyzes request intent and complexity.',
								configured_selector: 'gpt6luna-responses-toolsany',
								default_profile: 'gpt6luna-responses-toolsany',
								configured_profile: 'gpt6luna-responses-toolsany',
								effective_profile: 'gpt6luna-responses-toolsany',
								routing_source: 'config',
								overridden: false,
								via_affinity: false,
								stale_override: false
							}
						]
					})
			}
		]);

		render(Page);

		expect(screen.getByRole('heading', { level: 1, name: 'Model routing' })).toBeTruthy();
		expect(screen.getByText('What happens when an engine changes')).toBeTruthy();
		expect(screen.getByText(/Codex App Server bridges/)).toBeTruthy();
		expect(await screen.findByText('query_analysis')).toBeTruthy();
	});
});
