import { describe, expect, it } from 'vitest';

import type { FeedItem } from '$lib/feed/types';
import type { HitlRequest } from '$lib/hitl/types';
import {
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
