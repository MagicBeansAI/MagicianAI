import { describe, expect, it } from 'vitest';

import {
	canonicalAttentionLaneItems,
	canonicalAttentionProjectionFromResponse,
	parseCanonicalAttentionProjection
} from './canonicalAttentionProjection';
import type {
	CanonicalAttentionGroup,
	CanonicalAttentionIntegrity,
	CanonicalAttentionProjection,
	CanonicalFollowUpItem,
	CanonicalWorthItem
} from './canonicalAttentionProjection';

/**
 * A fixture is an unparsed server payload, not an already-parsed projection, so
 * the two integrity flags the parsed type pins to `true` stay boolean here: a
 * payload that admits an incomplete load is exactly what one case below feeds
 * the parser to prove it is rejected whole.
 */
type ProjectionPayload = Omit<CanonicalAttentionProjection, 'integrity' | 'diagnostics'> & {
	diagnostics?: unknown;
	integrity: Omit<CanonicalAttentionIntegrity, 'load_complete' | 'exact_once'> & {
		load_complete: boolean;
		exact_once: boolean;
	};
};

function projection(): ProjectionPayload {
	return {
		schema_version: 1,
		status: 'succeeded',
		projection_id: 'projection-1',
		universe_digest: 'digest-1',
		source_generation_token: null,
		created_at: 1_800_000_000_000,
		policy: {
			mode: 'canary',
			snapshot_id: 'route-1',
			model_version: 'route-model-1',
			seed_identity: 'anonymous:default',
			canary_fraction: 0.1
		},
		integrity: {
			load_complete: true,
			exact_once: true,
			source_total: 2,
			follow_up_source_total: 1,
			worth_a_look_source_total: 1,
			reconciled_total: 2,
			grouped_member_total: 2,
			materialized_total: 2,
			follow_up_lane_total: 1,
			worth_a_look_lane_total: 1,
			non_surfaced_total: 0,
			duplicate_hidden_total: 0,
			unmatched_total: 0,
			fallback_reason: null
		},
		duplicate_aliases: [],
		cross_lane_reconciliation: {
			schema_version: 1,
			status: 'succeeded',
			reason: null,
			authoritative_lane: 'follow_up',
			principal: 'anonymous',
			workspace: 'default',
			follow_up_source_total: 1,
			worth_a_look_source_total: 1,
			raw_source_total: 2,
			unique_source_total: 2,
			duplicate_hidden_total: 0,
			alias_record_total: 0,
			alias_records_returned: 0,
			aliases_truncated: false,
			reconciliation_digest: 'reconciliation-1'
		},
		lanes: {
			follow_up: [worthItem('follow_up')],
			worth_a_look: [followUpItem('worth_a_look')],
			non_surfaced: []
		}
	};
}

function group(id: string): CanonicalAttentionGroup {
	return { cluster_id: `cluster-${id}`, representative_id: id, member_ids: [id], member_count: 1 };
}

function followUpItem(
	served_lane: 'follow_up' | 'worth_a_look' | 'non_surfaced'
): CanonicalFollowUpItem {
	const canonicalId = 'follow_up:ann-1';
	const actionBase = '/api/magician/v2/channel-assist/annotations/ann-1';
	return {
		canonical_id: canonicalId,
		source_revision: 'rev-follow-1',
		origin_lane: 'follow_up',
		served_lane,
		learned_lane: 'worth_a_look',
		route_reason: 'learned_route_applied',
		route_applied: true,
		origin: {
			kind: 'follow_up',
			annotation_id: 'ann-1',
			provider: 'gmail',
			account_alias: 'personal',
			thread_id: 'thread-1'
		},
		group: group(canonicalId),
		actions: [
			{ id: 'open_source', kind: 'open_source', label: 'Open', method: 'get', href: `${actionBase}/message`, requires_confirmation: false },
			{ id: 'approve', kind: 'approve', label: 'Approve', method: 'post', href: `${actionBase}/approve`, requires_confirmation: true },
			{ id: 'acknowledge', kind: 'acknowledge', label: 'Acknowledge', method: 'post', href: `${actionBase}/acknowledge`, requires_confirmation: false },
			{ id: 'useful', kind: 'useful', label: 'Useful', method: 'post', href: `${actionBase}/useful`, requires_confirmation: false },
			{ id: 'dismiss', kind: 'dismiss', label: 'Dismiss', method: 'post', href: `${actionBase}/dismiss`, requires_confirmation: true },
			{ id: 'snooze', kind: 'snooze', label: 'Snooze', method: 'post', href: `${actionBase}/snooze`, requires_confirmation: false }
		],
		payload: {
			kind: 'follow_up',
			annotation_id: 'ann-1',
			subject: 'Reply requested',
			sender: 'A Person',
			summary: 'Please reply by Friday.',
			label: 'needs_reply',
			reason: 'Direct request',
			received_at: 1_800_000_000_000,
			open_url: null
		}
	};
}

