import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
	AGE_TICK_MS,
	formatElapsed,
	LIVE_TICK_MS,
	liveClock,
	minuteTick,
	runningElapsed,
	secondTick
} from './liveClock';

describe('liveClock', () => {
	beforeEach(() => {
		vi.useFakeTimers();
		vi.setSystemTime(100_000);
	});
	afterEach(() => vi.useRealTimers());

	it('runs one interval for every subscriber, and none once the last one leaves', () => {
		const seen: number[][] = [[], []];
		const a = secondTick.subscribe((now) => seen[0].push(now));
		const b = secondTick.subscribe((now) => seen[1].push(now));
		expect(vi.getTimerCount()).toBe(1);

		vi.advanceTimersByTime(LIVE_TICK_MS);
		expect(seen[0]).toEqual([100_000, 101_000]);
		expect(seen[1]).toEqual([100_000, 101_000]);

		a();
		expect(vi.getTimerCount()).toBe(1);
		b();
		expect(vi.getTimerCount()).toBe(0);
	});

	it('runs one minute interval for every card age, and none once the last card leaves', () => {
		const seen: number[] = [];
		const a = minuteTick.subscribe((now) => seen.push(now));
		const b = minuteTick.subscribe(() => {});
		expect(vi.getTimerCount()).toBe(1);

		vi.advanceTimersByTime(AGE_TICK_MS);
		expect(seen).toEqual([100_000, 100_000 + AGE_TICK_MS]);

		a();
		b();
		expect(vi.getTimerCount()).toBe(0);
	});

	it('holds the tick only while a host says a row is running', () => {
		const ticks: number[] = [];
		const clock = liveClock((now) => ticks.push(now));

		clock.sync(false);
		expect(vi.getTimerCount()).toBe(0);

		clock.sync(true);
		clock.sync(true); // idempotent: one subscription, not two
		expect(vi.getTimerCount()).toBe(1);
		vi.advanceTimersByTime(2 * LIVE_TICK_MS);
		expect(ticks).toEqual([100_000, 101_000, 102_000]);

		clock.sync(false);
		expect(vi.getTimerCount()).toBe(0);
		vi.advanceTimersByTime(LIVE_TICK_MS);
		expect(ticks).toHaveLength(3);

		clock.sync(true);
		clock.stop();
		expect(vi.getTimerCount()).toBe(0);
	});
});

describe('runningElapsed', () => {
	it('reads m:ss, then h:mm:ss past an hour', () => {
		expect(runningElapsed(58_000, 100_000)).toBe('0:42');
		expect(runningElapsed(0 + 1, 65_001)).toBe('1:05');
		expect(runningElapsed(1_000, 1_000 + 3_725_000)).toBe('1:02:05');
		expect(formatElapsed(42_999)).toBe('0:42');
	});

	it('clamps a start ahead of this clock to 0:00, and has nothing for no start', () => {
		expect(runningElapsed(101_000, 100_000)).toBe('0:00');
		expect(runningElapsed(undefined, 100_000)).toBeNull();
		expect(runningElapsed(null, 100_000)).toBeNull();
		expect(runningElapsed(0, 100_000)).toBeNull();
		expect(runningElapsed(Number.NaN, 100_000)).toBeNull();
	});
});
