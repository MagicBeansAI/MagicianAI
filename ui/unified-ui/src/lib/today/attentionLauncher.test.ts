import { describe, expect, it } from 'vitest';

import { todayAttentionItemId, todayHitlOpenTarget } from './attentionLauncher';
import type { TodayItem } from './types';

function todayItem(overrides: Partial<TodayItem> = {}): TodayItem {
	return {
		id: 'today:needs_you:v3:attention:feed-row',
		principal: 'alpha',
		workspace: 'prod',
		section: 'needs_you',
		priority: 1,
		title: 'Needs review',
		reason: 'Waiting',
		source_kind: 'approval',
		source_id: 'proposal-id',
		space_ids: [],
		status: 'needs_action',
		actions: [],
		evidence_refs: [],
		created_at: 1,
		updated_at: 2,
		metadata: {},
		...overrides
	};
}

describe('todayAttentionItemId', () => {
	it('prefers an exact item encoded in the source URL', () => {
		expect(
			todayAttentionItemId(
				todayItem({
					source_url: '/attention?attention_item=pause-route-1',
					metadata: { pause_state_id: 'pause-metadata-1' }
				})
			)
		).toBe('pause-route-1');
	});

	it('prefers canonical metadata aliases', () => {
		expect(
			todayAttentionItemId(
				todayItem({ metadata: { pause_state_id: 'pause-1', dedupe_key: 'feed-1' } })
			)
		).toBe('pause-1');
	});

	it('opens the Skill Evolution feed row instead of its proposal source id', () => {
		expect(
			todayAttentionItemId(
				todayItem({
					id: 'today:needs_you:skill_evolution_approval:proposal_review:candidate-1',
					source_id: 'proposal-1',
					metadata: {
						dedupe_key: 'skill_evolution_approval:proposal_review:candidate-1'
					}
				})
			)
		).toBe('skill_evolution_approval:proposal_review:candidate-1');
	});

	it('derives the underlying feed id before considering source_id', () => {
		expect(todayAttentionItemId(todayItem())).toBe('v3:attention:feed-row');
	});

	it('rejects arbitrary source ids but accepts known attention feed ids', () => {
		expect(todayAttentionItemId(todayItem({ id: 'synthetic', source_id: 'proposal-1' }))).toBeNull();
		expect(
			todayAttentionItemId(
				todayItem({ id: 'synthetic', source_id: 'skill_evolution_rollback:rollback-1' })
			)
		).toBe('skill_evolution_rollback:rollback-1');
	});
});

describe('todayHitlOpenTarget', () => {
	it('opens from the embedded backend request instead of the projected feed id', () => {
		const target = todayHitlOpenTarget(
			todayItem({
				metadata: {
					hitl_request: {
						id: 'question-4',
						source: 'clarification',
						input_type: 'choice',
						prompt: 'Which region?',
						input_schema: {
							options: [{ id: 'in', label: 'India' }]
						},
						identifiers: { correlation_id: 'question-4' },
						scope: {
							principal: 'alpha',
							workspace: 'prod',
							workflow_id: 'task-9',
							task_id: 'task-9',
							execution_id: 'planexec-9'
						}
					}
				}
			})
		);

		expect(target).toEqual(
			expect.objectContaining({ id: 'question-4', source: 'clarification' })
		);
	});

	it('adapts legacy approval metadata without requiring a loaded Attention row', () => {
		const target = todayHitlOpenTarget(
			todayItem({
				id: 'today:needs_you:approval-2',
				source_kind: 'approval',
				title: 'Approve deployment',
				metadata: { approval_id: 'approval-2', attention_kind: 'approval' }
			})
		);

		expect(target).toEqual(
			expect.objectContaining({
				id: 'approval-2',
				source: 'approval',
				input_type: 'confirmation'
			})
		);
	});
});
