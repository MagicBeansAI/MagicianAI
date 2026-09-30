import { describe, expect, it } from 'vitest';
import {
	AUTO_CODING_CHOICE_ID,
	blockedCodingProfiles,
	codingChoiceFromSelection,
	codingProfileOptionLabel,
	draftInvokesVibedev,
	groupCodingProfilesByEngine,
	pickerCodingProfiles,
	selectableCodingProfiles,
	type CodingProfile
} from './codingProfileStore';

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
	it('sends Auto as a kind, never as a profile id', () => {
		expect(codingChoiceFromSelection(AUTO_CODING_CHOICE_ID)).toEqual({ kind: 'auto' });
		expect(codingChoiceFromSelection('coding-balanced')).toEqual({
			kind: 'profile',
			profile_id: 'coding-balanced'
		});
		expect(codingChoiceFromSelection(null)).toBeNull();
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
