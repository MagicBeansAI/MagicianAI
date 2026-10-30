import { describe, expect, it } from 'vitest';

import { derivePresence, presenceLabel } from './presence';

const none = { hasPendingHitl: false, liveTurn: false, streaming: false, toolsRunning: false };

describe('derivePresence', () => {
	it('is idle with no signals', () => {
		expect(derivePresence(none)).toBe('idle');
	});

	it('is thinking for a live turn with no streaming text or tools yet', () => {
		expect(derivePresence({ ...none, liveTurn: true })).toBe('thinking');
	});

	it('is working once the live turn streams or runs tools', () => {
		expect(derivePresence({ ...none, liveTurn: true, streaming: true })).toBe('working');
		expect(derivePresence({ ...none, liveTurn: true, toolsRunning: true })).toBe('working');
	});

	it('treats streaming or running tools as working even without a live turn id', () => {
		expect(derivePresence({ ...none, streaming: true })).toBe('working');
		expect(derivePresence({ ...none, toolsRunning: true })).toBe('working');
	});

	it('reads a paused or review-waiting task as needs-you, not working', () => {
		expect(derivePresence({ ...none, liveTurn: true, taskWaitingOnUser: true })).toBe('needs-you');
		expect(derivePresence({ ...none, taskWaitingOnUser: false, toolsRunning: true })).toBe('working');
	});

	it('puts a pending HITL above everything else', () => {
		expect(derivePresence({ hasPendingHitl: true, liveTurn: true, streaming: true, toolsRunning: true })).toBe(
			'needs-you'
		);
		expect(derivePresence({ ...none, hasPendingHitl: true })).toBe('needs-you');
	});
});

describe('presenceLabel', () => {
	it('names each state for assistive tech', () => {
		expect(presenceLabel('idle')).toBe('Magician is idle');
		expect(presenceLabel('thinking')).toBe('Magician is thinking');
		expect(presenceLabel('working')).toBe('Magician is working');
		expect(presenceLabel('needs-you')).toBe('Magician needs you');
	});
});
