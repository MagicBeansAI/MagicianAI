import { describe, expect, it } from 'vitest';

import type { FeedItem } from '$lib/feed/types';
import type { HitlRequest } from '$lib/hitl/types';
import {
	attentionBusRow,
	attentionFeedRow,
	attentionCategoryCountsFromTotals,
	attentionFeedLanesForCategory,
	attentionRowAliases,
	attentionRowCategory,
	attentionRowOperationalCategory,
	filterAttentionRows,
	filterAttentionRowsByCategory,
	mergeAttentionRows,
	type AttentionDisplayRow
} from './model';

function request(overrides: Partial<HitlRequest> = {}): HitlRequest {
	return {
		id: 'pause-1',
		source: 'agentic',
		input_type: 'text',
		schema: {},
		prompt: 'Need input',
		scope: { execution_id: 'execution-1' },
		identifiers: { pause_state_id: 'pause-1', correlation_id: 'correlation-1' },
		...overrides
	};
}

function row(overrides: Partial<AttentionDisplayRow> = {}): AttentionDisplayRow {
	const hitl = request();
	return {
		key: hitl.id,
		source: hitl.source,
		prompt: hitl.prompt,
		scope: hitl.scope,
		at: 10,
		correlation_id: 'correlation-1',
		origin: 'bus',
		request: hitl,
		pause_state_id: 'pause-1',
		...overrides
	};
}

function feedItem(overrides: Partial<FeedItem> = {}): FeedItem {
	return {
		id: 'v3:attention:item-1',
		principal: 'anonymous',
		workspace: 'default',
		item_type: 'escalation',
		title: 'Feed prompt',
		status: 'needs_action',
		created_at: 1,
		updated_at: 2,
		actions: [],
		metadata: {
			attention_kind: 'input.requested',
			input_type: 'text',
			pause_state_id: 'pause-1',
			correlation_id: 'correlation-1',
			execution_id: 'execution-1'
		},
		...overrides
	};
}

describe('Attention row model', () => {
	it('opens producer-supplied local review context from both canonical and feed requests', () => {
		const href = '/work?view=quality&run=run-1&task=task-1';
		const input_schema = { options: [{ id: 'dismiss', label: 'Dismiss notice' }],
			context: { review_href: href, review_label: 'Review draft' } };
		const metadata = { attention_kind: 'user_request.pending', source: 'user_request', correlation_id: 'review-1', request_id: 'review-1',
			input_type: 'choice', input_schema, principal: 'anonymous', workspace: 'default' };
		const bus = attentionBusRow({ correlation_id: 'review-1', at: 1,
			raw: { event_type: 'HitlRequested', ...metadata, prompt: 'Review needed' } });
		const feed = attentionFeedRow(feedItem({ metadata }));
		for (const normalized of [bus, feed]) {
			expect(normalized).toMatchObject({ review_href: href, review_label: 'Review draft' });
			expect(normalized?.request?.schema.options).toEqual(input_schema.options);
		}
	});

	it.each(['https://example.com/review', '//example.com/review', '/\\example.com/review', '/ review', '/\nreview'])
	('rejects non-local or malformed review context %j', (href) => {
		const normalized = attentionBusRow({ correlation_id: 'review-1', at: 1, raw: {
			event_type: 'HitlRequested', source: 'user_request', correlation_id: 'review-1',
			principal: 'anonymous', workspace: 'default', input_type: 'choice', prompt: 'Review',
			input_schema: { options: [{ id: 'dismiss', label: 'Dismiss' }], context: { review_href: href } }
		} });
		expect(normalized).not.toBeNull();
		expect(normalized?.review_href).toBeUndefined();
	});

	it('keeps the bus row when feed and bus use different primary aliases', () => {
		const bus = row({ alias_ids: ['shared-correlation'] });
		const feed = row({
			key: 'approval-1',
			origin: 'feed',
			request: null,
			pause_state_id: undefined,
			correlation_id: 'approval-1',
			alias_ids: ['shared-correlation'],
			prompt: 'Older feed projection'
		});

		const [merged] = mergeAttentionRows([bus], [feed]);

		expect(merged).toMatchObject({
			key: bus.key,
			origin: 'bus',
			prompt: bus.prompt,
			request: bus.request
		});
		expect(attentionRowAliases(merged)).toContain('approval-1');
	});

	it('closes aliases transitively across skipped feed rows', () => {
		const busA = row({
			key: 'A',
			correlation_id: 'A',
			alias_ids: [],
			request: null,
			pause_state_id: undefined
		});
		const feedAB = row({
			key: 'A',
			correlation_id: 'A',
			alias_ids: ['B'],
			origin: 'feed',
			request: null,
			pause_state_id: undefined
		});
		const feedB = row({
			key: 'B',
			correlation_id: 'B',
			alias_ids: [],
			origin: 'feed',
			request: null,
			pause_state_id: undefined
		});

		const [merged] = mergeAttentionRows([busA], [feedAB, feedB]);

		expect(merged).toMatchObject({
			key: 'A',
			origin: 'bus',
			prompt: busA.prompt
		});
		expect(attentionRowAliases(merged)).toEqual(new Set(['A', 'B']));
	});

	it('retains the raw FeedItem id used by Feed, Today, and Square launchers', () => {
		const bus = row();
		const feed = attentionFeedRow(
			feedItem({
				id: 'v3:attention:raw-launcher-id',
				metadata: {
					attention_kind: 'input.requested',
					input_type: 'text',
					pause_state_id: 'pause-1',
					correlation_id: 'correlation-1',
					execution_id: 'execution-1'
				}
			})
		);

		const [merged] = mergeAttentionRows([bus], [feed]);

		expect(merged.origin).toBe('bus');
		expect(attentionRowAliases(merged)).toContain('v3:attention:raw-launcher-id');
	});

	it('preserves explicit metadata.source on the row and adapted request', () => {
		const normalized = attentionFeedRow(
			feedItem({
				metadata: {
					attention_kind: 'input.requested',
					input_type: 'text',
					source: 'clarification',
					pause_state_id: 'question-1',
					correlation_id: 'question-1',
					execution_id: 'task-1'
				}
			})
		);

		expect(normalized.source).toBe('clarification');
		expect(normalized.request?.source).toBe('clarification');
	});

	it('retains the raw feed id as an alias for durable failed dismissal', () => {
		const normalized = attentionFeedRow(
			feedItem({ status: 'failed', metadata: { correlation_id: 'failed-correlation' } })
		);

		expect(normalized.feed_item_id).toBe('v3:attention:item-1');
		expect(attentionRowAliases(normalized)).toContain('v3:attention:item-1');
	});

	it('maps detailed sources onto the cursor-backed operational categories', () => {
		expect(attentionRowOperationalCategory(row({ source: 'plan_approval' }))).toBe('approvals');
		expect(attentionRowOperationalCategory(row({ source: 'escalation' }))).toBe('escalations');
		expect(
			attentionRowOperationalCategory(row({ source: 'clarification', failed: true }))
		).toBe('failed');
		expect(attentionFeedLanesForCategory('failed')).toEqual(['requests', 'failed']);
		expect(attentionFeedLanesForCategory('all')).toEqual([
			'requests',
			'approvals',
			'escalations',
			'failed'
		]);
	});

	it('builds category counters from server totals instead of loaded rows', () => {
		expect(
			attentionCategoryCountsFromTotals({
				requests: 7,
				approvals: 3,
				escalations: 2,
				failed: 1,
				running: 99
			})
		).toEqual({
			// `all` must equal what the badge counts and what the `all` list
			// renders, or the badge describes a different set than the view it
			// opens.
			all: 13,
			requests: 7,
			approvals: 3,
			escalations: 2,
			failed: 1
		});
	});
});

