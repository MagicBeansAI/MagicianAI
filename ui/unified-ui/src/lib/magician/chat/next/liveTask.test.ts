import { describe, expect, it } from 'vitest';

import type { ChatMessage, ChatRenderTaskExecutionGroup } from '$lib/stores/chatStore';
import { storeStatusForCard, toLiveTaskView } from './liveTask';

const T0 = 1_760_000_000_000;

// Fixtures follow the task_status_update shape the chat store coalesces
// (see chatStore.rendering.test.ts / TaskStatusCard.component.test.ts).
function taskMessage(
	id: string,
	status: string,
	options: {
		executionId?: string;
		createdAt?: number;
		summary?: string;
		synthesisPending?: boolean;
		completionKind?: 'partial' | 'full';
	} = {}
): ChatMessage {
	return {
		id,
		session_id: 'session-1',
		direction: 'system',
		created_at: options.createdAt ?? T0,
		content: {
			type: 'task_status_update',
			task_id: 'task-1',
			display_label: 'Review release',
			status,
			execution_id: options.executionId,
			summary: options.summary,
			synthesis_pending: options.synthesisPending,
			completion_kind: options.completionKind
		}
	} as unknown as ChatMessage;
}

function group(executionId: string, updates: ChatMessage[]): ChatRenderTaskExecutionGroup {
	const latest = updates[updates.length - 1];
	return {
		id: `task:task-1::execution:${executionId}`,
		message: latest,
		messageIds: updates.map((u) => u.id),
		updates
	};
}

