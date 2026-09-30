import { afterEach, describe, expect, it, vi } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import { fetchRoutingOverview } from './modelRoutingStore';

afterEach(() => vi.restoreAllMocks());

describe('modelRoutingStore', () => {
	it('normalizes an older routing overview during a rolling backend restart', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/llm/routing',
				handle: () =>
					jsonResponse({
						affinity: 'codex',
						affinity_profile: 'op-harness-codex',
						profiles: [],
						overrides: {},
						rule: 'legacy',
						operations: [
							{
								operation: 'query_analysis',
								default_profile: 'gpt6luna-responses-toolsany',
								effective_profile: 'op-harness-codex',
								overridden: false,
								via_affinity: true
							}
						]
					})
			}
		]);

		const overview = await fetchRoutingOverview();
		expect(overview.operations[0]).toMatchObject({
			group: 'Other',
			description: 'Runs the query analysis LLM operation.',
			configured_selector: 'gpt6luna-responses-toolsany',
			configured_profile: 'gpt6luna-responses-toolsany',
			routing_source: 'parent',
			engine: 'parent',
			engine_source: 'config',
			follows_parent: false,
			parent_profiles: { chat: null, run: null }
		});
		expect(overview.driving_engines).toEqual({ chat: null, run: null });
	});
});
