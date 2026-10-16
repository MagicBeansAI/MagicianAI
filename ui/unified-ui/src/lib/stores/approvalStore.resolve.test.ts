import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const timedFetch = vi.fn();
vi.mock('$lib/shared/fetch', () => ({
	timedFetch: (input: RequestInfo | URL, init?: RequestInit) => timedFetch(input, init),
	DEFAULT_FETCH_TIMEOUT_MS: 30_000,
	LONG_FETCH_TIMEOUT_MS: 600_000
}));

import {
	clearApprovals,
	getApprovalSnapshot,
	handleCanonicalHitlApprovalEvent,
	resolveApproval
} from './approvalStore';
import { scopeIdentityStore } from './scopeIdentityStore';

function response(status: number, body: Record<string, unknown>): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json' }
	});
}

function seedPending(): void {
	handleCanonicalHitlApprovalEvent('HitlRequested', {
		correlation_id: 'approval-7',
		source: 'approval',
		agent_id: 'release-agent',
		principal: 'owner',
		workspace: 'default',
		timestamp: 10,
		input_schema: {
			goal_id: 'goal-1',
			cycle_id: 'cycle-1',
			pending_action_count: 1
		}
	});
}

function approvalDetails(status: 'approved' | 'rejected'): Record<string, unknown> {
	return {
		request: {
			approval_id: 'approval-7',
			principal: 'owner',
			workspace: 'default',
			agent_id: 'release-agent',
			goal_id: 'goal-1',
			cycle_id: 'cycle-1',
			trigger_seq: 1,
			status,
			created_at: 1,
			expires_at: 100,
			resolved_at: 20,
			pending_actions: []
		},
		deliveries: []
	};
}

describe('approval resolution reconciliation', () => {
	beforeEach(() => {
		timedFetch.mockReset();
		clearApprovals();
		scopeIdentityStore.observe('owner', 'default');
		seedPending();
	});

	afterEach(() => clearApprovals());

	it('uses a reversible validating shadow until canonical acceptance', async () => {
		let releaseResponse: ((value: Response) => void) | undefined;
		timedFetch
			.mockReturnValueOnce(new Promise<Response>((resolve) => (releaseResponse = resolve)))
			.mockRejectedValueOnce(new Error('detail refresh unavailable'));

		const resolving = resolveApproval('approval-7', 'approve');
		expect(getApprovalSnapshot('approval-7')?.status).toBe('validating');
		releaseResponse?.(response(200, { accepted: true, source: 'approval' }));

		expect(await resolving).toEqual({ resolved: true, status: 'approved' });
		expect(getApprovalSnapshot('approval-7')?.status).toBe('approved');
	});

	it('reconciles a conflicting response to the authoritative terminal state', async () => {
		timedFetch
			.mockResolvedValueOnce(response(409, { accepted: false, reason: 'already_resolved' }))
			.mockResolvedValueOnce(response(200, approvalDetails('rejected')));

		const result = await resolveApproval('approval-7', 'approve');

		expect(result).toEqual({ resolved: false, status: 'rejected' });
		expect(getApprovalSnapshot('approval-7')?.status).toBe('rejected');
	});
});
