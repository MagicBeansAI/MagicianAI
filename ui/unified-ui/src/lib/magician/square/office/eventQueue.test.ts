import { afterEach, describe, expect, it, vi } from 'vitest';

import { createEventQueue } from './eventQueue';
import type { OfficeEvent } from './officeEvents';

const ev = (type: OfficeEvent['type'], id: string = type): OfficeEvent => {
	switch (type) {
		case 'delivery-landed':
			return { type, id, citizenId: 'cto', createdAt: 1, title: id };
		case 'handoff':
			return { type, id, fromId: 'cto', toId: 'presto', questId: id };
		case 'task-routed':
			return { type, citizenId: 'cto', questId: id, title: id };
		case 'blocked':
			return { type, citizenId: 'cto', attentionId: id };
		case 'unblocked':
			return { type, citizenId: 'cto', attentionId: id };
		case 'work-started':
			return { type, citizenId: 'cto', questId: id };
		case 'work-ended':
			return { type, citizenId: 'cto', questId: id };
		case 'social-talk':
			return {
				type,
				id,
				venue: 'cooler',
				participantIds: ['cto', 'presto'],
				text: id
			};
	}
};

afterEach(() => {
	vi.useRealTimers();
});

describe('createEventQueue', () => {
	it('releases a burst of five sequentially', () => {
		vi.useFakeTimers();
		const released: string[] = [];
		const q = createEventQueue({
			intervalMs: 1_000,
			cap: 20,
			onRelease: (event) => released.push(event.type)
		});
		q.enqueue([
			ev('work-started', 'a'),
			ev('work-started', 'b'),
			ev('work-started', 'c'),
			ev('work-started', 'd'),
			ev('work-started', 'e')
		]);
		expect(released).toEqual([]);
		vi.advanceTimersByTime(1_000);
		expect(released).toEqual(['work-started']);
		vi.advanceTimersByTime(1_000);
		expect(released).toEqual(['work-started', 'work-started']);
		vi.advanceTimersByTime(3_000);
		expect(released).toHaveLength(5);
		q.stop();
	});

	it('drops the oldest low-priority events when the cap is hit and logs them', () => {
		vi.useFakeTimers();
		const released: string[] = [];
		const dropped: string[] = [];
		const q = createEventQueue({
			intervalMs: 1_000,
			cap: 3,
			onRelease: (event) => released.push(event.type),
			onDrop: (event) => dropped.push(event.type)
		});
		q.enqueue([
			ev('work-ended', 'old-end'),
			ev('delivery-landed', 'parcel'),
			ev('task-routed', 'task'),
			ev('handoff', 'pass'),
			ev('blocked', 'need')
		]);
		expect(dropped).toEqual(['work-ended', 'delivery-landed']);
		vi.advanceTimersByTime(4_000);
		expect(released).toEqual(['blocked', 'handoff', 'task-routed']);
		q.stop();
	});

	it('is inert when the queue is empty — no timer left running', () => {
		vi.useFakeTimers();
		const q = createEventQueue({
			intervalMs: 1_000,
			cap: 8,
			onRelease: () => undefined
		});
		expect(vi.getTimerCount()).toBe(0);
		q.enqueue([]);
		expect(vi.getTimerCount()).toBe(0);
		q.enqueue([ev('blocked', 'need')]);
		expect(vi.getTimerCount()).toBeGreaterThan(0);
		vi.advanceTimersByTime(1_000);
		expect(vi.getTimerCount()).toBe(0);
		q.stop();
		expect(vi.getTimerCount()).toBe(0);
	});
});
