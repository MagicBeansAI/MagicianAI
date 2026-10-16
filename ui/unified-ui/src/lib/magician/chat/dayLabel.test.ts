import { describe, expect, it } from 'vitest';

import { dayKey, dayLabel, dayStarts } from './dayLabel';

const now = new Date(2026, 9, 1, 15, 0).getTime(); // Thu 1 Oct 2026, 15:00 local
const at = (y: number, m: number, d: number, h = 10) => new Date(y, m, d, h).getTime();

describe('dayLabel', () => {
	it('names today and yesterday', () => {
		expect(dayLabel(at(2026, 9, 1, 0), now)).toBe('Today');
		expect(dayLabel(at(2026, 8, 30, 23), now)).toBe('Yesterday');
	});

	it('uses the weekday within the last week', () => {
		expect(dayLabel(at(2026, 8, 27), now)).toBe(
			new Date(2026, 8, 27).toLocaleDateString(undefined, { weekday: 'long' })
		);
	});

	it('uses a short date beyond a week, with the year only for other years', () => {
		expect(dayLabel(at(2026, 8, 1), now)).not.toMatch(/2026/);
		expect(dayLabel(at(2025, 11, 31), now)).toMatch(/2025/);
	});
});

describe('dayStarts', () => {
	it('marks the first message of each local day', () => {
		const items = [
			{ id: 'a', createdAt: at(2026, 8, 30, 9) },
			{ id: 'b', createdAt: at(2026, 8, 30, 18) },
			{ id: 'c', createdAt: at(2026, 9, 1, 8) },
			{ id: 'd', createdAt: at(2026, 9, 1, 9) }
		];
		expect([...dayStarts(items, now)]).toEqual([
			['a', 'Yesterday'],
			['c', 'Today']
		]);
		expect(dayKey(items[2].createdAt)).toBe('2026-10-01');
	});

	it('skips messages without a usable timestamp', () => {
		expect([...dayStarts([{ id: 'x', createdAt: 0 }], now)]).toEqual([]);
	});
});
