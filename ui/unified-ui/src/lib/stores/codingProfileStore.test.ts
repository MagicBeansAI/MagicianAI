import { describe, expect, it } from 'vitest';
import {
	AUTO_CODING_CHOICE_ID,
	blockedCodingProfiles,
	codingChoiceFromSelection,
	codingEngineNote,
	codingGatewaySummary,
	codingProfileOptionLabel,
	draftInvokesVibedev,
	groupCodingProfilesByEngine,
	pickerCodingProfiles,
	selectableCodingProfiles,
	decodeSavedCodingSelection,
	resolveSelectedCodingProfile,
	savedCodingSelection,
	type CodingProfile
} from './codingProfileStore';
import type { ModelChoice } from '$lib/llm/modelCatalog';

function row(id: string, extra: Partial<CodingProfile> = {}): CodingProfile {
	return {
		id,
		label: id,
		supports_user_image_inputs: false,
		is_default: false,
		...extra
	};
}

describe('selectableCodingProfiles', () => {
	it('keeps legacy Pi rows that omit selectable', () => {
		const rows = selectableCodingProfiles([row('coding-balanced')]);
		expect(rows.map((profile) => profile.id)).toEqual(['coding-balanced']);
	});

	it('hides a Codex row until it is selectable', () => {
		const rows = selectableCodingProfiles([
			row('coding-balanced'),
			row('codex-default', {
				engine: 'codex_app_server',
				selectable: false,
				readiness: 'disabled'
			})
		]);
		expect(rows.map((profile) => profile.id)).toEqual(['coding-balanced']);
	});

	it('shows a ready Codex row without inventing Auto at the API filter', () => {
		const rows = selectableCodingProfiles([
			row('coding-balanced', { is_default: true }),
			row('codex-default', {
				engine: 'codex_app_server',
				selectable: true,
				readiness: 'ready'
			})
		]);
		expect(rows.map((profile) => profile.id)).toEqual(['coding-balanced', 'codex-default']);
		expect(rows.some((profile) => profile.id === AUTO_CODING_CHOICE_ID)).toBe(false);
	});

	it('hides grok-default until it is selectable', () => {
		const hidden = selectableCodingProfiles([
			row('coding-balanced'),
			row('grok-default', {
				engine: 'grok_acp',
				selectable: false,
				readiness: 'disabled'
			})
		]);
		expect(hidden.map((profile) => profile.id)).toEqual(['coding-balanced']);

		const shown = selectableCodingProfiles([
			row('coding-balanced', { is_default: true }),
			row('grok-default', {
				engine: 'grok_acp',
				selectable: true,
				readiness: 'ready'
			})
		]);
		expect(shown.map((profile) => profile.id)).toEqual(['coding-balanced', 'grok-default']);
	});

	it('hides claude-default until it is selectable', () => {
		const hidden = selectableCodingProfiles([
			row('coding-balanced'),
			row('claude-default', {
				engine: 'claude_code',
				selectable: false,
				readiness: 'disabled'
			})
		]);
		expect(hidden.map((profile) => profile.id)).toEqual(['coding-balanced']);

		const shown = selectableCodingProfiles([
			row('coding-balanced', { is_default: true }),
			row('claude-default', {
				engine: 'claude_code',
				selectable: true,
				readiness: 'ready'
			})
		]);
		expect(shown.map((profile) => profile.id)).toEqual(['coding-balanced', 'claude-default']);
	});

	it('hides agy-default until it is selectable', () => {
		const hidden = selectableCodingProfiles([
			row('coding-balanced'),
			row('agy-default', {
				engine: 'agy_cli',
				selectable: false,
				readiness: 'disabled'
			})
		]);
		expect(hidden.map((profile) => profile.id)).toEqual(['coding-balanced']);

		const shown = selectableCodingProfiles([
			row('coding-balanced', { is_default: true }),
			row('agy-default', {
				engine: 'agy_cli',
				selectable: true,
				readiness: 'ready'
			})
		]);
		expect(shown.map((profile) => profile.id)).toEqual(['coding-balanced', 'agy-default']);
	});
});