function worthItem(
	served_lane: 'follow_up' | 'worth_a_look' | 'non_surfaced'
): CanonicalWorthItem {
	const canonicalId = 'worth_a_look:worth-1';
	const actionEndpoint = '/api/magician/v2/channel-assist/resurfacing/worth-1/action';
	return {
		canonical_id: canonicalId,
		source_revision: 'rev-worth-1',
		origin_lane: 'worth_a_look',
		served_lane,
		learned_lane: 'follow_up',
		route_reason: 'learned_route_applied',
		route_applied: true,
		origin: {
			kind: 'worth_a_look',
			candidate_id: 'worth-1',
			source_kind: 'web',
			source_ref: 'https://example.test/source'
		},
		group: group(canonicalId),
		actions: [
			{ id: 'open_source', kind: 'open_source', label: 'Open', method: 'get', href: 'https://example.test/source', requires_confirmation: false },
			{ id: 'useful', kind: 'useful', label: 'Useful', method: 'post', href: actionEndpoint, requires_confirmation: false },
			{ id: 'acknowledge', kind: 'acknowledge', label: 'Acknowledge', method: 'post', href: actionEndpoint, requires_confirmation: false },
			{ id: 'dismiss', kind: 'dismiss', label: 'Dismiss', method: 'post', href: actionEndpoint, requires_confirmation: true }
		],
		payload: {
			kind: 'worth_a_look',
			candidate_id: 'worth-1',
			line: 'Renewal date is approaching',
			why_now: 'The renewal is next week.',
			summary: 'Review the renewal terms.',
			source_title: 'Account renewal',
			source_kind: 'web',
			source_ref: 'https://example.test/source',
			open_url: 'https://example.test/source',
			temporal_anchor_at: 1_800_000_100_000,
			brief: { schema_version: 1, key_facts: ['Renewal next week'] }
		}
	};
}

