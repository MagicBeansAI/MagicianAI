import { cleanup, render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import { installFetchMock, jsonResponse } from '../../test/browser';
import ModelRoutingPanel from './ModelRoutingPanel.svelte';

type EngineFollow = 'parent' | 'pinned';

interface OverviewShape {
	effective?: string;
	overridden?: boolean;
	engine?: EngineFollow;
	engineSource?: 'config' | 'override';
	driving?: { chat: string | null; run: string | null };
}

function overview({
	effective = 'op-screen-luna',
	overridden = false,
	engine = 'parent',
	engineSource = 'config',
	driving = { chat: null, run: null }
}: OverviewShape = {}) {
	const chatProfile = driving.chat ? `op-harness-${driving.chat}` : null;
	const runProfile = driving.run ? `op-harness-${driving.run}` : null;
	const aParentDrives = Boolean(chatProfile || runProfile);
	return {
		affinity: driving.chat ?? driving.run ?? null,
		affinity_profile: chatProfile ?? runProfile,
		affinity_scope: 'flow',
		driving_engines: driving,
		locality: 'cloud',
		rule: 'override > parent > config',
		overrides: overridden ? { screen_observation: effective } : {},
		engine_pins: engineSource === 'override' ? { screen_observation: engine } : {},
		profiles: [
			{
				name: 'op-screen-local',
				provider: 'ollama',
				model: 'qwen3-vl',
				class: 'local',
				installed: true,
				selectable: true
			},
			{
				name: 'op-screen-luna',
				provider: 'openai',
				model: 'gpt-6-luna',
				class: 'api',
				installed: true,
				selectable: true
			},
			{
				name: 'op-screen-terra',
				provider: 'openai',
				model: 'gpt-5.6-terra',
				class: 'api',
				installed: true,
				selectable: true
			},
			{
				name: 'op-harness-codex',
				provider: 'harness-codex',
				model: 'default',
				class: 'harness',
				installed: true,
				selectable: true
			},
			{
				name: 'op-task-summary',
				provider: 'openai',
				model: 'gpt-5.6-summary',
				class: 'api',
				installed: true,
				selectable: true
			}
		],
		operations: [
			{
				operation: 'screen_observation',
				group: 'Vision',
				description: 'Narrates meaningful changes in periodically captured screen frames.',
				configured_selector: {
					default: 'op-screen-local',
					when_cloud: 'op-screen-luna'
				},
				default_profile: 'op-screen-local',
				configured_profile: 'op-screen-luna',
				effective_profile: effective,
				routing_source: overridden ? 'override' : 'config',
				overridden,
				stale_override: false,
				engine,
				engine_source: engineSource,
				follows_parent: false,
				local_floor: true,
				parent_profiles: { chat: chatProfile, run: runProfile }
			},
			{
				operation: 'task_summary',
				group: 'Summaries',
				description: 'Summarises a finished task for its owner.',
				configured_selector: { default: 'op-task-summary' },
				default_profile: 'op-task-summary',
				configured_profile: 'op-task-summary',
				effective_profile: 'op-task-summary',
				routing_source: aParentDrives ? 'parent' : 'config',
				overridden: false,
				stale_override: false,
				engine: 'parent',
				engine_source: 'config',
				follows_parent: true,
				local_floor: false,
				parent_profiles: { chat: chatProfile, run: runProfile }
			}
		]
	};
}

function engineRule(operation: string): HTMLElement {
	return screen.getByRole('group', { name: `Parent engine rule for ${operation}` });
}

afterEach(() => cleanup());

describe('ModelRoutingPanel', () => {
	it('shows purpose, conditional mappings, actual model, and the flow-scoped rule', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/llm/routing',
				handle: () => jsonResponse(overview())
			}
		]);

		render(ModelRoutingPanel);

		expect(await screen.findByText('screen_observation')).toBeTruthy();
		expect(screen.getByText('Narrates meaningful changes in periodically captured screen frames.')).toBeTruthy();
		expect(screen.getByText('openai · gpt-6-luna')).toBeTruthy();
		expect(screen.getByText(/follow the engine that starts each flow/)).toBeTruthy();
		expect(screen.queryByTestId('driving-engines')).toBeNull();
		expect(screen.getByLabelText('Configured mappings for screen_observation').textContent).toContain(
			'Cloud · op-screen-luna'
		);
	});

	it('applies a profile override immediately and offers automatic routing again', async () => {
		let effective = 'op-screen-luna';
		let overridden = false;
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/llm/routing',
				handle: () => jsonResponse(overview({ effective, overridden }))
			},
			{
				method: 'PUT',
				match: '/llm/routing/screen_observation',
				handle: (call) => {
					const body = JSON.parse(String(call.init?.body)) as { profile: string };
					effective = body.profile;
					overridden = true;
					return jsonResponse({ effective_profile: effective, overridden: true });
				}
			}
		]);
		const user = userEvent.setup();

		const { container } = render(ModelRoutingPanel);
		await screen.findByText('screen_observation');
		const select = container.querySelector('#profile-screen_observation');
		if (!(select instanceof HTMLSelectElement)) throw new Error('the profile select renders');
		await user.selectOptions(select, 'op-screen-terra');

		await waitFor(() => expect(screen.getByText('Use automatic routing')).toBeTruthy());
		expect(screen.getByText('openai · gpt-5.6-terra')).toBeTruthy();
		expect(calls.some((call) => call.method === 'PUT')).toBe(true);
	});

	it('renders the Parent/Pinned toggle with the current state and the local floor', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/llm/routing',
				handle: () => jsonResponse(overview({ engine: 'pinned', engineSource: 'override' }))
			}
		]);

		render(ModelRoutingPanel);
		await screen.findByText('screen_observation');

		const rule = engineRule('screen_observation');
		expect(within(rule).getByRole('button', { name: 'Parent' }).getAttribute('aria-pressed')).toBe(
			'false'
		);
		expect(within(rule).getByRole('button', { name: 'Pinned' }).getAttribute('aria-pressed')).toBe(
			'true'
		);
		expect(within(rule).getByText('local default — never follows')).toBeTruthy();
		expect(within(rule).getByRole('button', { name: 'Use config engine rule' })).toBeTruthy();

		const follows = engineRule('task_summary');
		expect(within(follows).getByRole('button', { name: 'Parent' }).getAttribute('aria-pressed')).toBe(
			'true'
		);
		expect(within(follows).queryByRole('button', { name: 'Use config engine rule' })).toBeNull();
	});

	it('pins an operation through the engine route and refreshes', async () => {
		let engine: EngineFollow = 'parent';
		let engineSource: 'config' | 'override' = 'config';
		const { calls } = installFetchMock([
			{
				method: 'GET',
				match: '/llm/routing',
				handle: () => jsonResponse(overview({ engine, engineSource }))
			},
			{
				method: 'PUT',
				match: '/llm/routing/screen_observation/engine',
				handle: (call) => {
					const body = JSON.parse(String(call.init?.body)) as { engine: EngineFollow };
					engine = body.engine;
					engineSource = 'override';
					return jsonResponse({
						operation: 'screen_observation',
						engine,
						engine_source: 'override'
					});
				}
			}
		]);
		const user = userEvent.setup();

		render(ModelRoutingPanel);
		await screen.findByText('screen_observation');
		await user.click(within(engineRule('screen_observation')).getByRole('button', { name: 'Pinned' }));

		await waitFor(() =>
			expect(
				within(engineRule('screen_observation'))
					.getByRole('button', { name: 'Pinned' })
					.getAttribute('aria-pressed')
			).toBe('true')
		);
		const pin = calls.find((call) => call.method === 'PUT');
		expect(pin?.url).toContain('/llm/routing/screen_observation/engine');
		expect(JSON.parse(String(pin?.init?.body))).toEqual({ engine: 'pinned' });
		expect(calls.filter((call) => call.method === 'GET').length).toBeGreaterThanOrEqual(2);
	});

	it('names the driving engines and the profile a following operation would ride', async () => {
		installFetchMock([
			{
				method: 'GET',
				match: '/llm/routing',
				handle: () => jsonResponse(overview({ driving: { chat: 'codex', run: null } }))
			}
		]);

		render(ModelRoutingPanel);
		await screen.findByText('task_summary');

		expect(screen.getByTestId('driving-engines').textContent).toContain('Chat: codex · Run: magician');
		expect(screen.getByText('Parent engine')).toBeTruthy();
		expect(screen.getByText('Follows the engine that starts each flow')).toBeTruthy();
		expect(within(engineRule('task_summary')).getByText('would ride Chat → op-harness-codex')).toBeTruthy();
	});
});
