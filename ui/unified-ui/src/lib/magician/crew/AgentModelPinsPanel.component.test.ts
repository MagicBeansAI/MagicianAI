import { render, screen, waitFor } from '@testing-library/svelte';
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
	operations: [],
	overrides: {},
	driving_engines: { chat: null, run: null }
};

const coding = {
	profiles: [
		{ id: 'auto', label: 'Auto', is_default: false, supports_user_image_inputs: false },
		{ id: 'coding-premium', label: 'GPT-6.1 Sol coding', is_default: false, supports_user_image_inputs: true }
	]
};

describe('AgentModelPinsPanel', () => {
	it('shows an unpinned agent following global routing, and saves a pin as a merge patch', async () => {
		const { calls } = installFetchMock([
			{ method: 'GET', match: '/api/magician/v2/agents/senior-software-developer', handle: () => jsonResponse(agentRecord(null)) },
			{ method: 'GET', match: '/api/magician/v2/llm/routing', handle: () => jsonResponse(routing) },
			{ method: 'GET', match: '/api/magician/v2/coding/profiles', handle: () => jsonResponse(coding) },
			{
				method: 'PATCH',
				match: '/api/magician/v2/agents/senior-software-developer',
				handle: () =>
					jsonResponse(
						agentRecord({ planning: { profile: 'grok47-responses-vision-toolsany-rhigh', provider: '', model: '' } }, 7)
					)
			}
		]);

		render(AgentModelPinsPanel, { agentId: 'senior-software-developer' });

		const planning = (await screen.findByLabelText('Decisions & planning')) as HTMLSelectElement;
		expect(screen.getByText(/no model is pinned/i)).toBeTruthy();
		const optionValues = Array.from(planning.options).map((option) => option.value);
		expect(optionValues).toContain('grok47-responses-vision-toolsany-rhigh');
		expect(optionValues).not.toContain('chat-openai-adaptive-normal');
		const codingSelect = screen.getByLabelText('Coding engine model') as HTMLSelectElement;
		expect(Array.from(codingSelect.options).map((option) => option.value)).not.toContain('auto');

		const user = userEvent.setup();
		await user.selectOptions(planning, 'grok47-responses-vision-toolsany-rhigh');
		await user.click(screen.getByRole('button', { name: 'Save pins' }));

		await waitFor(() => expect(calls.some((call) => call.method === 'PATCH')).toBe(true));
		const patch = calls.find((call) => call.method === 'PATCH');
		expect(new Headers(patch?.init?.headers).get('If-Match')).toBe('v6');
		expect(JSON.parse(String(patch?.init?.body))).toEqual({
			llm_routing: {
				planning: { profile: 'grok47-responses-vision-toolsany-rhigh', provider: '', model: '' }
			}
		});
		expect(await screen.findByText(/1 pin overrides global routing/i)).toBeTruthy();
	});

	it('clears every pin by removing the routing block', async () => {
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/api/magician/v2/agents/principal-software-engineer',
				handle: () => jsonResponse({ ...agentRecord({ coding_profile: 'coding-premium' }), definition: { agent_id: 'principal-software-engineer', llm_routing: { coding_profile: 'coding-premium' } } })
			},
			{ method: 'GET', match: '/api/magician/v2/llm/routing', handle: () => jsonResponse(routing) },
			{ method: 'GET', match: '/api/magician/v2/coding/profiles', handle: () => jsonResponse(coding) },
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
