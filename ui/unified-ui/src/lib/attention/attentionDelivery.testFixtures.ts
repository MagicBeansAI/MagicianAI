import type {
	AttentionDeliveryPageResponse,
	AttentionDeliveryRootDecision
} from './attentionDelivery';
import type {
	CanonicalAttentionItem,
	CanonicalAttentionLane,
	CanonicalAttentionOriginAction,
	CanonicalAttentionOriginLane,
	CanonicalAttentionProjection
} from './canonicalAttentionProjection';

function followUpActions(annotationId: string): CanonicalAttentionOriginAction[] {
	const base = `/api/magician/v2/channel-assist/annotations/${encodeURIComponent(annotationId)}`;
	return [
		{ id: 'open_source', kind: 'open_source', label: 'Open', method: 'get', href: `${base}/message`, requires_confirmation: false },
		{ id: 'approve', kind: 'approve', label: 'Approve', method: 'post', href: `${base}/approve`, requires_confirmation: true },
		{ id: 'acknowledge', kind: 'acknowledge', label: 'Acknowledge', method: 'post', href: `${base}/acknowledge`, requires_confirmation: false },
		{ id: 'useful', kind: 'useful', label: 'Useful', method: 'post', href: `${base}/useful`, requires_confirmation: false },
		{ id: 'dismiss', kind: 'dismiss', label: 'Dismiss', method: 'post', href: `${base}/dismiss`, requires_confirmation: true },
		{ id: 'snooze', kind: 'snooze', label: 'Snooze', method: 'post', href: `${base}/snooze`, requires_confirmation: false }
	];
}

function worthActions(candidateId: string, sourceRef: string): CanonicalAttentionOriginAction[] {
	const base = `/api/magician/v2/channel-assist/resurfacing/${encodeURIComponent(candidateId)}`;
	return [
		{ id: 'open_source', kind: 'open_source', label: 'Open', method: 'get', href: sourceRef, requires_confirmation: false },
		{ id: 'useful', kind: 'useful', label: 'Useful', method: 'post', href: `${base}/action`, requires_confirmation: false },
		{ id: 'acknowledge', kind: 'acknowledge', label: 'Acknowledge', method: 'post', href: `${base}/action`, requires_confirmation: false },
		{ id: 'dismiss', kind: 'dismiss', label: 'Dismiss', method: 'post', href: `${base}/action`, requires_confirmation: true }
	];
}

export function deliveryFollowUpItem(
	index: number,
	servedLane: CanonicalAttentionLane = 'follow_up'
): CanonicalAttentionItem {
	const annotationId = `ann-${index}`;
	const canonicalId = `follow_up:${annotationId}`;
	return {
		canonical_id: canonicalId,
		source_revision: `revision-${index}`,
		origin_lane: 'follow_up',
		served_lane: servedLane,
		learned_lane: servedLane,
		route_reason: 'baseline_mode',
		route_applied: false,
		origin: {
			kind: 'follow_up',
			annotation_id: annotationId,
			provider: 'gmail',
			account_alias: 'primary',
			thread_id: `thread-${index}`
		},
		group: {
			cluster_id: `cluster-${index}`,
			representative_id: canonicalId,
			member_ids: [canonicalId],
			member_count: 1
		},
		actions: followUpActions(annotationId),
		payload: {
			kind: 'follow_up',
			annotation_id: annotationId,
			subject: `Server item ${index}`,
			sender: 'sender@example.test',
			summary: `Summary ${index}`,
			label: 'needs_reply',
			reason: null,
			received_at: index,
			due_text: null,
			due_at: null,
			open_url: null
		}
	};
}

export function deliveryWorthItem(
	index: number,
	servedLane: CanonicalAttentionLane = 'follow_up'
): CanonicalAttentionItem {
	const candidateId = `worth-${index}`;
	const canonicalId = `worth_a_look:${candidateId}`;
	const sourceRef = `https://example.test/${index}`;
	return {
		canonical_id: canonicalId,
		source_revision: null,
		origin_lane: 'worth_a_look',
		served_lane: servedLane,
		learned_lane: servedLane,
		route_reason: 'baseline_mode',
		route_applied: false,
		origin: { kind: 'worth_a_look', candidate_id: candidateId, source_kind: 'web', source_ref: sourceRef },
		group: {
			cluster_id: `cluster-worth-${index}`,
			representative_id: canonicalId,
			member_ids: [canonicalId],
			member_count: 1
		},
		actions: worthActions(candidateId, sourceRef),
		payload: {
			kind: 'worth_a_look',
			candidate_id: candidateId,
			line: `Worth server item ${index}`,
			why_now: `Why ${index}`,
			summary: `Worth summary ${index}`,
			source_title: `Source ${index}`,
			source_kind: 'web',
			source_ref: sourceRef,
			open_url: sourceRef,
			temporal_anchor_at: index,
			brief: null
		}
	};
}

