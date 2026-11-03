import { describe, expect, it } from 'vitest';

import {
	cardTimesLabel,
	formatClockTime,
	formatExactTime,
	timeSpan,
	toEpochMs
} from './cardTimes';
import { formatTimeAgo } from './formatRelativeTime';

const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;
// Local noon, so a few hours either way stays on the same calendar day.
const NOW = new Date(2026, 9, 31, 12, 0, 0).getTime();

function clock(ms: number): string {
	return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

describe('formatTimeAgo', () => {
	it('reads as an age up to a week, then as a short date', () => {
		expect(formatTimeAgo(NOW - 20_000, NOW)).toBe('now');
		expect(formatTimeAgo(NOW - 2 * MIN, NOW)).toBe('2m ago');
		expect(formatTimeAgo(NOW - 3 * HOUR, NOW)).toBe('3h ago');
		expect(formatTimeAgo(NOW - 2 * DAY, NOW)).toBe('2d ago');
		const old = NOW - 10 * DAY;
		expect(formatTimeAgo(old, NOW)).toBe(
			new Date(old).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
		);
	});

	it('reads a time ahead of the clock (server skew) as now, and nothing as empty', () => {
		expect(formatTimeAgo(NOW + 5 * MIN, NOW)).toBe('now');
		expect(formatTimeAgo(undefined, NOW)).toBe('');
		expect(formatTimeAgo(Number.NaN, NOW)).toBe('');
	});
});

describe('toEpochMs', () => {
	it('takes epoch ms, epoch seconds and ISO strings', () => {
		expect(toEpochMs(NOW)).toBe(NOW);
		expect(toEpochMs(NOW / 1000)).toBe(NOW);
		expect(toEpochMs(new Date(NOW).toISOString())).toBe(NOW);
	});

	it('drops anything that is not a usable time', () => {
		for (const value of [undefined, null, 0, -1, Number.NaN, Infinity, '', '  ', 'not a date']) {
			expect(toEpochMs(value as number | string | null | undefined)).toBeUndefined();
		}
	});
});

describe('formatClockTime', () => {
	it('shows only the time today, and the date with it on other days', () => {
		expect(formatClockTime(NOW - HOUR, NOW)).toBe(clock(NOW - HOUR));
		const earlier = NOW - 2 * DAY;
		const day = new Date(earlier).toLocaleDateString([], { month: 'short', day: 'numeric' });
		expect(formatClockTime(earlier, NOW)).toBe(`${day}, ${clock(earlier)}`);
	});

	it('is empty for an unusable time, never "Invalid Date"', () => {
		expect(formatClockTime(undefined, NOW)).toBe('');
		expect(formatClockTime(Number.NaN, NOW)).toBe('');
		expect(formatExactTime(undefined)).toBe('');
	});
});

describe('timeSpan', () => {
	it('is the earliest and latest usable time, skipping the rest', () => {
		expect(timeSpan([NOW, null, NOW - HOUR, 'bad', new Date(NOW + MIN).toISOString()])).toEqual({
			createdAt: NOW - HOUR,
			updatedAt: NOW + MIN
		});
		expect(timeSpan([])).toEqual({ createdAt: undefined, updatedAt: undefined });
	});
});

describe('cardTimesLabel', () => {
	it('reads "Created <clock> · Updated <age>", with the exact times in the title', () => {
		const label = cardTimesLabel({ createdAt: NOW - HOUR, updatedAt: NOW - 2 * MIN }, NOW);
		expect(label?.text).toBe(`Created ${clock(NOW - HOUR)} · Updated 2m ago`);
		expect(label?.title).toBe(
			`Created ${formatExactTime(NOW - HOUR)}\nUpdated ${formatExactTime(NOW - 2 * MIN)}`
		);
	});

	it('names the start with the given word', () => {
		expect(cardTimesLabel({ createdAt: NOW, updatedAt: NOW }, NOW, 'Started')?.text).toBe(
			`Started ${clock(NOW)} · Updated now`
		);
	});

	it('leaves out a part whose time is unknown, and is null with neither', () => {
		expect(cardTimesLabel({ createdAt: NOW - HOUR }, NOW)?.text).toBe(`Created ${clock(NOW - HOUR)}`);
		expect(cardTimesLabel({ updatedAt: NOW - 3 * HOUR }, NOW)?.text).toBe('Updated 3h ago');
		expect(cardTimesLabel({ createdAt: Number.NaN, updatedAt: 0 }, NOW)).toBeNull();
		expect(cardTimesLabel({}, NOW)).toBeNull();
	});
});