describe('pickerCodingProfiles', () => {
	it('appends Auto after named rows and never makes it the default', () => {
		const rows = pickerCodingProfiles([
			row('coding-balanced', { is_default: true, supports_user_image_inputs: true }),
			row('codex-default', {
				engine: 'codex_app_server',
				selectable: true,
				readiness: 'ready'
			}),
			row('grok-default', {
				engine: 'grok_acp',
				selectable: true,
				readiness: 'ready'
			}),
			row('claude-default', {
				engine: 'claude_code',
				selectable: true,
				readiness: 'ready'
			}),
			row('agy-default', {
				engine: 'agy_cli',
				selectable: true,
				readiness: 'ready'
			})
		]);
		expect(rows.map((profile) => profile.id)).toEqual([
			'coding-balanced',
			'codex-default',
			'grok-default',
			'claude-default',
			'agy-default',
			AUTO_CODING_CHOICE_ID
		]);
		const auto = rows.find((profile) => profile.id === AUTO_CODING_CHOICE_ID);
		expect(auto?.is_default).toBe(false);
		expect(auto?.supports_user_image_inputs).toBe(true);
	});

	it('hides Auto when no named profile is selectable', () => {
		expect(
			pickerCodingProfiles([
				row('codex-default', { selectable: false, readiness: 'disabled' })
			])
		).toEqual([]);
	});
});

describe('codingChoiceFromSelection', () => {
	it('sends Auto as a kind and refuses unknown explicit model choices', () => {
		expect(codingChoiceFromSelection(AUTO_CODING_CHOICE_ID, [])).toEqual({ kind: 'auto' });
		expect(() => codingChoiceFromSelection('coding-balanced', [])).toThrow('unavailable');
		expect(codingChoiceFromSelection(null, [])).toBeNull();
	});
});

describe('typed coding preferences', () => {
	const model: ModelChoice = {
		id: 'provider/model', model: 'physical-model', label: 'Model', provider: 'provider', connection: 'provider',
		thinking: { mode: 'effort', levels: ['low', 'high'], default: 'low' }, limits: null,
		capabilities: { vision: true, tools: true, streaming: true, computer_use: false, structured_output: false, web_search: false },
		choices: { auto: 'canonical-auto', high: 'canonical-high' }
	};
	const profiles = [
		row('canonical-auto', { engine: 'pi', is_default: true, model_choice: model, model_selection: { model_id: model.id, thinking: 'auto' } }),
		row('canonical-high', { engine: 'pi', model_selection: { model_id: model.id, thinking: 'high' } }),
		row('old-coding-pin', { engine: 'pi', model_selection: { model_id: model.id, thinking: 'high' } })
	];
	it('migrates old IDs once and restores model/thinking after aliases are removed', () => {
		const migrated = resolveSelectedCodingProfile(profiles, 'old-coding-pin', null);
		expect(migrated).toBe('canonical-high');
		const saved = savedCodingSelection(profiles, migrated);
		expect(saved).toEqual({ kind: 'model', engine: 'pi', model_id: model.id, thinking: 'high' });
		expect(resolveSelectedCodingProfile(profiles.slice(0, 2), null, saved)).toBe('canonical-high');
	});
	it('retains missing choices and never switches engines to recover one', () => {
		expect(resolveSelectedCodingProfile(profiles, 'retired-unknown', null)).toBe('retired-unknown');
		expect(resolveSelectedCodingProfile(profiles, 'canonical-auto', { kind: 'model', engine: 'claude_code', model_id: model.id, thinking: 'high' })).toBe('__unavailable_coding_selection__');
		expect(resolveSelectedCodingProfile(profiles, 'canonical-auto', { kind: 'model', engine: 'pi', model_id: model.id, thinking: 'none' })).toBe('__unavailable_coding_selection__');
	});
	it('converts returning Pi and native CLI preferences without legacy rows', () => {
		const sol = { ...model, id: 'openai/gpt-6.1-sol' };
		const rows = profiles.slice(0, 2).map(profile => ({ ...profile,
			model_choice: profile.model_choice ? sol : null,
			model_selection: { model_id: sol.id, thinking: profile.model_selection!.thinking }
		}));
		expect(resolveSelectedCodingProfile(rows, 'coding-premium', null)).toBe('canonical-high');
		expect(resolveSelectedCodingProfile(rows, 'coding-premium', { kind: 'auto' })).toBe(AUTO_CODING_CHOICE_ID);
		expect(resolveSelectedCodingProfile(rows.slice(0, 1), 'coding-premium', null)).toBe('__unavailable_coding_selection__');
		for (const [old, engine, connection] of [
			['codex-default', 'codex_app_server', 'harness-codex'],
			['claude-default', 'claude_code', 'harness-claude_code'],
			['grok-default', 'grok_acp', 'harness-grok'],
			['agy-default', 'agy_cli', 'harness-agy']
		]) {
			const native = { ...model, id: `native/${engine}/${connection}/default` };
			const entries = [row('canonical-auto', { engine, model_choice: native }), row('canonical-high', { engine })];
			expect(resolveSelectedCodingProfile(entries, old, null)).toBe('canonical-auto');
			expect(resolveSelectedCodingProfile(rows, old, null)).toBe('__unavailable_coding_selection__');
		}
	});
	it('preserves opt-in Auto distinctly from a model with Auto thinking', () => {
		expect(savedCodingSelection(profiles, AUTO_CODING_CHOICE_ID)).toEqual({ kind: 'auto' });
		expect(resolveSelectedCodingProfile(profiles, null, { kind: 'auto' })).toBe(AUTO_CODING_CHOICE_ID);
		expect(savedCodingSelection(profiles, 'canonical-auto')).toMatchObject({ kind: 'model', thinking: 'auto' });
	});
	it('rejects malformed typed preferences without interpreting them as defaults', () => {
		expect(decodeSavedCodingSelection('{"kind":"auto"}')).toEqual({ kind: 'auto' });
		const saved = savedCodingSelection(profiles, 'canonical-high');
		expect(decodeSavedCodingSelection(JSON.stringify(saved))).toEqual(saved);
		for (const raw of ['broken', '{}', '{"kind":"model","engine":"pi","model_id":"provider/model"}', '{"kind":"model","engine":"pi","model_id":"provider/model","thinking":""}']) {
			expect(decodeSavedCodingSelection(raw)).toBeNull();
		}
	});
});

