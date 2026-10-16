import { describe, expect, it } from 'vitest';
import { chatHitlSource } from './escalationRouting';

describe('chatHitlSource', () => {
	it('routes planning clarifications through the clarification dispatcher', () => {
		expect(chatHitlSource('clarification', undefined)).toBe('clarification');
	});

	it('keeps service-backed requests and generic pauses on their canonical sources', () => {
		expect(chatHitlSource('clarification', 'request-1')).toBe('user_request');
		expect(chatHitlSource('cannot_proceed', undefined)).toBe('escalation');
	});
});
