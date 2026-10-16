import { describe, expect, it } from 'vitest';

import { parseWorkItem, type WorkItem } from '$lib/stores/workLiveStore';
import { notchRunDots, NOTCH_DONE_DOT_LIMIT, NOTCH_DONE_DOT_MS, NOTCH_RUN_DOT_LIMIT } from './notchRuns';

function item(fields: Record<string, unknown>): WorkItem {
	return parseWorkItem({ kind: 'coding_run', context: 'Shop', ...fields }) as WorkItem;
}

describe('notch run dots', () => {
	it('sends the host an id, a state, a tooltip and a path, capped', () => {
		const items = Array.from({ length: NOTCH_RUN_DOT_LIMIT + 3 }, (_, index) =>
			item({ id: `run:r${index}`, state: 'working', title: `Run ${index}`, web_path: `/coding-runs?run=r${index}` })
		);
		const dots = notchRunDots(items);
		expect(dots).toHaveLength(NOTCH_RUN_DOT_LIMIT);
		expect(dots[0]).toEqual({ id: 'run:r0', state: 'working', title: 'Run 0 · Shop', path: '/coding-runs?run=r0' });
	});

	it('gives a crew one dot and a Work task its title alone', () => {
		const dots = notchRunDots([
			item({
				id: 'crew:crew_1',
				kind: 'crew',
				state: 'needs_you',
				title: 'Login with codes',
				web_path: '/vibe?project=p&crew=crew_1'
			}),
			item({ id: 'task:task_9', kind: 'work_task', state: 'working', title: 'Weekly report', context: '' })
		]);
		expect(dots).toEqual([
			{
				id: 'crew:crew_1',
				state: 'needs_you',
				title: 'Login with codes · Shop',
				path: '/vibe?project=p&crew=crew_1'
			},
			{ id: 'task:task_9', state: 'working', title: 'Weekly report', path: null }
		]);
	});

	it('leads the label with the run, not the VibeDev prefix', () => {
		const run = item({ id: 'run:a', state: 'working', title: 'VibeDev · Fix the footer' });
		expect(notchRunDots([run])[0].title).toBe('Fix the footer · Shop');
	});

	it('gives a stopped item no dot', () => {
		const now = Date.parse('2026-10-14T12:00:00Z');
		const items = [
			item({ id: 'run:a', state: 'stopped', title: 'Stopped' }),
			item({ id: 'task:b', kind: 'work_task', state: 'done', title: 'Done', updated_at: '2026-10-14T11:59:30Z' })
		];
		expect(notchRunDots(items, now).map((dot) => dot.id)).toEqual(['task:b']);
	});

	it('keeps a done item green for two minutes, the latest four', () => {
		const now = Date.parse('2026-10-14T12:00:00Z');
		const at = (secondsAgo: number) => new Date(now - secondsAgo * 1000).toISOString();
		const done = (id: string, secondsAgo: number) =>
			item({ id, state: 'done', title: id, updated_at: at(secondsAgo) });
		const items = [
			item({ id: 'w', state: 'working', title: 'w', updated_at: at(900) }),
			done('d1', 10),
			done('d2', 20),
			done('d3', 30),
			done('d4', 40),
			done('d5', 50),
			done('old', NOTCH_DONE_DOT_MS / 1000 + 1),
			item({ id: 'f', state: 'failed', title: 'f', updated_at: at(900) })
		];
		const ids = notchRunDots(items, now).map((dot) => dot.id);
		expect(ids).toEqual(['w', 'd1', 'd2', 'd3', 'd4', 'f']);
		expect(ids.filter((id) => id.startsWith('d'))).toHaveLength(NOTCH_DONE_DOT_LIMIT);
	});
});
