import { describe, expect, it } from 'vitest';
import type { Task } from '$lib/stores/taskStore';
import {
	autoAdvanceDelay,
	bucketTasks,
	recentTasks,
	shortRelativeTime,
	slideLabel,
	stepSlideIndex,
	taskDotTone
} from './opsCarousel';
import { avgCostPerCall, formatPerCall, peakSpendHour } from './opsCarousel';

const task = (id: string, status: Task['status'], updatedAt: string, createdAt = updatedAt) =>
	({ id, status, updatedAt, createdAt }) as Pick<Task, 'id' | 'status' | 'updatedAt' | 'createdAt'>;

describe('opsCarousel helpers', () => {
	it('buckets tasks like the pie and floors succeeded at the pulse count', () => {
		const tasks = [
			task('a', 'running', ''),
			task('b', 'paused', ''),
			task('c', 'planning', ''),
			task('d', 'completed', ''),
			task('e', 'failed', ''),
			task('f', 'ready', ''),
			task('g', 'pending', '')
		];
		expect(bucketTasks(tasks)).toEqual({ active: 3, succeeded: 1, failed: 1, total: 5 });
		expect(bucketTasks(tasks, 4)).toEqual({ active: 3, succeeded: 4, failed: 1, total: 8 });
	});

	it('orders recent tasks by updatedAt desc and caps the list', () => {
		const tasks = Array.from({ length: 25 }, (_, i) =>
			task(`t${i}`, 'completed', new Date(Date.UTC(2026, 8, 28, 0, i)).toISOString())
		);
		const recent = recentTasks(tasks);
		expect(recent).toHaveLength(20);
		expect(recent[0].id).toBe('t24');
		expect(recent[19].id).toBe('t5');
		expect(recentTasks(tasks, 3).map((t) => t.id)).toEqual(['t24', 't23', 't22']);
	});

	it('falls back to createdAt and keeps unparseable times last', () => {
		const tasks = [task('bad', 'ready', 'nope', 'nope'), task('old', 'ready', '', '2026-09-27T00:00:00Z')];
		expect(recentTasks(tasks).map((t) => t.id)).toEqual(['old', 'bad']);
	});

	it('formats short relative times', () => {
		const now = Date.parse('2026-09-28T12:00:00Z');
		expect(shortRelativeTime('2026-09-28T11:59:30Z', now)).toBe('now');
		expect(shortRelativeTime('2026-09-28T11:56:00Z', now)).toBe('4m');
		expect(shortRelativeTime('2026-09-28T10:00:00Z', now)).toBe('2h');
		expect(shortRelativeTime('2026-09-27T11:00:00Z', now)).toBe('1d');
		expect(shortRelativeTime(undefined, now)).toBe('');
	});

	it('maps statuses to dot tones', () => {
		expect(taskDotTone('running')).toBe('active');
		expect(taskDotTone('completed')).toBe('success');
		expect(taskDotTone('failed')).toBe('danger');
		expect(taskDotTone('ready')).toBe('muted');
	});

	it('wraps slide steps in both directions', () => {
		expect(stepSlideIndex(1, 2, 1)).toBe(0);
		expect(stepSlideIndex(0, 2, -1)).toBe(1);
		expect(stepSlideIndex(0, 0, 1)).toBe(0);
	});

	it('holds auto-advance while interacting and until the manual pause ends', () => {
		expect(autoAdvanceDelay({ nowMs: 1000, pausedUntilMs: 0, interacting: false })).toBe(0);
		expect(autoAdvanceDelay({ nowMs: 1000, pausedUntilMs: 0, interacting: true, intervalMs: 8000 })).toBe(8000);
		expect(autoAdvanceDelay({ nowMs: 1000, pausedUntilMs: 16000, interacting: false })).toBe(15000);
	});

	it('labels slides for dots and announcements', () => {
		expect(slideLabel(1, 2, 'State of Operations')).toBe('Slide 2 of 2, State of Operations');
	});
});

describe('economics stat grid helpers', () => {
	it('averages spend per call and formats sub-cent values with four decimals', () => {
		expect(avgCostPerCall(0.04, 27)).toBeCloseTo(0.00148, 5);
		expect(avgCostPerCall(1, 0)).toBeNull();
		expect(formatPerCall(0.0014814)).toBe('$0.0015');
		expect(formatPerCall(0.25)).toBe('$0.25');
		expect(formatPerCall(null)).toBe('—');
	});

	it('names the peak spend hour', () => {
		const hours = new Array(24).fill(0);
		expect(peakSpendHour(hours)).toBeNull();
		hours[12] = 0.02;
		hours[3] = 0.01;
		expect(peakSpendHour(hours)).toBe('12p');
		hours[15] = 0.05;
		expect(peakSpendHour(hours)).toBe('3p');
		hours[0] = 0.09;
		expect(peakSpendHour(hours)).toBe('12a');
	});
});