export function deliveryProjection(
	items: CanonicalAttentionItem[] = [deliveryFollowUpItem(1), deliveryFollowUpItem(2)]
): CanonicalAttentionProjection {
	const followUp = items.filter((item) => item.served_lane === 'follow_up');
	const worth = items.filter((item) => item.served_lane === 'worth_a_look');
	const nonSurfaced = items.filter((item) => item.served_lane === 'non_surfaced');
	const followOrigins = items.filter((item) => item.origin_lane === 'follow_up').length;
	const worthOrigins = items.length - followOrigins;
	return {
		schema_version: 1,
		status: 'baseline_fallback',
		projection_id: 'projection-delivery-1',
		universe_digest: 'universe-digest-1',
		source_generation_token: null,
		created_at: Date.now() - 2_000,
		policy: {
			mode: 'baseline',
			snapshot_id: null,
			model_version: null,
			seed_identity: 'anonymous:default',
			canary_fraction: 0
		},
		integrity: {
			load_complete: true,
			exact_once: true,
			source_total: items.length,
			follow_up_source_total: followOrigins,
			worth_a_look_source_total: worthOrigins,
			reconciled_total: items.length,
			grouped_member_total: items.length,
			materialized_total: items.length,
			follow_up_lane_total: followUp.length,
			worth_a_look_lane_total: worth.length,
			non_surfaced_total: nonSurfaced.length,
			duplicate_hidden_total: 0,
			unmatched_total: 0,
			fallback_reason: 'bandit_disabled'
		},
		duplicate_aliases: [],
		diagnostics: null,
		cross_lane_reconciliation: {
			schema_version: 1,
			status: 'succeeded',
			reason: null,
			authoritative_lane: 'follow_up',
			principal: 'anonymous',
			workspace: 'default',
			follow_up_source_total: followOrigins,
			worth_a_look_source_total: worthOrigins,
			raw_source_total: items.length,
			unique_source_total: items.length,
			duplicate_hidden_total: 0,
			alias_record_total: 0,
			alias_records_returned: 0,
			aliases_truncated: false,
			reconciliation_digest: 'reconciliation-delivery-1'
		},
		lanes: { follow_up: followUp, worth_a_look: worth, non_surfaced: nonSurfaced }
	};
}

export function deliveryRoot(
	projection: CanonicalAttentionProjection,
	lane: CanonicalAttentionOriginLane = 'follow_up'
): AttentionDeliveryRootDecision {
	return {
		decision_id: 'root-decision-1',
		lane,
		projection_id: projection.projection_id,
		universe_digest: projection.universe_digest,
		policy_snapshot_id: projection.policy.snapshot_id,
		policy_model_version: projection.policy.model_version,
		posterior_version: 0,
		seed_identity: 'baseline',
		universe_size: projection.lanes[lane].length,
		created_at: Date.now() - 1_000,
		expires_at: Date.now() + 60_000
	};
}

export function deliveryPage(args: {
	root: AttentionDeliveryRootDecision;
	items: CanonicalAttentionItem[];
	pageIndex: number;
	pageStart: number;
	pageSize: number;
	cursor: string | null;
	nextCursor: string | null;
	deliveryId?: string;
}): AttentionDeliveryPageResponse {
	const delivered = args.pageStart + args.items.length;
	return {
		schema_version: 1,
		status: 'baseline_fallback',
		fallback_reason: 'bandit_disabled',
		root_decision: args.root,
		page: {
			delivery_id: args.deliveryId ?? `delivery-${args.pageIndex}`,
			page_index: args.pageIndex,
			page_start: args.pageStart,
			page_size: args.pageSize,
			cursor: args.cursor,
			next_cursor: args.nextCursor,
			has_more: args.nextCursor !== null,
			expires_at: args.root.expires_at
		},
		items: args.items.map((item, index) => ({
			position: args.pageStart + index + 1,
			candidate_id: item.canonical_id,
			source_revision: item.source_revision,
			root_policy_propensity: 1,
			conditional_delivery_propensity: 1,
			exposure_token: `exposure-${args.pageStart + index + 1}`,
			item
		})),
		impression_policy: { min_visible_ms: 750, visibility_rule_version: 'delivery-visible-v1' },
		health: {
			bandit_mode: 'disabled',
			canary_assigned: false,
			applied: false,
			baseline_preserved: true,
			complete_universe_recorded: true,
			propensity_coverage: 1,
			degradation_reason: 'bandit_disabled',
			root_sample_count: 0,
			delivered_count: delivered,
			remaining_count: args.root.universe_size - delivered,
			exact_revision_match: true,
			replay: args.pageIndex > 0
		}
	};
}
