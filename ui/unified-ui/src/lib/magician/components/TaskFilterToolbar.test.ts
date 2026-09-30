import { describe, expect, it } from 'vitest';
import { orderTaskTags } from './TaskFilterToolbar.svelte';

describe('task filter tag ordering', () => {
	it('puts shortest tags first with a deterministic alphabetical tie-break', () => {
		expect(orderTaskTags(['follow-up', 'ops', 'qa', 'Bug', 'planning'])).toEqual([
			'qa',
			'Bug',
			'ops',
			'planning',
			'follow-up'
		]);
	});

	it('does not mutate the tag array supplied by the task store', () => {
		const tags = ['longer', 'x'];

		orderTaskTags(tags);

		expect(tags).toEqual(['longer', 'x']);
	});
});
