import { describe, expect, it } from 'vitest';

import {
	approvalHitlOpenTarget,
	clearApprovals,
	getApprovalSnapshot,
	handleCanonicalHitlApprovalEvent,
	type ApprovalSummary
} from './approvalStore';
import { scopeIdentityStore } from './scopeIdentityStore';

describe('approval direct-open target', () => {
	it('uses the durable approval id as the canonical respond key', () => {
		const approval: ApprovalSummary = {
			approval_id: 'approval-4',
			principal: 'owner',
			workspace: 'default',
			agent_id: 'release-agent',
			goal_id: 'goal-1',
			cycle_id: 'cycle-2',
			execution_id: 'exec-9',
			trigger_seq: 3,
			status: 'pending',
			created_at: 1,
			expires_at: 100,
			updated_at: 2,
			pending_action_count: 2,
			pending_actions: [
				{ step_id: 'step-1', action_description: 'Deploy the production release' },
				{ step_id: 'step-2', action_description: 'Notify the release channel' }
			]
		};

		expect(approvalHitlOpenTarget(approval)).toEqual({
			id: 'approval-4',
			source: 'approval',
			input_type: 'confirmation',
			prompt:
				'Approval required for 2 pending actions\n\nPending actions:\n- Deploy the production release\n- Notify the release channel',
			input_schema: { confirm_label: 'Approve', deny_label: 'Reject' },
			identifiers: {
				approval_id: 'approval-4',
				correlation_id: 'approval-4'
			},
			scope: {
				principal: 'owner',
				workspace: 'default',
				execution_id: 'exec-9',
				agent_id: 'release-agent'
			},
			at: 2
		});
	});

	it('does not describe a metadata-only approval as zero actions', () => {
		const approval: ApprovalSummary = {
			approval_id: 'approval-5',
			principal: 'owner',
			workspace: 'default',
			agent_id: 'release-agent',
			goal_id: 'goal-1',
			cycle_id: 'cycle-2',
			trigger_seq: 3,
			status: 'pending',
			created_at: 1,
			expires_at: 100,
			updated_at: 2,
			pending_action_count: 0
		};

		expect(approvalHitlOpenTarget(approval).prompt).toBe(
			'Approval required before the agent can continue'
		);
	});

	it('retains origin scope and action descriptions from canonical events', () => {
		clearApprovals();
		scopeIdentityStore.observe('owner', 'default');
		handleCanonicalHitlApprovalEvent('HitlRequested', {
			correlation_id: 'approval-event-1',
			source: 'approval',
			principal: 'owner',
			workspace: 'default',
			agent_id: 'release-agent',
			execution_id: 'exec-12',
			timestamp: 12,
			input_schema: {
				pending_action_count: 2,
				pending_action_descriptions: ['Deploy release', 'Notify stakeholders']
			}
		});

		const approval = getApprovalSnapshot('approval-event-1');
		expect(approval).toEqual(
			expect.objectContaining({
				principal: 'owner',
				workspace: 'default',
				pending_actions: ['Deploy release', 'Notify stakeholders']
			})
		);
		expect(approval && approvalHitlOpenTarget(approval).prompt).toContain(
			'- Notify stakeholders'
		);
		clearApprovals();
	});
});
