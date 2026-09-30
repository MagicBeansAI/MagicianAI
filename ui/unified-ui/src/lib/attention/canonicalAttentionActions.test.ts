import { describe, expect, it, vi } from 'vitest';

import { executeCanonicalAttentionAction } from './canonicalAttentionActions';
import type { CanonicalAttentionItem, CanonicalAttentionOriginAction } from './canonicalAttentionProjection';

function followUpInWorthLane(): CanonicalAttentionItem {
	return {
		canonical_id: 'follow_up:ann-1', source_revision: 'rev', origin_lane: 'follow_up',
		served_lane: 'worth_a_look', learned_lane: 'worth_a_look', route_reason: 'model', route_applied: true,
		origin: { kind: 'follow_up', annotation_id: 'ann-1', provider: 'gmail', account_alias: 'p', thread_id: 't' },
		group: { cluster_id: 'c-follow', representative_id: 'follow_up:ann-1', member_ids: ['follow_up:ann-1'], member_count: 1 },
		actions: [{ id: 'dismiss', kind: 'dismiss', label: 'Dismiss', method: 'post', href: '/api/magician/v2/channel-assist/annotations/ann-1/dismiss', requires_confirmation: true }],
		payload: { kind: 'follow_up', annotation_id: 'ann-1', subject: null, sender: null, summary: null, label: null, reason: null, received_at: null, open_url: null }
	};
}

function worthInFollowUpLane(): CanonicalAttentionItem {
	return {
		canonical_id: 'worth_a_look:worth-1', source_revision: 'rev', origin_lane: 'worth_a_look',
		served_lane: 'follow_up', learned_lane: 'follow_up', route_reason: 'model', route_applied: true,
		origin: { kind: 'worth_a_look', candidate_id: 'worth-1', source_kind: 'web', source_ref: 'https://example.test' },
		group: { cluster_id: 'c-worth', representative_id: 'worth_a_look:worth-1', member_ids: ['worth_a_look:worth-1'], member_count: 1 },
		actions: [{ id: 'useful', kind: 'useful', label: 'Useful', method: 'post', href: '/api/magician/v2/channel-assist/resurfacing/worth-1/action', requires_confirmation: false }],
		payload: { kind: 'worth_a_look', candidate_id: 'worth-1', line: 'Line', why_now: 'Now', summary: 'Summary', source_title: 'Source', source_kind: 'web', source_ref: 'https://example.test', open_url: null, temporal_anchor_at: null, brief: null }
	};
}

function dependencies() {
	return {
		approveFollowUp: vi.fn(), usefulFollowUp: vi.fn(), acknowledgeFollowUp: vi.fn(),
		dismissFollowUp: vi.fn().mockResolvedValue({ ok: true, feedbackReceipt: null }),
		dismissFollowUpWrongLane: vi.fn().mockResolvedValue({ ok: true, feedbackReceipt: null }),
		snoozeFollowUp: vi.fn(),
		actOnWorth: vi.fn().mockResolvedValue({ ok: true, feedbackReceipt: null })
	};
}

describe('canonical origin action dispatch', () => {
	it('uses the Follow-up lifecycle after a Follow-up origin is served in Worth a look', async () => {
		const item = followUpInWorthLane();
		const deps = dependencies();
		await executeCanonicalAttentionAction(item, item.actions[0], deps);
		expect(deps.dismissFollowUp).toHaveBeenCalledWith('ann-1');
		expect(deps.actOnWorth).not.toHaveBeenCalled();
	});

	it('maps Worth useful to its existing positive-feedback action after routing into Follow-up', async () => {
		const item = worthInFollowUpLane();
		const deps = dependencies();
		await executeCanonicalAttentionAction(item, item.actions[0], deps);
		expect(deps.actOnWorth).toHaveBeenCalledWith('worth-1', 'open');
		expect(deps.dismissFollowUp).not.toHaveBeenCalled();
	});

	it('forwards exact decision/delivery attribution without changing origin dispatch', async () => {
		const item = worthInFollowUpLane();
		const deps = dependencies();
		const attribution = {
			decision_id: 'decision-1', candidate_id: 'worth_a_look:worth-1',
			source_revision: 'rev', delivery_id: 'delivery-1', impression_id: 'impression-1'
		};
		await executeCanonicalAttentionAction(item, item.actions[0], deps, attribution);
		expect(deps.actOnWorth).toHaveBeenCalledWith('worth-1', 'open', attribution);
		expect(deps.dismissFollowUp).not.toHaveBeenCalled();
	});

	it('keeps Worth acknowledge distinct from useful after routing into Follow-up', async () => {
		const item = worthInFollowUpLane();
		item.actions = [{
			id: 'acknowledge', kind: 'acknowledge', label: 'Acknowledge', method: 'post',
			href: '/api/magician/v2/channel-assist/resurfacing/worth-1/action', requires_confirmation: false
		}];
		const deps = dependencies();
		await executeCanonicalAttentionAction(item, item.actions[0], deps);
		expect(deps.actOnWorth).toHaveBeenCalledWith('worth-1', 'acknowledge');
	});

	it('never turns open_source navigation into the Worth positive-feedback label', async () => {
		const item = worthInFollowUpLane();
		item.actions = [{
			id: 'open_source', kind: 'open_source', label: 'Open', method: 'get',
			href: 'https://example.test', requires_confirmation: false
		}];
		const deps = dependencies();
		expect(await executeCanonicalAttentionAction(item, item.actions[0], deps)).toEqual({
			ok: false,
			error: 'Open links must be navigated, not posted.'
		});
		expect(deps.actOnWorth).not.toHaveBeenCalled();
	});

	it('rejects a descriptor that was not supplied with that exact item', async () => {
		const item = worthInFollowUpLane();
		const forged = { ...item.actions[0], href: '/api/forged' } as CanonicalAttentionOriginAction;
		const deps = dependencies();
		expect(await executeCanonicalAttentionAction(item, forged, deps)).toEqual({
			ok: false,
			error: 'Action does not belong to this projection item.'
		});
		expect(deps.actOnWorth).not.toHaveBeenCalled();
	});

	it('rejects a forged confirmation downgrade for an origin action', async () => {
		const item = followUpInWorthLane();
		const forged = { ...item.actions[0], requires_confirmation: false } as CanonicalAttentionOriginAction;
		const deps = dependencies();
		expect(await executeCanonicalAttentionAction(item, forged, deps)).toEqual({
			ok: false,
			error: 'Action does not belong to this projection item.'
		});
		expect(deps.dismissFollowUp).not.toHaveBeenCalled();
	});
});