describe('C2/C3 attention rows', () => {
	it('renders a stalled wait as an escalation with Cancel and Open task, not an ask', () => {
		const stalled = attentionFeedRow(
			feedItem({
				task_id: 'task-7',
				title: 'Landlord email',
				metadata: {
					attention_reason: 'waiting_without_question',
					execution_id: 'exec-7',
					actions: ['cancel', 'open_task']
				}
			}),
			'escalations'
		);
		expect(stalled.request).toBeNull();
		expect(stalled.source).toBe('escalation');
		expect(stalled.category).toBe('escalations');
		expect(stalled.prompt).toBe('Waiting on you, but no question was delivered');
		expect(stalled.stalledWait).toEqual({
			executionId: 'exec-7',
			taskId: 'task-7',
			actions: ['cancel', 'open_task']
		});
		expect(stalled.review_href).toBe('/work?filter=all&selected=task-7');
		expect(stalled.review_label).toBe('Open task');
	});

	it('labels a delegated child ask and keeps the child execution', () => {
		const child = attentionFeedRow(
			feedItem({
				task_id: 'parent-task',
				metadata: {
					attention_kind: 'input.requested',
					input_type: 'text',
					pause_state_id: 'pause-child',
					execution_id: 'exec-child',
					delegation_child: true,
					delegated_agent_id: 'researcher'
				}
			})
		);
		expect(child.delegated_agent_id).toBe('researcher');
		expect(child.execution_id).toBe('exec-child');
		expect(child.request?.scope.execution_id).toBe('exec-child');
	});

	it('a delivered question suppresses only its scoped execution stalled warning', () => {
		const stalled = attentionFeedRow(feedItem({
			id: 'stalled', task_id: 'task-7', metadata: {
				attention_reason: 'waiting_without_question', execution_id: 'exec-7', actions: ['cancel', 'open_task']
			}
		}), 'escalations');
		const scope = { principal: 'anonymous', workspace: 'default', task_id: 'task-7', execution_id: 'exec-7' };
		const ask = row({ key: 'delivered', correlation_id: 'delivered', pause_state_id: 'delivered', scope,
			request: request({ scope, identifiers: { correlation_id: 'delivered' } }) });
		expect(mergeAttentionRows([ask], [stalled])).toEqual([ask]);
		for (const changed of [{ execution_id: 'other-execution' }, { workspace: 'other-workspace' }, { principal: 'other-owner' }]) {
			const otherScope = { ...scope, ...changed };
			const other = { ...ask, scope: otherScope, request: request({ scope: otherScope }) };
			expect(mergeAttentionRows([other], [stalled])).toHaveLength(2);
		}
		expect(mergeAttentionRows([], [stalled])).toEqual([stalled]);
		expect(mergeAttentionRows([{ ...ask, stale: true }], [stalled])).toHaveLength(2);
		expect(mergeAttentionRows([{ ...ask, failed: true }], [stalled])).toHaveLength(2);
	});

	it('carries the delegation label onto the bus row that wins the merge', () => {
		const bus = row({ key: 'pause-child', pause_state_id: 'pause-child', request: request({ id: 'pause-child', identifiers: { pause_state_id: 'pause-child' } }) });
		const feed = row({ key: 'pause-child', origin: 'feed', pause_state_id: 'pause-child', delegated_agent_id: 'researcher' });
		const [merged] = mergeAttentionRows([bus], [feed]);
		expect(merged.origin).toBe('bus');
		expect(merged.delegated_agent_id).toBe('researcher');
		expect(merged.request?.delegated_agent_id).toBe('researcher');
	});
});
