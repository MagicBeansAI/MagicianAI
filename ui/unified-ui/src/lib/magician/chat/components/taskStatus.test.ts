import { describe, expect, it } from 'vitest';
import type {
	ChatMessage,
	ChatMessageContent,
	ChatRenderTaskExecutionGroup
} from '$lib/stores/chatStore';
import {
	buildArchivedTaskHref,
	buildThreadTaskHref,
	isRunExpanded,
	isTerminalTaskStatus,
	taskCardTimes,
	taskExecutionLabel,
	taskRunCollapsedLine,
	taskStatusReason,
	taskStatusVisual
} from './taskStatus';

function message(content: Partial<ChatMessageContent> = {}): ChatMessage {
	return {
		id: 'message-1',
		session_id: 'session-1',
		direction: 'system',
		created_at: 1,
		content: {
			type: 'task_status_update',
			task_id: 'task-1',
			status: 'running',
			...content
		}
	};
}

function group(content: Partial<ChatMessageContent> = {}, id = 'group-1'): ChatRenderTaskExecutionGroup {
	const current = message(content);
	return { id, message: current, messageIds: [current.id], updates: [current] };
}

describe('chat task status visuals', () => {
	it.each([
		['completed', true], ['failed', true], ['cancelled', true], ['archived', true],
		['running', false], ['paused', false], ['planning', false], [undefined, false]
	])('classifies terminal status %j', (status, expected) => {
		expect(isTerminalTaskStatus(status)).toBe(expected);
	});

	it.each([
		['created', { tone: 'created', icon: 'sparkle', verb: 'Created' }],
		['planning', { tone: 'running', icon: 'spinner', verb: 'Planning' }],
		['ready', { tone: 'neutral', icon: 'clock', verb: 'Plan approved, not started' }],
		['pending', { tone: 'neutral', icon: 'clock', verb: 'Not started' }],
		['draft', { tone: 'neutral', icon: 'pause', verb: 'Plan ready for review' }],
		['rejected', { tone: 'neutral', icon: 'clock', verb: 'Plan rejected' }],
		['running', { tone: 'running', icon: 'spinner', verb: 'Running' }],
		['in_progress', { tone: 'running', icon: 'spinner', verb: 'Running' }],
		['paused', { tone: 'neutral', icon: 'pause', verb: 'Paused' }],
		['completed', { tone: 'completed', icon: 'check', verb: 'Completed' }],
		['failed', { tone: 'failed', icon: 'alert', verb: 'Failed' }],
		['cancelled', { tone: 'cancelled', icon: 'pause', verb: 'Cancelled' }],
		['archived', { tone: 'neutral', icon: 'check', verb: 'Archived' }],
		['deferred', { tone: 'neutral', icon: 'clock', verb: 'deferred' }],
		[undefined, { tone: 'neutral', icon: 'clock', verb: 'Updated' }]
	] as const)('maps %j to stable card visuals', (status, expected) => {
		expect(taskStatusVisual(status)).toEqual(expected);
	});

	it('preserves partial delivery after synthesis finishes', () => {
		expect(taskStatusVisual('completed', false, 'partial').verb).toBe('Partially completed');
		expect(taskStatusVisual('completed', true, 'partial').verb).toBe('Preparing final result');
		expect(taskStatusVisual('completed', false, 'full').verb).toBe('Completed');
	});

	it('lets synthesis pending override a terminal-looking status', () => {
		expect(taskStatusVisual('completed', true)).toEqual({
			tone: 'running', icon: 'spinner', verb: 'Preparing final result'
		});
	});
});

describe('chat task run disclosure', () => {
	it('honors explicit expansion overrides', () => {
		const current = group({ status: 'running' });
		expect(isRunExpanded(new Map([[current.id, false]]), current, 1)).toBe(false);
		expect(isRunExpanded(new Map([[current.id, true]]), current, 3)).toBe(true);
	});

	it('opens active or lone runs and collapses terminal runs in a multi-run card', () => {
		expect(isRunExpanded(new Map(), group({ status: 'running' }), 3)).toBe(true);
		expect(isRunExpanded(new Map(), group({ status: 'completed' }), 1)).toBe(true);
		expect(isRunExpanded(new Map(), group({ status: 'completed' }), 2)).toBe(false);
	});

	it('uses the first clean summary sentence for collapsed multi-run rows', () => {
		expect(taskRunCollapsedLine(group({
			status: 'completed',
			summary: '**Done.** [Open result](https://example.com) for details.'
		}), 2)).toBe('Done.');
	});

	it('avoids repeating a lone run summary and humanizes status underscores', () => {
		expect(taskRunCollapsedLine(group({ status: 'waiting_for_user', summary: 'Do not repeat' }), 1))
			.toBe('waiting for user');
		expect(taskRunCollapsedLine(group({ status: undefined, summary: '' }), 1)).toBe('update');
	});

	it('labels primary and numbered executions without exposing execution ids', () => {
		expect(taskExecutionLabel(group({ execution_id: undefined }), 0, 2)).toBe('Primary run');
		expect(taskExecutionLabel(group({ execution_id: 'exec-secret' }), 1, 3)).toBe('Run 2');
		expect(taskExecutionLabel(group({ execution_id: 'exec-secret' }), 0, 1)).toBe('Run');
	});
});

