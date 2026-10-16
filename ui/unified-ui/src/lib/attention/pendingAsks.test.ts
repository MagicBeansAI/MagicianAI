import { describe, expect, it } from 'vitest';
import { get, writable } from 'svelte/store';

import type { FeedItem } from '$lib/feed/types';
import type { HitlOpenTarget } from '$lib/hitl/types';
import { promptFor } from '$lib/hitl/respondToHitl';
import { hitlRequestFromOpenTarget } from '$lib/attention/openHitlPrompt';
import type { HitlPendingEntry } from '$lib/stores/pendingHitlStore';
import type { AttentionStoreState } from '$lib/stores/attentionStore';
import type { TaskPanelModel } from '$lib/magician/tasks/UnifiedTaskPanel.svelte';
import {
	collectPendingAsks,
	createPendingAsksStore,
	mergeTaskPanelAsks,
	pendingAsksForTask,
	pendingAsksForThread
} from './pendingAsks';

const REVIEW_HREF = '/work?view=quality&run=run-1&task=task-relentless';

/** The Relentless review ask as the feed carries it: task-scoped, no thread on the ask. */
function relentlessReviewRow(overrides: Partial<FeedItem> = {}): FeedItem {
	return {
		id: 'feed-relentless-1',
		principal: 'owner',
		workspace: 'default',
		item_type: 'task',
		task_id: 'task-relentless',
		ui_thread_id: 'research',
		title: 'Review the accepted draft',
		status: 'needs_action',
		created_at: 1,
		updated_at: 50,
		actions: [],
		metadata: {
			hitl_request: {
				id: 'req-review-1',
				source: 'user_request',
				input_type: 'choice',
				prompt: 'Relentless accepted a draft. Approve it?',
				input_schema: {
					request_type: 'relentless.accepted_review',
					options: [
						{ id: 'approve', label: 'Approve draft' },
						{ id: 'reject', label: 'Reject draft' }
					],
					context: { review_href: REVIEW_HREF, review_label: 'Review draft' }
				},
				identifiers: { request_id: 'req-review-1', correlation_id: 'req-review-1' },
				scope: { principal: 'owner', workspace: 'default', task_id: 'task-relentless' }
			}
		},
		...overrides
	};
}

function liveEntry(): HitlPendingEntry {
	return {
		correlation_id: 'req-review-1',
		at: 40,
		raw: {
			event_type: 'HitlRequested',
			data: {
				correlation_id: 'req-review-1',
				request_id: 'req-review-1',
				source: 'user_request',
				input_type: 'choice',
				prompt: 'Relentless accepted a draft. Approve it?',
				input_schema: {
					options: [
						{ id: 'approve', label: 'Approve draft' },
						{ id: 'reject', label: 'Reject draft' }
					],
					context: { review_href: REVIEW_HREF }
				},
				scope: { principal: 'owner', workspace: 'default', task_id: 'task-relentless' }
			}
		}
	} as HitlPendingEntry;
}

function feed(requests: FeedItem[] = []): Pick<AttentionStoreState, 'requests' | 'approvals' | 'escalations'> {
	return { requests, approvals: [], escalations: [] };
}

describe('collectPendingAsks', () => {
	it('lists a task-only feed ask with the feed row thread and an answerable target', () => {
		const asks = collectPendingAsks([], feed([relentlessReviewRow()]));
		expect(asks).toHaveLength(1);
		expect(asks[0].taskId).toBe('task-relentless');
		expect(asks[0].threadId).toBe('research');
		expect(hitlRequestFromOpenTarget(asks[0].target)).not.toBeNull();
	});

	it('lists an ask in both sources once, keeping the thread only the feed row knows', () => {
		const asks = collectPendingAsks([liveEntry()], feed([relentlessReviewRow()]));
		expect(asks).toHaveLength(1);
		expect(asks[0].threadId).toBe('research');
	});

	it("falls back to the task's thread from the task store", () => {
		const asks = collectPendingAsks(
			[liveEntry()],
			feed(),
			new Map([['task-relentless', 'ops']])
		);
		expect(asks[0].threadId).toBe('ops');
	});

	it('leaves a thread-less ask unthreaded rather than defaulting it to general', () => {
		const asks = collectPendingAsks([liveEntry()], feed());
		expect(asks[0].threadId).toBeNull();
		expect(pendingAsksForThread(asks, 'general')).toEqual([]);
	});

	it('skips failed rows and stalled waits', () => {
		const asks = collectPendingAsks(
			[],
			feed([
				relentlessReviewRow({ status: 'failed' }),
				relentlessReviewRow({
					id: 'stalled',
					metadata: { attention_reason: 'waiting_without_question' }
				})
			])
		);
		expect(asks).toEqual([]);
	});

	it('carries the producer review link and approve/reject options into the prompt', () => {
		const [ask] = collectPendingAsks([], feed([relentlessReviewRow()]));
		expect(ask.target.input_schema?.review_href).toBe(REVIEW_HREF);
		const request = hitlRequestFromOpenTarget(ask.target);
		const prompt = promptFor(request!);
		expect(prompt.review).toEqual({ href: REVIEW_HREF, label: 'Review draft' });
		expect(prompt.choices?.map((choice) => choice.label)).toEqual(['Approve draft', 'Reject draft']);
	});

	it('drops a review link that could leave the origin', () => {
		const row = relentlessReviewRow();
		const embedded = (row.metadata as { hitl_request: { input_schema: { context: Record<string, string> } } })
			.hitl_request;
		embedded.input_schema.context = { review_href: '//evil.example/x' };
		const [ask] = collectPendingAsks([], feed([row]));
		expect(ask.target.input_schema?.review_href).toBeUndefined();
	});
});