describe('toLiveTaskView', () => {
	it('reads a draft plan as waiting for review, not as running', () => {
		const view = toLiveTaskView(taskMessage('m1', 'draft'), []);
		expect(view.status).toBe('waiting');
		expect(view.verb).toBe('Plan ready for review');
		// A draft is itself an ask; a pause is not.
		expect(view.awaitingApproval).toBe(true);
		expect(toLiveTaskView(taskMessage('m2', 'paused'), []).awaitingApproval).toBe(false);
	});

	it('lets the task store win when the card message lags it at draft or planning', () => {
		const lagging = toLiveTaskView(
			taskMessage('m1', 'draft', { summary: 'Plan ready for review.' }),
			[],
			'ready'
		);
		expect(lagging.status).toBe('queued');
		expect(lagging.verb).toBe('Plan approved, not started');
		expect(lagging.reason).toBeUndefined();

		// A message that has moved on (a run started) is not overridden.
		expect(toLiveTaskView(taskMessage('m2', 'running'), [], 'ready').status).toBe('running');
	});

	it('reads an approved or unplanned task as not started, never as running', () => {
		const approved = toLiveTaskView(taskMessage('m1', 'ready'), []);
		expect(approved.status).toBe('queued');
		expect(approved.verb).toBe('Plan approved, not started');
		expect(approved.terminal).toBe(false);

		const unplanned = toLiveTaskView(taskMessage('m2', 'pending'), []);
		expect(unplanned.status).toBe('queued');
		expect(unplanned.verb).toBe('Not started');
	});

	it('takes id, title and the summary reason from the task_status_update content', () => {
		const view = toLiveTaskView(taskMessage('m1', 'running', { summary: 'Checking CI. Then tags.' }), []);
		expect(view.id).toBe('task-1');
		expect(view.title).toBe('Review release');
		expect(view.status).toBe('running');
		expect(view.reason).toBe('Checking CI. Then tags.');
		expect(view.terminal).toBe(false);
	});

	it('never titles the card with a raw id: "Task · <short id>" when there is no label', () => {
		const message = taskMessage('m1', 'running');
		delete (message.content as { display_label?: string }).display_label;
		expect(toLiveTaskView(message, []).title).toBe('Task · 1');
		// Live: the card read `task_2679cefd6f664bf7bd4a3b57fae4ce75`.
		message.content.task_id = 'task_2679cefd6f664bf7bd4a3b57fae4ce75';
		expect(toLiveTaskView(message, []).title).toBe('Task · 2679ce');
		// The reason is not a title, even when it is the only prose there is.
		message.content.summary = 'Searching flights.';
		expect(toLiveTaskView(message, []).title).toBe('Task · 2679ce');
		delete (message.content as { task_id?: string }).task_id;
		expect(toLiveTaskView(message, []).title).toBe('Task');
		expect(toLiveTaskView(message, []).id).toBe('m1');
	});

	it('titles from the display label through the drawer’s title rule', () => {
		const message = taskMessage('m1', 'running');
		// A routed prompt passed as the label becomes its goal.
		message.content.display_label =
			'web-researcher / Do not delegate further. Use browser with default cdp to search MakeMyTrip for the cheapest one-way economy flight BLR to DEL…';
		expect(toLiveTaskView(message, []).title).toBe(
			'Search MakeMyTrip for the cheapest one-way economy flight BLR to DEL'
		);
		// An id-shaped label is not a label.
		message.content.display_label = 'task-1';
		expect(toLiveTaskView(message, []).title).toBe('Task · 1');
	});

	it('carries no progress fraction (run-level data has no meaningful percentage)', () => {
		const view = toLiveTaskView(taskMessage('m1', 'running'), []);
		expect('progress' in view).toBe(false);
	});

	it('lists one step per run with its status', () => {
		const done = taskMessage('r1', 'completed', { executionId: 'e1' });
		const running = taskMessage('r2', 'running', { executionId: 'e2', summary: 'Rerunning the flaky suite.' });
		const view = toLiveTaskView(running, [group('e1', [done]), group('e2', [running])]);
		expect(view.steps.map((s) => s.status)).toEqual(['done', 'running']);
		expect(view.steps.map((s) => s.label)).toEqual(['Run 1', 'Run 2']);
		// The running run's label is a real step, distinct from the reason.
		expect(view.currentStep).toBe('Run 2');
		expect(view.reason).toBe('Rerunning the flaky suite.');
	});

	it('drops the summary-derived step when the reason already says it', () => {
		const running = taskMessage('r2', 'running', { executionId: 'e2', summary: 'Rerunning the flaky suite. Almost there.' });
		const view = toLiveTaskView(running, [group('e2', [running])]);
		expect(view.reason).toBe('Rerunning the flaky suite. Almost there.');
		expect(view.currentStep).toBeUndefined();
	});

	it('shows the reason for every state that has one, as classic does (content.summary)', () => {
		const failed = toLiveTaskView(taskMessage('m', 'failed', { summary: 'Auth expired for `gh` - re-login needed.' }), []);
		expect(failed.status).toBe('failed');
		// Kept verbatim (markdown, hyphens intact): the card renders it as markdown.
		expect(failed.reason).toBe('Auth expired for `gh` - re-login needed.');

		const waiting = toLiveTaskView(taskMessage('m', 'paused', { summary: 'Needs your approval to send the e-mail.' }), []);
		expect(waiting.status).toBe('waiting');
		expect(waiting.reason).toBe('Needs your approval to send the e-mail.');

		const done = toLiveTaskView(taskMessage('m', 'completed', { summary: 'Release **v1.2** tagged.' }), []);
		expect(done.reason).toBe('Release **v1.2** tagged.');

		expect(toLiveTaskView(taskMessage('m', 'completed', { summary: '   ' }), []).reason).toBeUndefined();
		expect(toLiveTaskView(taskMessage('m', 'completed'), []).reason).toBeUndefined();
	});

	it('maps a partial completion to partial, terminal, with its detail as the reason', () => {
		const view = toLiveTaskView(
			taskMessage('m', 'completed', { completionKind: 'partial', summary: 'Tagged; changelog still open.' }),
			[]
		);
		expect(view.status).toBe('partial');
		expect(view.terminal).toBe(true);
		expect(view.verb).toBe('Partially completed');
		expect(view.reason).toBe('Tagged; changelog still open.');

		const disposition = toLiveTaskView(taskMessage('m', 'partial_success'), []);
		expect(disposition.status).toBe('partial');
		expect(disposition.terminal).toBe(true);

		expect(toLiveTaskView(taskMessage('m', 'completed', { completionKind: 'full' }), []).status).toBe('completed');
	});

	it('does not read a partial completion kind on a still-running task', () => {
		const view = toLiveTaskView(taskMessage('m', 'running', { completionKind: 'partial' }), []);
		expect(view.status).toBe('running');
		expect(view.terminal).toBe(false);
	});

	it('falls back to the running step label, then the status verb', () => {
		const running = taskMessage('r1', 'running', { executionId: 'e1' });
		expect(toLiveTaskView(running, [group('e1', [running])]).currentStep).toBe('Run');
		expect(toLiveTaskView(taskMessage('m1', 'planning'), []).currentStep).toBe('Planning');
	});

	it('settles a run still marked running once the task store reads it finished', () => {
		// Run 2's own completion update never reached the chat, but the task
		// store already has the task completed.
		const done = taskMessage('r1', 'completed', { executionId: 'e1' });
		const stale = taskMessage('r2', 'running', { executionId: 'e2' });
		const view = toLiveTaskView(stale, [group('e1', [done]), group('e2', [stale])], 'completed');
		expect(view.terminal).toBe(true);
		expect(view.status).toBe('completed');
		expect(view.currentStep).toBeUndefined();
		expect(view.steps.map((s) => s.status)).toEqual(['done', 'done']);

		const failed = toLiveTaskView(stale, [group('e2', [stale])], 'failed');
		expect(failed.steps.map((s) => s.status)).toEqual(['failed']);
		// A synthesizing task stays live whatever the store says.
		const synth = taskMessage('r3', 'running', { executionId: 'e2', synthesisPending: true });
		expect(toLiveTaskView(synth, [], 'completed').terminal).toBe(false);
	});

	it('storeStatusForCard ignores a terminal status of an earlier run', () => {
		expect(storeStatusForCard({ status: 'completed', executionId: 'e2' }, 'e2')).toBe('completed');
		// The card shows rerun e3; the store still has e2's completion.
		expect(storeStatusForCard({ status: 'completed', executionId: 'e2' }, 'e3')).toBeUndefined();
		expect(storeStatusForCard({ status: 'running', executionId: 'e2' }, 'e3')).toBe('running');
		expect(storeStatusForCard({ status: 'completed' }, 'e3')).toBe('completed');
		expect(storeStatusForCard({ status: 'failed', executionId: 'e2' }, undefined)).toBe('failed');
		expect(storeStatusForCard(undefined, 'e1')).toBeUndefined();
	});

	it('detects terminal statuses', () => {
		const done = taskMessage('r1', 'completed', { executionId: 'e1', createdAt: T0 + 5_000 });
		const view = toLiveTaskView(done, [group('e1', [taskMessage('r0', 'running', { executionId: 'e1', createdAt: T0 + 2_000 }), done])]);
		expect(view.terminal).toBe(true);
		expect(view.status).toBe('completed');
		expect(view.currentStep).toBeUndefined();
		expect(view.startedAt).toBe(T0 + 2_000);
		expect(view.finishedAt).toBe(T0 + 5_000);

		expect(toLiveTaskView(taskMessage('m', 'failed'), []).terminal).toBe(true);
		expect(toLiveTaskView(taskMessage('m', 'cancelled'), []).status).toBe('cancelled');
	});

	it('keeps a finished-but-synthesizing task running', () => {
		const view = toLiveTaskView(taskMessage('m', 'running', { synthesisPending: true }), []);
		expect(view.terminal).toBe(false);
		expect(view.currentStep).toBe('Preparing final result');
	});

	it('maps a paused task to waiting on the user', () => {
		const paused = taskMessage('r1', 'paused', { executionId: 'e1' });
		const view = toLiveTaskView(paused, [group('e1', [paused])]);
		expect(view.status).toBe('waiting');
		expect(view.terminal).toBe(false);
		expect(view.steps[0].status).toBe('waiting');
	});

	it('normalises second-resolution timestamps to milliseconds', () => {
		const view = toLiveTaskView(taskMessage('m', 'running', { createdAt: 1_700_000_000 }), []);
		expect(view.startedAt).toBe(1_700_000_000_000);
		expect(view.finishedAt).toBeUndefined();
	});

	it('collects per-run output blocks like TaskStatusCard, and the latest run ids', () => {
		const first = taskMessage('r1', 'completed', { executionId: 'e1' });
		const second = taskMessage('r2', 'completed', { executionId: 'e2' });
		(second.content as { output_files?: unknown[] }).output_files = [
			{ type: 'file', source: { type: 'session_output' }, relative_path: 'out.png', mime_type: 'image/png' }
		];
		const view = toLiveTaskView(second, [group('e1', [first]), group('e2', [second])]);
		expect(view.outputs).toHaveLength(1);
		expect(view.outputs[0]).toMatchObject({ key: 'task:task-1::execution:e2', sessionId: 'session-1' });
		expect(view.outputs[0].label).toBeTruthy();
		expect(view.outputs[0].blocks[0]).toMatchObject({ relative_path: 'out.png' });
		expect(view.taskId).toBe('task-1');
		expect(view.executionId).toBe('e2');
	});

	it('has no outputs and no execution id for a bare status message', () => {
		const view = toLiveTaskView(taskMessage('m1', 'running'), []);
		expect(view.outputs).toEqual([]);
		expect(view.executionId).toBeUndefined();
	});
});
