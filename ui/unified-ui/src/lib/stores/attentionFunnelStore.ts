import { getCurrentScopeIdentity } from '$lib/stores/scopeIdentityStore';

export interface AttentionRouteEventSummary {
	event_id: string;
	source_kind: string;
	source_ref: string;
	provider?: string | null;
	account_alias?: string | null;
	source_family: string;
	candidate_key: string;
	stage: string;
	outcome: string;
	lane?: string | null;
	route_reason?: string | null;
	drop_reason?: string | null;
	priority?: string | null;
	confidence?: number | null;
	occurred_at: number;
	created_at: number;
}

export interface AttentionFunnelObservability {
	scope: {
		principal: string;
		workspace: string;
	};
	since_ms?: number | null;
	total_events: number;
	routed_events: number;
	dropped_events: number;
	by_outcome: Record<string, number>;
	by_stage: Record<string, number>;
	by_source_kind: Record<string, number>;
	by_source_family: Record<string, number>;
	by_lane: Record<string, number>;
	by_route_reason: Record<string, number>;
	by_drop_reason: Record<string, number>;
	recent_events: AttentionRouteEventSummary[];
}

export async function fetchAttentionFunnelObservability(
	lookbackHours = 24 * 7
): Promise<AttentionFunnelObservability> {
	const params = new URLSearchParams({
		lookback_hours: String(lookbackHours),
		recent_limit: '12'
	});
	const res = await fetch(`/api/magician/v2/attention-funnel/observability?${params.toString()}`);
	if (!res.ok) {
		let message = `attention funnel request failed (${res.status})`;
		try {
			const body = await res.json();
			if (typeof body?.error === 'string' && body.error.trim()) message = body.error;
		} catch {
			// Keep the status-derived message when the response is not JSON.
		}
		throw new Error(message);
	}
	return (await res.json()) as AttentionFunnelObservability;
}

export function attentionLabel(value: string | null | undefined): string {
	if (!value) return 'Unknown';
	switch (value) {
		case 'needs_you':
			return 'Needs you';
		case 'follow_up':
			return 'Follow-ups';
		case 'worth_a_look':
			return 'Worth a look';
		case 'active_work':
			return 'Active work';
		case 'delivered':
			return 'Delivered';
		case 'changed':
			return 'Changed';
		case 'ingested':
			return 'Ingested';
		case 'distilled':
			return 'Distilled';
		case 'extracted':
			return 'Extracted';
		case 'filtered':
			return 'Filtered';
		case 'routed':
			return 'Routed';
		case 'surfaced':
			return 'Surfaced';
		case 'acted':
			return 'Acted';
		case 'dropped':
			return 'Dropped';
		case 'started':
			return 'Started';
		case 'succeeded':
			return 'Succeeded';
		case 'skipped':
			return 'Skipped';
		case 'failed':
			return 'Failed';
		case 'promise':
			return 'Promise';
		case 'resurfacing':
			return 'Resurfacing';
		case 'memory':
			return 'Memory';
		case 'task':
			return 'Task';
		case 'episode':
			return 'Episode';
		case 'calendar':
			return 'Calendar';
		case 'meeting':
			return 'Meeting';
		case 'comm':
			return 'Communication';
		case 'comms_ingest':
			return 'Comms ingest';
		case 'screen_observation':
			return 'Screen observation';
		case 'tab_observation':
			return 'Tab observation';
		case 'owner_approval_required':
			return 'Owner approval';
		case 'owner_intervention_required':
			return 'Owner intervention';
		case 'promise_or_obligation':
			return 'Promise / obligation';
		case 'actionable_communication':
			return 'Actionable comms';
		case 'failed_work':
			return 'Failed work';
		case 'non_actionable_useful_context':
			return 'Useful context';
		case 'active_follow_up_exists':
			return 'Active follow-up exists';
		case 'action_already_handled':
			return 'Action already handled';
		case 'action_materialization_failed':
			return 'Follow-up creation failed';
		case 'duplicate_of_higher_priority_lane':
			return 'Duplicate higher lane';
		case 'owner_dismissed':
			return 'Dismissed by owner';
		case 'sensitive_or_suppressed':
			return 'Sensitive / suppressed';
		case 'stale_content_revision':
			return 'Stale content revision';
		case 'missing_safe_summary':
			return 'Missing safe summary';
		case 'recency_only':
			return 'Recency only';
		case 'weak_signal':
			return 'Weak signal';
		case 'cooldown_active':
			return 'Cooldown active';
		case 'curator_deferred':
			return 'Curator deferred';
		case 'unsupported_source':
			return 'Unsupported source';
		default:
			return value.replaceAll('_', ' ');
	}
}
