import { describe, expect, it } from 'vitest';

import {
	buildSelectedTaskMap,
	buildTaskLookup,
	sanitizeDependsOn
} from './taskMention';

describe('taskMention helpers', () => {
	it('drops stale dependency ids before building the selected task map', () => {
		const lookup = buildTaskLookup([
			{ id: 't1', title: 'Alpha', status: 'completed' },
			{ id: 't2', title: 'Beta', status: 'completed' }
		]);

		const sanitized = sanitizeDependsOn(['t1', 'missing', 't2'], lookup);
		const selected = buildSelectedTaskMap(sanitized, lookup);

		expect(sanitized).toEqual(['t1', 't2']);
		expect(Array.from(selected.keys())).toEqual(['t1', 't2']);
		expect(selected.get('t1')?.title).toBe('Alpha');
	});

	it('preserves depends_on ordering in the selected task map', () => {
		const lookup = buildTaskLookup([
			{ id: 't1', title: 'Alpha', status: 'completed' },
			{ id: 't2', title: 'Beta', status: 'completed' }
		]);

		const selected = buildSelectedTaskMap(['t2', 't1'], lookup);

		expect(Array.from(selected.keys())).toEqual(['t2', 't1']);
	});
});
