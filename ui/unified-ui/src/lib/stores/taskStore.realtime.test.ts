import { afterEach, describe, expect, it } from 'vitest';

import type { V2WebSocketEvent } from '$lib/realtime/v2-websocket';
import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
import { isScopedTaskRefreshEvent } from './taskStore';

afterEach(() => scopeIdentityStore.reset());

function taskDeleted(principal: string, workspace: string): V2WebSocketEvent {
	return {
		event_type: 'TaskDeleted',
		data: {
			principal,
			workspace,
			task_id: 'task_1',
			title: 'Leaving the list',
			deleted_at: 1,
			timestamp: 1
		}
	} as V2WebSocketEvent;
}

describe('task store realtime refresh', () => {
	it('refreshes when a task in this scope is archived or deleted', () => {
		scopeIdentityStore.observe('alpha', 'prod');
		expect(isScopedTaskRefreshEvent(taskDeleted('alpha', 'prod'))).toBe(true);
	});

	it("ignores another scope's deletions", () => {
		scopeIdentityStore.observe('alpha', 'prod');
		expect(isScopedTaskRefreshEvent(taskDeleted('beta', 'prod'))).toBe(false);
	});
});
