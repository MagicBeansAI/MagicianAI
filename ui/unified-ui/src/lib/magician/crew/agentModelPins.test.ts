import { describe, expect, it } from 'vitest';

import {
	buildModelPinPatch,
	countModelPins,
	modelPinStateFromDefinition,
	modelPinStatesEqual,
	pinnableProfiles,
	type ModelPinProfileOption
} from './agentModelPins';

describe('agent model pins', () => {
	it('reads an unpinned agent as having no pins', () => {
		const state = modelPinStateFromDefinition({ agent_id: 'writer' });
		expect(countModelPins(state)).toBe(0);
		expect(state.lanes.planning).toBe('');
		expect(state.coding_profile).toBe('');
	});

	it('reads lane, coding and per-operation pins off a definition', () => {
		const state = modelPinStateFromDefinition({
			llm_routing: {
				planning: { profile: 'gpt61sol-responses-vision-toolsany-rhigh-out64k', provider: '', model: '' },
				evaluation: null,
				coding_profile: 'coding-premium',
				operations: { agentic_decision: { profile: 'grok47-responses-vision-toolsany-rhigh' } }
			}
		});
		expect(state.lanes.planning).toBe('gpt61sol-responses-vision-toolsany-rhigh-out64k');
		expect(state.lanes.evaluation).toBe('');
		expect(state.coding_profile).toBe('coding-premium');
		expect(state.operations).toEqual({ agentic_decision: 'grok47-responses-vision-toolsany-rhigh' });
		expect(countModelPins(state)).toBe(3);
	});

	it('removes the whole routing block when every pin is cleared', () => {
		const saved = modelPinStateFromDefinition({ llm_routing: { coding_profile: 'coding-premium' } });
		const cleared = modelPinStateFromDefinition({});
		expect(buildModelPinPatch(saved, cleared)).toEqual({ llm_routing: null });
	});

	it('sends only the lanes that changed: set ones as profile endpoints, cleared ones as null', () => {
		const saved = modelPinStateFromDefinition({
			llm_routing: { evaluation: { profile: 'x' }, memory_consolidation: { profile: 'y' } }
		});
		const next = modelPinStateFromDefinition({
			llm_routing: { evaluation: { profile: 'x' }, memory_consolidation: { profile: 'y' } }
		});
		next.lanes.planning = 'grok47-responses-vision-toolsany-rhigh';
		next.lanes.memory_consolidation = '';
		next.coding_profile = 'coding-premium';
		expect(buildModelPinPatch(saved, next)).toEqual({
			llm_routing: {
				planning: { profile: 'grok47-responses-vision-toolsany-rhigh', provider: '', model: '' },
				memory_consolidation: null,
				coding_profile: 'coding-premium'
			}
		});
	});

	it('keeps a direct provider/model pin through an unrelated save, and can clear it', () => {
		const definition = {
			llm_routing: { evaluation: { profile: null, provider: 'openai', model: 'gpt-6.1-sol' } }
		};
		const saved = modelPinStateFromDefinition(definition);
		expect(saved.lanes.evaluation).toBe('direct:openai/gpt-6.1-sol');
		expect(countModelPins(saved)).toBe(1);

		const next = modelPinStateFromDefinition(definition);
		next.lanes.planning = 'grok47-responses-vision-toolsany-rhigh';
		const patch = buildModelPinPatch(saved, next) as { llm_routing: Record<string, unknown> };
		expect(patch.llm_routing).not.toHaveProperty('evaluation');

		const cleared = modelPinStateFromDefinition(definition);
		cleared.lanes.evaluation = '';
		expect(buildModelPinPatch(saved, cleared)).toEqual({ llm_routing: null });
	});

	it('compares pin states by value', () => {
		const a = modelPinStateFromDefinition({ llm_routing: { planning: { profile: 'x' } } });
		const b = modelPinStateFromDefinition({ llm_routing: { planning: { profile: 'x' } } });
		expect(modelPinStatesEqual(a, b)).toBe(true);
		b.lanes.planning = 'y';
		expect(modelPinStatesEqual(a, b)).toBe(false);
	});

	it('offers only concrete, selectable profiles, API profiles first', () => {
		const profiles: ModelPinProfileOption[] = [
			{ name: 'op-harness-codex', provider: 'harness-codex', model: 'default', class: 'harness', installed: true, selectable: true },
			{ name: 'chat-openai-adaptive-normal', provider: 'adaptive', model: 'adaptive composite', class: 'api', installed: true, selectable: false },
			{ name: 'local-gemma', provider: 'ollama', model: 'gemma', class: 'local', installed: true, selectable: true },
			{ name: 'grok47-responses-vision-toolsany-rhigh', provider: 'xai', model: 'grok-4.7', class: 'api', installed: true, selectable: true }
		];
		expect(pinnableProfiles(profiles).map((profile) => profile.name)).toEqual([
			'grok47-responses-vision-toolsany-rhigh',
			'local-gemma',
			'op-harness-codex'
		]);
	});
});
