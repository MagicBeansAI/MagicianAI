import { describe, expect, it } from 'vitest';
import {
	DAY_MS,
	LLM_SHORT_WINDOW_LANE,
	laneForWindowDays,
	queryAnchorMs,
	rollingWindowPredicate,
	rollingWindowStartMs
} from './queryWindow';

const NOW = Date.UTC(2026, 9, 30, 10, 17, 42, 123);
const MINUTE = Date.UTC(2026, 9, 30, 10, 18, 0, 0);

describe('queryAnchorMs', () => {
	it('rounds up to the minute so same-minute rebuilds produce the same SQL', () => {
		expect(queryAnchorMs(NOW)).toBe(MINUTE);
		expect(queryAnchorMs(MINUTE)).toBe(MINUTE);
		expect(queryAnchorMs(MINUTE - 59_999)).toBe(MINUTE);
		expect(queryAnchorMs(MINUTE + 1)).toBe(MINUTE + 60_000);
	});
});

describe('rollingWindowPredicate', () => {
	it('emits a literal epoch-ms lower bound the server can parse, not INTERVAL', () => {
		const sql = rollingWindowPredicate(7, NOW);
		expect(sql).toBe(`timestamp_ms > ${MINUTE - 7 * DAY_MS}`);
		expect(sql).toMatch(/^timestamp_ms > \d{13}$/);
		expect(sql).not.toContain('INTERVAL');
		expect(sql).not.toContain('now()');
	});

	it('keeps the same rolling N x 24h window as the INTERVAL form', () => {
		expect(rollingWindowStartMs(30, NOW)).toBe(MINUTE - 30 * DAY_MS);
		expect(rollingWindowStartMs(1, NOW)).toBe(MINUTE - DAY_MS);
	});

	it('is stable within a minute and moves across one', () => {
		expect(rollingWindowPredicate(7, NOW)).toBe(rollingWindowPredicate(7, MINUTE - 1));
		expect(rollingWindowPredicate(7, NOW)).not.toBe(rollingWindowPredicate(7, MINUTE + 1));
	});

	it('never widens the window past N days (server lookback stays N days)', () => {
		for (const now of [NOW, MINUTE, MINUTE - 59_999]) {
			expect(now - rollingWindowStartMs(7, now)).toBeLessThanOrEqual(7 * DAY_MS);
			expect(now - rollingWindowStartMs(7, now)).toBeGreaterThan(7 * DAY_MS - 60_000);
		}
	});
});

describe('laneForWindowDays', () => {
	it('routes windows of at most one day to the short-window lane', () => {
		expect(laneForWindowDays(1)).toBe(LLM_SHORT_WINDOW_LANE);
		expect(laneForWindowDays(7)).toBeUndefined();
		expect(laneForWindowDays(30)).toBeUndefined();
		// Lifetime is unbounded — never the short lane.
		expect(laneForWindowDays(0)).toBeUndefined();
	});
});