describe('pendingAsksForThread (composer dock)', () => {
	it('shows a task-only ask in the thread its feed row names', () => {
		const asks = collectPendingAsks([liveEntry()], feed([relentlessReviewRow()]));
		expect(pendingAsksForThread(asks, 'Research').map((target) => target.id)).toEqual(['req-review-1']);
		expect(pendingAsksForThread(asks, 'general')).toEqual([]);
	});

	it("shows an ask whose task sits in the thread's tasks", () => {
		const asks = collectPendingAsks([liveEntry()], feed());
		expect(
			pendingAsksForThread(asks, 'general', { threadTaskIds: new Set(['task-relentless']) })
		).toHaveLength(1);
	});

	it('leaves a plan approval to the planner dock', () => {
		const asks = collectPendingAsks([liveEntry()], feed([relentlessReviewRow()])).map((ask) => ({
			...ask,
			target: { ...ask.target, source: 'plan_approval' as const }
		}));
		expect(
			pendingAsksForThread(asks, 'research', { planApprovalTaskIds: new Set(['task-relentless']) })
		).toEqual([]);
	});
});

describe('mergeTaskPanelAsks (deep panel)', () => {
	const model = {
		id: 'task-relentless',
		status: 'running',
		attention: null,
		ask: null,
		moreAsks: [],
		error: null,
		queuedFor: null,
		currentStep: null,
		totalSteps: null,
		currentStepLabel: null,
		elapsedMs: null,
		lastProgressAt: null,
		plan: null,
		run: { steps: [], timeline: null, responsibility: null, provenance: [] },
		output: null,
		runs: null
	} as unknown as TaskPanelModel;

	it('promotes the pending ask to the panel ask with a waiting attention', () => {
		const targets = pendingAsksForTask(
			collectPendingAsks([], feed([relentlessReviewRow()])),
			'task-relentless'
		);
		const merged = mergeTaskPanelAsks(model, targets);
		expect(merged?.ask?.id).toBe('req-review-1');
		expect(merged?.attention?.source).toBe('user_request');
	});

	it('does not repeat an ask the run payload already lists, matched on aliases', () => {
		const [target] = pendingAsksForTask(
			collectPendingAsks([], feed([relentlessReviewRow()])),
			'task-relentless'
		);
		const payloadAsk: HitlOpenTarget = { ...target, id: 'payload-id' };
		const withAsk = { ...model, ask: payloadAsk };
		expect(mergeTaskPanelAsks(withAsk, [target])).toBe(withAsk);
	});

	it('lists further asks under moreAsks', () => {
		const [target] = pendingAsksForTask(
			collectPendingAsks([], feed([relentlessReviewRow()])),
			'task-relentless'
		);
		const other: HitlOpenTarget = {
			...target,
			id: 'req-2',
			identifiers: { request_id: 'req-2', correlation_id: 'req-2' }
		};
		const merged = mergeTaskPanelAsks(model, [target, other]);
		expect(merged?.ask?.id).toBe('req-review-1');
		expect(merged?.moreAsks?.map((ask) => ask.id)).toEqual(['req-2']);
	});
});

describe('createPendingAsksStore', () => {
	it('recomputes when a source changes (HitlResolved drops the entry)', () => {
		const entries = writable<HitlPendingEntry[]>([liveEntry()]);
		const attention = writable({ ...feed(), failed: [] } as unknown as AttentionStoreState);
		const tasks = writable({ tasks: [] as { id: string; uiThreadId?: string }[] });
		const store = createPendingAsksStore(entries, attention, tasks);
		expect(get(store)).toHaveLength(1);
		entries.set([]);
		expect(get(store)).toEqual([]);
	});
});