describe('canonical attention projection parser', () => {
	it('accepts the source-generation fence and adapts canonical diagnostics without owning lane validity', () => {
		const payload = projection();
		payload.source_generation_token = 'source-generation-1';
		payload.diagnostics = {
			rank_generation: 4,
			rank_scope: 'canonical_eligible_universe',
			follow_up_health: {
				total_active: 1,
				source_family_counts: { promise: 1 },
				embedded_candidates: 1,
				embedding_coverage: 1,
				learned_rank_changes: 0,
				coverage_scope: 'active_cohort',
				rank_policy_version: 'rank-v1',
				semantic_ranking_enabled: true,
				actionability_mode: 'shadow',
				actionability_snapshot_id: 'action-1',
				semantic_extraction_coverage: 1,
				actionability_scored_count: 1,
				actionability_fallback_count: 0
			},
			worth_a_look_health: {},
			grouping_mode: 'shadow',
			grouping_snapshot_id: 'group-1',
			grouping_generation: 3,
			grouping_scope: 'canonical_eligible_universe',
			grouping_health: {
				candidate_total: 2,
				member_total: 2,
				cluster_total: 2,
				representative_total: 2,
				collapsed_member_total: 0,
				scored_pair_total: 1,
				cannot_link_total: 0,
				fallback_ungrouped_total: 0,
				totals_reconcile: true
			},
			decision: {
				decision_id: 'decision-1',
				decided_at: 1_800_000_000_000,
				surface: 'follow_up',
				routing_mode: 'shadow',
				routing_snapshot_id: 'route-1',
				eligible_item_count: 2,
				selected_item_count: 2,
				returned_item_count: 1,
				complete_universe_recorded: true,
				complete_cross_lane_universe: true,
				degradation_reason: null
			},
			impression_policy: {
				min_visible_ms: 1000,
				visibility_rule_version: 'visible-v1'
			},
			routing_health: {
				routing_snapshot_valid: true,
				evaluated_count: 2,
				learned_route_count: 0,
				applied_route_count: 0,
				baseline_retained_count: 2,
				impression_eligible_count: 2,
				decision_item_coverage: 1,
				all_candidates_path: 'canonical'
			},
			bandit_health: null,
			ranks: [],
			decision_items: []
		};

		const parsed = parseCanonicalAttentionProjection(payload);
		expect(parsed.ok).toBe(true);
		if (!parsed.ok) return;
		expect(parsed.projection.source_generation_token).toBe('source-generation-1');
		expect(parsed.projection.diagnostics?.health?.total_active).toBe(1);
		expect(parsed.projection.diagnostics?.actionability?.mode).toBe('shadow');
		expect(parsed.projection.diagnostics?.grouping?.scope).toBe('canonical_eligible_universe');
		expect(parsed.projection.diagnostics?.routing?.decision.decision_id).toBe('decision-1');
	});

	it('accepts a complete exact union and preserves server lane order and cross-origin placement', () => {
		const parsed = parseCanonicalAttentionProjection(projection());
		expect(parsed.ok).toBe(true);
		if (!parsed.ok) return;
		expect(canonicalAttentionLaneItems(parsed.projection, 'follow_up').map((item) => item.canonical_id))
			.toEqual(['worth_a_look:worth-1']);
		expect(parsed.projection.lanes.follow_up[0].origin_lane).toBe('worth_a_look');
		expect(parsed.projection.lanes.worth_a_look[0].origin_lane).toBe('follow_up');
	});

	it('preserves every grouped member and rejects inconsistent member order instead of deduping', () => {
		const value = projection();
		const memberIds = ['worth_a_look:worth-1', 'follow_up:ann-1'];
		for (const item of [value.lanes.follow_up[0], value.lanes.worth_a_look[0]]) {
			item.group = {
				cluster_id: 'shared-cluster',
				representative_id: 'worth_a_look:worth-1',
				member_ids: [...memberIds],
				member_count: 2
			};
		}
		expect(parseCanonicalAttentionProjection(value).ok).toBe(true);
		value.lanes.worth_a_look[0].group.member_ids.reverse();
		expect(parseCanonicalAttentionProjection(value).ok).toBe(false);
	});

	it('accepts a server-owned Worth duplicate alias without materializing the duplicate card', () => {
		const value = projection();
		value.lanes.follow_up = [followUpItem('follow_up')];
		value.lanes.follow_up[0].learned_lane = 'follow_up';
		value.lanes.follow_up[0].route_applied = false;
		value.lanes.follow_up[0].route_reason = 'baseline';
		value.lanes.worth_a_look = [];
		value.integrity.duplicate_hidden_total = 1;
		value.integrity.materialized_total = 1;
		value.integrity.grouped_member_total = 1;
		value.integrity.follow_up_lane_total = 1;
		value.integrity.worth_a_look_lane_total = 0;
		value.duplicate_aliases = [{
			owner_canonical_id: 'follow_up:ann-1',
			duplicate_canonical_id: 'worth_a_look:worth-1',
			reason: 'exact_source_identity'
		}];
		Object.assign(value.cross_lane_reconciliation, {
			unique_source_total: 1,
			duplicate_hidden_total: 1,
			alias_record_total: 1,
			alias_records_returned: 1
		});
		const parsed = parseCanonicalAttentionProjection(value);
		expect(parsed.ok).toBe(true);
		if (!parsed.ok) return;
		expect(parsed.projection.duplicate_aliases).toHaveLength(1);
		expect(parsed.projection.lanes.worth_a_look).toEqual([]);
	});

	it.each([
		['materialized duplicate', (value: any) => value.lanes.worth_a_look = [worthItem('worth_a_look')]],
		['missing owner', (value: any) => value.duplicate_aliases[0].owner_canonical_id = 'follow_up:missing'],
		['self alias', (value: any) => value.duplicate_aliases[0].duplicate_canonical_id = 'follow_up:ann-1'],
		['duplicate alias id', (value: any) => value.duplicate_aliases.push({ ...value.duplicate_aliases[0] })],
		['alias count mismatch', (value: any) => value.cross_lane_reconciliation.alias_records_returned = 0]
	])('rejects duplicate reconciliation for %s', (_label, mutate) => {
		const value: any = projection();
		value.lanes.follow_up = [followUpItem('follow_up')];
		value.lanes.follow_up[0].learned_lane = 'follow_up';
		value.lanes.follow_up[0].route_applied = false;
		value.lanes.follow_up[0].route_reason = 'baseline';
		value.lanes.worth_a_look = [];
		Object.assign(value.integrity, { duplicate_hidden_total: 1, materialized_total: 1, grouped_member_total: 1, follow_up_lane_total: 1, worth_a_look_lane_total: 0 });
		value.duplicate_aliases = [{ owner_canonical_id: 'follow_up:ann-1', duplicate_canonical_id: 'worth_a_look:worth-1', reason: 'exact_source_identity' }];
		Object.assign(value.cross_lane_reconciliation, { unique_source_total: 1, duplicate_hidden_total: 1, alias_record_total: 1, alias_records_returned: 1 });
		mutate(value);
		expect(parseCanonicalAttentionProjection(value).ok).toBe(false);
	});

	it.each([
		['missing lane', (value: any) => delete value.lanes.non_surfaced],
		['partial source reconciliation', (value: any) => value.integrity.reconciled_total = 1],
		['unreconciled hidden duplicate', (value: any) => value.integrity.duplicate_hidden_total = 1],
		['lane count mismatch', (value: any) => value.integrity.follow_up_lane_total = 2],
		['wrong containing lane', (value: any) => value.lanes.follow_up[0].served_lane = 'worth_a_look'],
		['origin/payload mismatch', (value: any) => value.lanes.follow_up[0].payload.kind = 'follow_up'],
		['wrong origin endpoint', (value: any) => value.lanes.follow_up[0].actions[0].href = '/wrong'],
		['raw, unqualified canonical id', (value: any) => value.lanes.follow_up[0].canonical_id = 'worth-1'],
		['group omits its item', (value: any) => value.lanes.follow_up[0].group.member_ids = ['follow_up:ann-1']],
		['baseline route applies', (value: any) => value.policy.mode = 'baseline'],
		['duplicate canonical id', (value: any) => value.lanes.worth_a_look[0].canonical_id = 'worth_a_look:worth-1']
	])('rejects the whole projection for %s', (_label, mutate) => {
		const value = projection();
		mutate(value);
		expect(parseCanonicalAttentionProjection(value).ok).toBe(false);
	});

	it('rejects a typed incomplete fallback atomically instead of accepting empty canonical halves', () => {
		const value = projection();
		value.status = 'baseline_fallback';
		value.integrity.load_complete = false;
		value.integrity.exact_once = false;
		value.integrity.fallback_reason = 'canonical_source_totals_changed';
		value.integrity.source_total = 0;
		value.integrity.follow_up_source_total = 0;
		value.integrity.worth_a_look_source_total = 0;
		value.integrity.reconciled_total = 0;
		value.integrity.grouped_member_total = 0;
		value.integrity.materialized_total = 0;
		value.integrity.follow_up_lane_total = 0;
		value.integrity.worth_a_look_lane_total = 0;
		value.lanes = { follow_up: [], worth_a_look: [], non_surfaced: [] };
		expect(parseCanonicalAttentionProjection(value).ok).toBe(false);
	});

	it('returns null from an additive legacy response when the union is absent or malformed', () => {
		expect(canonicalAttentionProjectionFromResponse({ items: [] })).toBeNull();
		expect(canonicalAttentionProjectionFromResponse({ canonical_attention_projection: {} })).toBeNull();
		expect(
			canonicalAttentionProjectionFromResponse({ canonical_attention_projection: projection() })
		).not.toBeNull();
	});
});
