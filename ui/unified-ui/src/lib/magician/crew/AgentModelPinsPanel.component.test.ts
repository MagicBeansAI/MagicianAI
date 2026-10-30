import { render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../../test/browser';
import AgentModelPinsPanel from './AgentModelPinsPanel.svelte';

function agentRecord(llmRouting: unknown, version = 6) {
	return {
		definition: { agent_id: 'senior-software-developer', version, llm_routing: llmRouting },
		version,
		etag: `v${version}`
	};
}

const routing = {
	profiles: [
		{ name: 'gpt61sol-responses-vision-toolsany-rhigh-out64k', provider: 'openai', model: 'gpt-6.1-sol', class: 'api', installed: true, selectable: true },
		{ name: 'grok47-responses-vision-toolsany-rhigh', provider: 'xai', model: 'grok-4.7', class: 'api', installed: true, selectable: true },
		{ name: 'chat-openai-adaptive-normal', provider: 'adaptive', model: 'adaptive composite', class: 'api', installed: true, selectable: false }
	],
	models: [
		{ id: 'sol', label: 'GPT Sol', provider: 'openai', connection: 'openai', model: 'gpt-sol', limits: null,
			capabilities: { vision: true, tools: true }, thinking: { mode: 'effort', levels: ['low', 'high'], default: 'low' },
			choices: { auto: 'model/openai/gpt-sol/auto', high: 'model/openai/gpt-sol/high' } },
		{ id: 'grok', label: 'Grok', provider: 'xai', connection: 'xai', model: 'grok-4.7', limits: null,
			capabilities: { vision: true, tools: true }, thinking: { mode: 'effort', levels: ['low', 'high'], default: 'low' },
			choices: { auto: 'model/xai/grok-4.7/auto', high: 'model/xai/grok-4.7/high' } }
	],
	operations: [],
	overrides: {},
	driving_engines: { chat: null, run: null }
};

const coding = {
	models: [{ id: 'sol', engine: 'pi', label: 'GPT Sol coding', provider: 'openai', connection: 'openai', model: 'gpt-sol', limits: null,
		capabilities: { vision: true, tools: true }, thinking: { mode: 'effort', levels: ['low', 'high'], default: 'low' },
		choices: { auto: 'pi/model/sol/auto', low: 'pi/model/sol/low', high: 'pi/model/sol/high' } }],
	engines: [{ id: 'pi', selectable: true, readiness: 'ready' }],
	default_selection: null
};

describe('AgentModelPinsPanel', () => {
	it('shows an unpinned agent following global routing, and saves a pin as a merge patch', async () => {
		const { calls } = installFetchMock([
			{ method: 'GET', match: '/api/magician/v2/agents/senior-software-developer', handle: () => jsonResponse(agentRecord(null)) },
			{ method: 'GET', match: '/api/magician/v2/llm/routing', handle: () => jsonResponse(routing) },
			{ method: 'GET', match: '/api/magician/v2/coding/models', handle: () => jsonResponse(coding) },
			{
				method: 'PATCH',
				match: '/api/magician/v2/agents/senior-software-developer',
				handle: () =>
					jsonResponse(
						agentRecord({ planning: { profile: 'model/xai/grok-4.7/auto', provider: '', model: '' } }, 7)
					)
			}
		]);

		render(AgentModelPinsPanel, { agentId: 'senior-software-developer' });

		const planning = await screen.findByRole('button', { name: 'Decisions & planning' });
		expect(screen.getByText(/no model is pinned/i)).toBeTruthy();
		expect(screen.getByRole('button', { name: 'Coding engine model' })).toBeTruthy();

		const user = userEvent.setup();
		await user.click(planning);
		await user.click(within(screen.getByRole('region', { name: 'Decisions & planning choices' })).getByRole('button', { name: 'Grok' }));
		await user.click(screen.getByRole('button', { name: 'Save pins' }));

		await waitFor(() => expect(calls.some((call) => call.method === 'PATCH')).toBe(true));
		const patch = calls.find((call) => call.method === 'PATCH');
		expect(new Headers(patch?.init?.headers).get('If-Match')).toBe('v6');
		expect(JSON.parse(String(patch?.init?.body))).toEqual({
			llm_routing: {
				planning: { profile: 'model/xai/grok-4.7/auto', provider: '', model: '' }
			}
		});
		expect(await screen.findByText(/1 pin overrides global routing/i)).toBeTruthy();
	});

	it('assigns a model with Auto through the concrete operation handle', async () => {
		const model = {
			id: 'sol', label: 'GPT Sol', provider: 'openai', connection: 'openai', model: 'gpt-sol', limits: null,
			capabilities: { tools: true }, thinking: { mode: 'effort', levels: ['low', 'high'], default: 'low' },
			choices: { auto: 'opaque-auto', low: 'opaque-low', high: 'opaque-high' },
			operation_choices: { auto: 'opaque-initial', low: 'opaque-low', high: 'opaque-high' }
		};
		const { calls } = installFetchMock([
			{ method: 'GET', match: '/api/magician/v2/agents/senior-software-developer', handle: () => jsonResponse(agentRecord({ planning: { profile: routing.profiles[0].name } })) },
			{ method: 'GET', match: '/api/magician/v2/llm/routing', handle: () => jsonResponse({ ...routing, models: [model] }) },
			{ method: 'GET', match: '/api/magician/v2/coding/models', handle: () => jsonResponse(coding) },
			{ method: 'PATCH', match: '/api/magician/v2/agents/senior-software-developer', handle: () => jsonResponse(agentRecord({ planning: { profile: 'opaque-initial' } }, 7)) }
		]);
		render(AgentModelPinsPanel, { agentId: 'senior-software-developer' });
		const user = userEvent.setup();
		const select = await screen.findByRole('button', { name: 'Decisions & planning' });
		expect(select.textContent).toContain('gpt-6.1-sol · openai · saved');
		await user.click(select);
		await user.click(within(screen.getByRole('region', { name: 'Decisions & planning choices' })).getByRole('button', { name: 'GPT Sol' }));
		expect(screen.getByRole('slider', { name: 'Thinking level' })).toBeTruthy();
		await user.click(screen.getByRole('button', { name: 'Save pins' }));
		await waitFor(() => expect(calls.some(call => call.method === 'PATCH')).toBe(true));
		expect(JSON.parse(String(calls.find(call => call.method === 'PATCH')?.init?.body)))
			.toEqual({ llm_routing: { planning: { profile: 'opaque-initial', provider: '', model: '' } } });
	});

	it('clears every pin by removing the routing block', async () => {
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents/principal-software-engineer',
				handle: () => jsonResponse({ ...agentRecord({ coding_profile: 'coding-premium' }), definition: { agent_id: 'principal-software-engineer', llm_routing: { coding_profile: 'coding-premium' } } })
			},
			{ method: 'GET', match: '/api/magician/v2/llm/routing', handle: () => jsonResponse(routing) },
			{ method: 'GET', match: '/api/magician/v2/coding/models', handle: () => jsonResponse(coding) },
			{
				method: 'PATCH',
				match: '/api/magician/v2/agents/principal-software-engineer',
				handle: () => jsonResponse({ definition: { agent_id: 'principal-software-engineer' }, version: 7, etag: 'v7' })
			}
		]);

		render(AgentModelPinsPanel, { agentId: 'principal-software-engineer' });
		await screen.findByLabelText('Coding engine model');
		expect(await screen.findByText(/1 pin overrides/i)).toBeTruthy();

		const user = userEvent.setup();
		await user.click(screen.getByRole('button', { name: 'Clear pins' }));
		await user.click(screen.getByRole('button', { name: 'Save pins' }));

		await waitFor(() => expect(calls.some((call) => call.method === 'PATCH')).toBe(true));
		const patch = calls.find((call) => call.method === 'PATCH');
		expect(JSON.parse(String(patch?.init?.body))).toEqual({ llm_routing: null });
		expect(await screen.findByText(/no model is pinned/i)).toBeTruthy();
	});
});
