// The hook's pure math — the part that decides which role is on screen at
// any elapsed time. Wrong here means the wrong word is showing, silently,
// on every visit.
import { describe, expect, it } from 'vitest';

import { cycleIndex, HOLD_MS, ROLES } from './roleCycle';

describe('ROLES', () => {
	it('ends on the small, specific role rather than the expected ones', () => {
		expect(ROLES[ROLES.length - 1]).toBe('someone who still checks the score');
	});

	it('leads with the role that sets the expected pattern', () => {
		expect(ROLES[0]).toBe('a professional');
	});
});

describe('cycleIndex', () => {
	it('holds the first role for the full HOLD_MS window', () => {
		expect(cycleIndex(0, ROLES.length)).toBe(0);
		expect(cycleIndex(HOLD_MS - 1, ROLES.length)).toBe(0);
	});

	it('advances to the next role the instant HOLD_MS elapses', () => {
		expect(cycleIndex(HOLD_MS, ROLES.length)).toBe(1);
		expect(cycleIndex(HOLD_MS * 2, ROLES.length)).toBe(2);
	});

	it('wraps back to 0 after a full pass through every role', () => {
		expect(cycleIndex(HOLD_MS * ROLES.length, ROLES.length)).toBe(0);
		expect(cycleIndex(HOLD_MS * (ROLES.length + 1), ROLES.length)).toBe(1);
	});

	it('degrades a non-positive count to 0 rather than NaN', () => {
		expect(cycleIndex(HOLD_MS * 3, 0)).toBe(0);
		expect(cycleIndex(HOLD_MS * 3, -1)).toBe(0);
		expect(Number.isNaN(cycleIndex(HOLD_MS * 3, 0))).toBe(false);
	});
});