describe('draftInvokesVibedev', () => {
	it('recognizes an explicit @vibedev draft and ignores spoken-looking prose', () => {
		expect(draftInvokesVibedev('@vibedev fix the footer')).toBe(true);
		expect(draftInvokesVibedev('@vibedev #discuss the layout')).toBe(true);
		expect(draftInvokesVibedev('please start a vibedev build')).toBe(false);
	});
});

describe('groupCodingProfilesByEngine', () => {
	it('labels config profiles as Pi and keeps Auto last and ungrouped', () => {
		const groups = groupCodingProfilesByEngine([
			row('coding-balanced', { engine: 'pi' }),
			row('codex-default', { engine: 'codex_app_server' }),
			row('coding-grok47', { engine: 'pi' }),
			row('claude-default', { engine: 'claude_code' }),
			row(AUTO_CODING_CHOICE_ID)
		]);
		expect(groups.map((group) => group.label)).toEqual(['Pi', 'Codex', 'Claude Code', null]);
		// A Pi row running a Grok model stays under Pi, not the Grok engine.
		expect(groups[0].profiles.map((profile) => profile.id)).toEqual([
			'coding-balanced',
			'coding-grok47'
		]);
		expect(groups[3].profiles.map((profile) => profile.id)).toEqual([AUTO_CODING_CHOICE_ID]);
	});

	it('shows an unknown engine id as sent', () => {
		expect(groupCodingProfilesByEngine([row('x', { engine: 'future_cli' })])[0].label).toBe(
			'future_cli'
		);
	});
});

describe('blocked coding engines', () => {
	it('keeps blocked rows out of the picker and names why they are blocked', () => {
		const rows = [
			row('coding-balanced', { engine: 'pi' }),
			row('grok-default', {
				label: 'Grok',
				engine: 'grok_acp',
				selectable: false,
				reason: 'sign in required'
			}),
			row('codex-default', { label: 'Codex', engine: 'codex_app_server', selectable: false })
		];
		expect(pickerCodingProfiles(rows).map((profile) => profile.id)).not.toContain('grok-default');
		expect(blockedCodingProfiles(rows).map((profile) => profile.id)).toEqual([
			'grok-default',
			'codex-default'
		]);
		expect(codingProfileOptionLabel(rows[1])).toBe('Grok (unavailable: sign in required)');
		expect(codingProfileOptionLabel(rows[2])).toBe('Codex (unavailable)');
		expect(codingProfileOptionLabel(rows[0])).toBe('coding-balanced');
	});
});

describe('engine notes and gateway disclosure', () => {
	it('names the login each engine runs on', () => {
		expect(codingEngineNote('claude_code')).toBe('Claude Code with your Claude login.');
		expect(codingEngineNote('pi')).toContain('You pick the model');
		expect(codingEngineNote('unknown')).toBeNull();
	});

	it('lists what a gateway reaches and never claims an unlisted reach is empty', () => {
		expect(codingGatewaySummary({ mcp_servers: [], plugins: [], listed: true })).toContain(
			'MCP servers: none · plugins: none'
		);
		const named = codingGatewaySummary({ mcp_servers: ['github', 'linear'], plugins: ['x'], listed: true });
		expect(named).toContain('MCP servers: github, linear · plugins: x');
		expect(named).toContain('xAI account (not listable)');
		expect(codingGatewaySummary({ mcp_servers: [], plugins: [], listed: false })).toContain(
			'unknown (listing failed)'
		);
	});
});