describe('chat task links', () => {
	it('builds named-thread links with encoded thread and task ids', () => {
		const content: ChatMessageContent = {
			type: 'task_status_update',
			task_id: 'task/with spaces',
			ui_thread_id: 'Sales & Ops'
		};
		expect(buildThreadTaskHref(content)).toBe(
			'/t/Sales%20%26%20Ops?selected=task%2Fwith+spaces'
		);
		expect(buildArchivedTaskHref(content)).toBe(
			'/t/Sales%20%26%20Ops?selected=task%2Fwith+spaces&filter=all'
		);
	});

	it.each([
		{},
		{ task_id: ' ' },
		{ task_id: 'task-1' },
		{ task_id: 'task-1', ui_thread_id: 'general' },
		{ task_id: 'task-1', ui_thread_id: '  ' }
	])('does not create dead links for internal/general task context %j', (overrides) => {
		const content = { type: 'task_status_update', ...overrides } as ChatMessageContent;
		expect(buildThreadTaskHref(content)).toBeNull();
		expect(buildArchivedTaskHref(content)).toBeNull();
	});
});

describe('taskStatusReason', () => {
	it('is the summary for every status, verbatim apart from trimming', () => {
		for (const status of ['running', 'paused', 'completed', 'failed', 'cancelled']) {
			expect(taskStatusReason({ summary: ` Token expired - re-login. `, status } as ChatMessageContent)).toBe(
				'Token expired - re-login.'
			);
		}
	});

	it('is undefined for a missing or blank summary', () => {
		expect(taskStatusReason({})).toBeUndefined();
		expect(taskStatusReason({ summary: '  \n ' })).toBeUndefined();
	});
});

describe('taskCardTimes', () => {
	const T0 = 1_760_000_000_000;
	function at(createdAt: number, id = `m-${createdAt}`): ChatMessage {
		return { ...message(), id, created_at: createdAt };
	}
	function runOf(...updates: ChatMessage[]): ChatRenderTaskExecutionGroup {
		const latest = updates[updates.length - 1];
		return { id: 'run-1', message: latest, messageIds: updates.map((u) => u.id), updates };
	}

	it('spans the card\'s own updates when the task store does not have the task', () => {
		const first = at(T0);
		const latest = at(T0 + 90_000);
		expect(taskCardTimes(latest, [runOf(first, latest)])).toEqual({
			createdAt: T0,
			updatedAt: T0 + 90_000
		});
	});

	it('reads epoch-second update times as seconds', () => {
		expect(taskCardTimes(at(T0 / 1000))).toEqual({ createdAt: T0, updatedAt: T0 });
	});

	it('takes the record\'s creation time and its last progress when later than the card', () => {
		const update = at(T0);
		expect(
			taskCardTimes(update, [runOf(update)], {
				createdAt: new Date(T0 - 3_600_000).toISOString(),
				lastProgressAt: T0 + 120_000
			})
		).toEqual({ createdAt: T0 - 3_600_000, updatedAt: T0 + 120_000 });
	});

	it('keeps a later card update over the record\'s progress, and ignores its updatedAt', () => {
		expect(taskCardTimes(at(T0 + 300_000), [], { lastProgressAt: T0 + 60_000 }).updatedAt).toBe(
			T0 + 300_000
		);
		// updatedAt moves on heartbeats and local edits (and is "now" when the
		// wire had none): an idle task must not read "Updated now".
		const record = { updatedAt: new Date(T0 + 3_600_000).toISOString() };
		expect(taskCardTimes(at(T0), [], record).updatedAt).toBe(T0);
	});

	it('never dates the task as created after its card\'s first update', () => {
		// The store stamps a record that came without created_at as created "now".
		const update = at(T0);
		expect(
			taskCardTimes(update, [], { createdAt: new Date(T0 + 86_400_000).toISOString() }).createdAt
		).toBe(T0);
	});

	it('leaves a time out when nothing gives it', () => {
		expect(taskCardTimes(at(0), [], { createdAt: 'not a date' })).toEqual({
			createdAt: undefined,
			updatedAt: undefined
		});
	});
});
