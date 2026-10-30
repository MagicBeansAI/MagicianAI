import { describe, expect, it } from 'vitest';

import { DETAILED_HINT_MIN_CHARS, isDetailedHint } from './hitlDetails';

describe('isDetailedHint', () => {
	it('keeps a short single line plain', () => {
		expect(isDetailedHint('Retry attempt 2. Previous answer: Q1')).toBe(false);
		expect(isDetailedHint('')).toBe(false);
		expect(isDetailedHint(null)).toBe(false);
		expect(isDetailedHint(undefined)).toBe(false);
	});

	it('treats a hint on several lines as details', () => {
		expect(isDetailedHint('Main goal\n- sub goal')).toBe(true);
	});

	it('treats a long single line as details', () => {
		expect(isDetailedHint('x'.repeat(DETAILED_HINT_MIN_CHARS + 1))).toBe(true);
		expect(isDetailedHint('x'.repeat(DETAILED_HINT_MIN_CHARS))).toBe(false);
	});

	it('ignores surrounding whitespace', () => {
		expect(isDetailedHint('  one line  \n')).toBe(false);
	});
});
