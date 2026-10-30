import { describe, expect, it } from 'vitest';
import type { ChatProfile } from './chatProfileStore';
import { decodeSavedModelSelection, resolveSelectedChatProfile, UNAVAILABLE_MODEL_SELECTION } from './chatProfileStore';
import type { ModelChoice } from '$lib/llm/modelCatalog';

function profile(name: string, isDefault = false): ChatProfile {
	return {
		name,
		provider: 'ollama',
		model: `${name}-model`,
		is_default: isDefault,
		supports_user_image_inputs: false
	};
}

describe('chat profile selection', () => {
	it('preserves a valid user selection even when another profile is default', () => {
		expect(resolveSelectedChatProfile(
			[profile('fast', true), profile('deep')],
			'deep'
		)).toBe('deep');
	});

	it('preserves a missing saved selector so the composer reports it instead of silently changing models', () => {
		expect(resolveSelectedChatProfile(
			[profile('fast'), profile('deep', true)],
			'removed'
		)).toBe('removed');
	});

	it('uses the first profile when none is marked default', () => {
		expect(resolveSelectedChatProfile([profile('first'), profile('second')], null)).toBe('first');
	});

	it('keeps an unavailable selection through an empty catalog and leaves a fresh selection empty', () => {
		expect(resolveSelectedChatProfile([], 'removed')).toBe('removed');
		expect(resolveSelectedChatProfile([], null)).toBeNull();
	});

	it('matches profile names exactly instead of silently changing case', () => {
		expect(resolveSelectedChatProfile(
			[profile('Precise'), profile('fallback', true)],
			'precise'
		)).toBe('precise');
	});

	const choice: ModelChoice = {
		id: 'provider/model', label: 'Model', model: 'physical-model', provider: 'provider', connection: 'provider', limits: null,
		capabilities: { vision: true, tools: true, streaming: true, computer_use: false, structured_output: false, web_search: false },
		thinking: { mode: 'effort', levels: ['low', 'high'], default: 'low' },
		choices: { auto: 'canonical-auto', low: 'canonical-low', high: 'canonical-high' }
	};
	const catalogProfiles: ChatProfile[] = [
		{ ...profile('canonical-auto', true), model_choice: choice, model_selection: { model_id: choice.id, thinking: 'auto' } },
		{ ...profile('canonical-high'), model_selection: { model_id: choice.id, thinking: 'high' } },
		{ ...profile('old-fixed-profile'), model_selection: { model_id: choice.id, thinking: 'high' } }
	];

	it('migrates a legacy selection to its model choice while preserving fixed High', () => {
		expect(resolveSelectedChatProfile(catalogProfiles, 'old-fixed-profile')).toBe('canonical-high');
	});

	it('restores a typed saved choice after every old alias has been removed', () => {
		expect(resolveSelectedChatProfile(catalogProfiles.slice(0, 2), null, [choice], { model_id: choice.id, thinking: 'high' })).toBe('canonical-high');
	});

	it('converts an offline old selection even when the server no longer returns aliases', () => {
		const sol = { ...choice, id: 'openai/gpt-6.1-sol' };
		const rows = catalogProfiles.slice(0, 2).map(row => ({ ...row,
			model_choice: row.model_choice ? sol : null,
			model_selection: { model_id: sol.id, thinking: row.model_selection!.thinking }
		}));
		const old = 'chat-gpt61sol-responses-vision-toolsauto-thinking';
		expect(resolveSelectedChatProfile(rows, old, [sol])).toBe('canonical-high');
		expect(resolveSelectedChatProfile(rows, old, [sol], { model_id: sol.id, thinking: 'auto' })).toBe('canonical-auto');
		expect(resolveSelectedChatProfile(rows.slice(0, 1), old, [sol])).toBe(UNAVAILABLE_MODEL_SELECTION);
		for (const unknown of ['op-embedding-local', '__proto__', 'constructor']) {
			expect(resolveSelectedChatProfile(rows, unknown, [sol])).toBe(unknown);
		}
	});

	it('refuses to substitute the default for an unavailable typed choice', () => {
		expect(resolveSelectedChatProfile(catalogProfiles, 'canonical-auto', [choice], { model_id: 'removed-model', thinking: 'high' })).toBe(UNAVAILABLE_MODEL_SELECTION);
	});

	it('decodes only complete typed preferences', () => {
		expect(decodeSavedModelSelection('{"model_id":"provider/model","thinking":"high"}')).toEqual({ model_id: choice.id, thinking: 'high' });
		for (const raw of ['broken', '{}', '{"model_id":1,"thinking":"low"}', '{"model_id":"provider/model","thinking":""}']) {
			expect(decodeSavedModelSelection(raw)).toBeNull();
		}
	});
});
