import { describe, expect, it } from 'vitest';
import { isComposerReferenceRequestCurrent, type ComposerReferenceRequest } from './composerReferenceGuard';

describe('composer reference request generation', () => {
	it('rejects stale A responses after A -> B -> A navigation', () => {
		const firstA: ComposerReferenceRequest = { generation: 1, sessionId: 'a', scopeKey: 'p:w' };
		const b: ComposerReferenceRequest = { generation: 2, sessionId: 'b', scopeKey: 'p:w' };
		const secondA: ComposerReferenceRequest = { generation: 3, sessionId: 'a', scopeKey: 'p:w' };

		expect(isComposerReferenceRequestCurrent(firstA, 2, 'b', 'p:w')).toBe(false);
		expect(isComposerReferenceRequestCurrent(firstA, 3, 'a', 'p:w')).toBe(false);
		expect(isComposerReferenceRequestCurrent(b, 3, 'a', 'p:w')).toBe(false);
		expect(isComposerReferenceRequestCurrent(secondA, 3, 'a', 'p:w')).toBe(true);
	});

	it('also rejects a response after scope changes at the same generation', () => {
		const request: ComposerReferenceRequest = { generation: 4, sessionId: 'a', scopeKey: 'p:one' };
		expect(isComposerReferenceRequestCurrent(request, 4, 'a', 'p:two')).toBe(false);
	});
});
