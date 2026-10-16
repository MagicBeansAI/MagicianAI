import { describe, expect, it } from 'vitest';

import { tintFor, tintVars } from './tint';

describe('tintFor', () => {
	it('is deterministic', () => {
		expect(tintFor('web-researcher')).toBe(tintFor('web-researcher'));
	});

	it('stays in 1..5, including for the empty string', () => {
		for (const key of ['', 'a', 'b', 'source:example.com', '🦊']) {
			const slot = tintFor(key);
			expect(slot).toBeGreaterThanOrEqual(1);
			expect(slot).toBeLessThanOrEqual(5);
			expect(Number.isInteger(slot)).toBe(true);
		}
	});

	it('spreads 100 distinct keys over all five slots', () => {
		const seen = new Set<number>();
		for (let i = 0; i < 100; i++) seen.add(tintFor(`key-${i}`));
		expect([...seen].sort()).toEqual([1, 2, 3, 4, 5]);
	});
});

describe('tintVars', () => {
	it('returns a style string pointing at the key slot', () => {
		const n = tintFor('artifact.pdf');
		expect(tintVars('artifact.pdf')).toBe(
			`--cn-tint: var(--tint-${n}); --cn-tint-ink: var(--tint-${n}-ink);`
		);
	});
});
