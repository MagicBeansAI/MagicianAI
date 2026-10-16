import { describe, expect, it } from 'vitest';

import { todayActionEndpoint, todayActionTaskId } from './actions';

describe('Today source actions', () => {
	it('accepts only the scoped Today POST action contract', () => {
		expect(
			todayActionEndpoint({
				id: 'create_task',
				label: 'Create task',
				action_type: 'today_source_action',
				payload: {
					method: 'POST',
					endpoint: '/api/magician/v2/today/items/item/actions/create_task'
				}
			})
		).toBe('/api/magician/v2/today/items/item/actions/create_task');
		expect(
			todayActionEndpoint({
				id: 'bad',
				label: 'Bad',
				payload: { method: 'GET', endpoint: '/api/magician/v2/today/items/x/actions/bad' }
			})
		).toBeNull();
		expect(
			todayActionEndpoint({
				id: 'bad',
				label: 'Bad',
				payload: { method: 'POST', endpoint: 'https://example.com/action' }
			})
		).toBeNull();
	});

	it('prefers the explicit navigation task and falls back safely', () => {
		expect(
			todayActionTaskId({
				navigate_to: { kind: 'task', task_id: 'task_navigation' },
				task_id: 'task_top_level',
				task: { manifest: { task_id: 'task_record' } }
			})
		).toBe('task_navigation');
		expect(todayActionTaskId({ task: { manifest: { task_id: 'task_record' } } })).toBe(
			'task_record'
		);
		expect(todayActionTaskId({})).toBeNull();
	});
});
