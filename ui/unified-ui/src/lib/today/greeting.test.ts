import { describe, expect, it } from 'vitest';

import { greetingFor } from './greeting';

describe('greetingFor', () => {
	// All dates are local-time constructions.

	it('reads as morning from 05:00 through 11:59', () => {
		expect(greetingFor(new Date(2026, 6, 4, 5, 0))).toBe('Good morning');
		expect(greetingFor(new Date(2026, 6, 4, 9, 30))).toBe('Good morning');
		expect(greetingFor(new Date(2026, 6, 4, 11, 59))).toBe('Good morning');
	});

	it('reads as afternoon from 12:00 through 17:59', () => {
		expect(greetingFor(new Date(2026, 6, 4, 12, 0))).toBe('Good afternoon');
		expect(greetingFor(new Date(2026, 6, 4, 17, 59))).toBe('Good afternoon');
	});

	it('reads as evening from 18:00 onward, including the late-night hours before 05:00', () => {
		expect(greetingFor(new Date(2026, 6, 4, 18, 0))).toBe('Good evening');
		expect(greetingFor(new Date(2026, 6, 4, 23, 30))).toBe('Good evening');
		expect(greetingFor(new Date(2026, 6, 4, 0, 30))).toBe('Good evening');
		expect(greetingFor(new Date(2026, 6, 4, 4, 59))).toBe('Good evening');
	});

	it('does not repeat the weekday already rendered in the date line', () => {
		expect(greetingFor(new Date(2026, 6, 6, 9, 0))).toBe('Good morning');
		expect(greetingFor(new Date(2026, 6, 10, 15, 0))).toBe('Good afternoon');
		expect(greetingFor(new Date(2026, 6, 5, 21, 0))).toBe('Good evening');
	});
});
