import { describe, expect, it } from 'vitest';

import type { TodayItem } from './types';
import { formatSpaceLabel, todaySpaceGroups } from './spaceGroups';

function item(id: string, spaceIds: string[]): TodayItem {
	return {
		id,
		principal: 'anonymous',
		workspace: 'default',
		section: 'changed',
		priority: 0,
		title: id,
		summary: null,
		reason: '',
		source_kind: 'memory',
		source_id: id,
		source_url: null,
		space_ids: spaceIds,
		thread_id: null,
		task_id: null,
		agent_id: null,
		status: 'info',
		actions: [],
		evidence_refs: [],
		created_at: 1,
		updated_at: 1,
		metadata: {}
	};
}

describe('todaySpaceGroups', () => {
	it('labels items without a space as Other and sorts that group last', () => {
		const groups = todaySpaceGroups([
			item('none', []),
			item('beta', ['beta_space']),
			item('alpha', ['alpha-space'])
		]);

		expect(groups.map(({ id }) => id)).toEqual(['alpha-space', 'beta_space', 'unfiled']);
		expect(groups.map(({ label }) => label)).toEqual(['Alpha Space', 'Beta Space', 'Other']);
	});

	it('normalizes a space identifier for display', () => {
		expect(formatSpaceLabel('customer_success')).toBe('Customer Success');
		expect(formatSpaceLabel('launch-planning')).toBe('Launch Planning');
	});
});
