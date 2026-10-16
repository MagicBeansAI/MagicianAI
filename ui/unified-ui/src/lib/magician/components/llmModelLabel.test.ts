import { describe, expect, it } from 'vitest';
import { answeredModelLabel, requestedModelLabel } from './llmModelLabel';

describe('llm row model labels', () => {
	it('names the requested model, then the capability, then a generic word', () => {
		expect(requestedModelLabel({ model: 'Claude Code · opus', engine: 'claude_code' })).toBe(
			'Claude Code · opus'
		);
		expect(requestedModelLabel({ capability: 'chat.inline' })).toBe('chat.inline');
		expect(requestedModelLabel({})).toBe('model');
		expect(requestedModelLabel(undefined)).toBe('model');
	});

	it('names the model that answered from the settled event', () => {
		expect(answeredModelLabel({ model: 'claude-opus-5-5', provider: 'anthropic' })).toBe(
			'claude-opus-5-5'
		);
		expect(answeredModelLabel({ harness_model: 'opus' })).toBe('opus');
		expect(answeredModelLabel({ provider: 'anthropic' })).toBe('anthropic');
	});

	it('returns null when the settled event names no model, so the running label stays', () => {
		expect(answeredModelLabel({ duration_ms: 120 })).toBeNull();
		expect(answeredModelLabel({ model: '  ' })).toBeNull();
		expect(answeredModelLabel(undefined)).toBeNull();
	});
});
