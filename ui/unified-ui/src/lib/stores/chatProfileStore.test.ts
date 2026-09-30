import { describe, expect, it } from 'vitest';
import type { ChatProfile } from './chatProfileStore';
import { resolveSelectedChatProfile } from './chatProfileStore';

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

	it('falls back to the configured default when the saved profile disappeared', () => {
		expect(resolveSelectedChatProfile(
			[profile('fast'), profile('deep', true)],
			'removed'
		)).toBe('deep');
	});

	it('uses the first profile when none is marked default', () => {
		expect(resolveSelectedChatProfile([profile('first'), profile('second')], null)).toBe('first');
	});

	it('returns null for an empty catalog', () => {
		expect(resolveSelectedChatProfile([], 'removed')).toBeNull();
	});

	it('matches profile names exactly instead of silently changing case', () => {
		expect(resolveSelectedChatProfile(
			[profile('Precise'), profile('fallback', true)],
			'precise'
		)).toBe('fallback');
	});
});
