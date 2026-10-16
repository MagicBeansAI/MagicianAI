import { describe, expect, it } from 'vitest';

import { countTone, deltaAria, formatSpend, spendTone, withYday } from './pulseFormat';
import { formatDelta } from './pulseQueries';

describe('spendTone', () => {
	it('tints more spend as bad and less spend as good (inverted polarity)', () => {
		expect(spendTone(3.2, 1.1)).toBe('bad');
		expect(spendTone(1.1, 3.2)).toBe('good');
	});

	it('treats sub-cent drift as neutral (formatDelta noise band)', () => {
		expect(spendTone(1.052, 1.05)).toBe('neutral');
		expect(spendTone(1.05, 1.052)).toBe('neutral');
	});

	it('reads spend appearing from a zero yesterday as bad ("new today" is the more-spent side)', () => {
		expect(spendTone(0.42, 0)).toBe('bad');
	});

	it('is neutral on an all-zero pair', () => {
		expect(spendTone(0, 0)).toBe('neutral');
	});

	it('is neutral exactly when formatDelta renders no currency delta', () => {
		const pairs: Array<[number, number]> = [
			[0, 0],
			[1.052, 1.05],
			[3.2, 1.1],
			[1.1, 3.2],
			[0.42, 0],
			[0, 0.42]
		];
		for (const [today, yesterday] of pairs) {
			expect(spendTone(today, yesterday) === 'neutral').toBe(
				formatDelta(today, yesterday, 'currency') === ''
			);
		}
	});
});

describe('countTone', () => {
	it('tints more as good and fewer as bad (normal polarity)', () => {
		expect(countTone(12, 10)).toBe('good');
		expect(countTone(8, 10)).toBe('bad');
	});

	it('treats deltas that round to 0% as neutral (formatDelta noise band)', () => {
		expect(countTone(1001, 1000)).toBe('neutral');
		expect(countTone(1000, 1001)).toBe('neutral');
	});

	it('reads activity appearing from a zero yesterday as good', () => {
		expect(countTone(5, 0)).toBe('good');
	});

	it('is neutral on an all-zero pair', () => {
		expect(countTone(0, 0)).toBe('neutral');
	});

	it('is neutral exactly when formatDelta renders no count delta', () => {
		const pairs: Array<[number, number]> = [
			[0, 0],
			[1001, 1000],
			[12, 10],
			[8, 10],
			[5, 0],
			[0, 5]
		];
		for (const [today, yesterday] of pairs) {
			expect(countTone(today, yesterday) === 'neutral').toBe(
				formatDelta(today, yesterday, 'count') === ''
			);
		}
	});
});

describe('formatSpend', () => {
	it('renders cents below $100 and drops them from $100 up', () => {
		expect(formatSpend(0)).toBe('$0.00');
		expect(formatSpend(12.345)).toBe('$12.35');
		expect(formatSpend(99.99)).toBe('$99.99');
		expect(formatSpend(100)).toBe('$100');
		expect(formatSpend(1234.56)).toBe('$1235');
	});
});

describe('withYday', () => {
	it('suffixes real deltas and passes through quiet/new-today text', () => {
		expect(withYday('')).toBe('');
		expect(withYday('new today')).toBe('new today');
		expect(withYday('+18%')).toBe('+18% vs yday');
	});
});

describe('deltaAria', () => {
	it('builds the accessible-name suffix per delta kind', () => {
		expect(deltaAria('')).toBe('');
		expect(deltaAria('new today')).toBe(', none yesterday');
		expect(deltaAria('+18%')).toBe(', +18% versus yesterday');
	});
});
