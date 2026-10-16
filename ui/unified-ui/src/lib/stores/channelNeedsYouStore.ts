/**
 * Channel Assist Follow-ups surface. Client for the
 * `/channel-assist/follow-ups` endpoint (the classifier's `needs_approval`
 * annotations) plus the approve / snooze / dismiss actions. Body-blind: cards
 * carry only metadata + the classifier's label/reason, never a message body.
 */

import {
	parseAttentionFeedbackReceipt,
	parseAttentionActionabilityCard,
	parseAttentionActionabilityPage,
	parseAttentionActionabilityTraining,
	parseChannelFollowUpLearningHealth,
	type AttentionActionabilityCard,
	type AttentionActionabilityPage,
	type AttentionActionabilityTrainingStatus,
	type AttentionFeedbackReceipt,
	type ChannelFollowUpDismissReason,
	type ChannelFollowUpLearningHealth
} from '$lib/channel/channelFollowUpLearning';
import {
	parseAttentionGroupingMetadata,
	parseAttentionGroupingPage,
	type AttentionGroupingMetadata,
	type AttentionGroupingPage
} from '$lib/attention/attentionGrouping';
import {
	canonicalCandidateId,
	parseAttentionDecisionItem,
	parseAttentionRoutingPage,
	type AttentionDecisionItem,
	type AttentionRoutingPage
} from '$lib/attention/attentionRouting';
import {
	parseAttentionBanditDecision,
	parseAttentionBanditHealth,
	type AttentionBanditDecision,
	type AttentionBanditHealth,
	type AttentionFeedbackAttribution
} from '$lib/attention/attentionBandit';
import { attentionRankRecomputeStore } from '$lib/stores/attentionRankRecomputeStore';
import {
	parseAttentionSemanticExtractionHealth,
	type AttentionSemanticExtractionHealth
} from '$lib/attention/attentionSemanticExtraction';
import {
	canonicalAttentionProjectionFromResponse,
	type CanonicalAttentionProjection
} from '$lib/attention/canonicalAttentionProjection';

export type ChannelFollowUpLane = 'user_assist' | 'envoy';

export interface ChannelFollowUp {
	annotation_id: string;
	/** Stable attention candidate identity and exact source revision used for
	 * revision-bound pair corrections. Older responses may omit both. */
	candidate_id?: string;
	source_revision?: string | null;
	/** 'gmail' | 'agentmail' | 'whatsapp' | 'whatsapp_kapso'. */
	provider: string;
	account_alias: string;
	/** The mailbox's own address — which account the thread lives in. */
	account_email: string | null;
	thread_id: string;
	lane: ChannelFollowUpLane;
	state?: string;
	/** Newer evidence invalidated a pending/inserted draft. */
	review_required?: boolean;
	/** Debug provenance inside the unified Follow-ups surface. */
	source_family?: 'promise' | 'comms_ingest' | string;
	/** 'needs_reply' | 'follow_up' (the actionable labels that surface here). */
	label: string | null;
	confidence: number | null;
	reason: string | null;
	proposed_action: unknown;
	subject: string | null;
	sender: string | null;
	/** Locally-derived summary for the distilled message/thread. */
	summary: string | null;
	/** Exact distilled message evidence the classifier acted on. */
	evidence_message_id: string | null;
	evidence_message_at: number | null;
	/** When the annotation was created (classifier run time). */
	created_at: number;
	/** When the thread's latest message ARRIVED (epoch ms) — the real received
	 *  time; null if unknown. */
	received_at: number | null;
	/** Deep link to open the thread in its provider (routed to `account_email`). */
	open_url: string | null;
	/** Channel-execution actions the adapter declares (empty for channels with no
	 *  action adapter). Rendered generically — no channel-specific UI. */
	available_actions?: ChannelActionDescriptor[];
	/** Baseline and semantic ranks are both retained so ordering changes remain
	 *  inspectable. They are absent when Slice 1 has not evaluated this row. */
	baseline_rank?: number;
	learned_rank?: number;
	rank_delta?: number;
	learning_score?: number | null;
	/** Calibrated actionability is additive and never required to render or act
	 *  on a card. Shadow metadata is preview-only; enforced metadata reflects
	 *  active ordering, while fallback rows retain their Slice 1 placement. */
	actionability_probability?: number | null;
	actionability_explanation?: { code: string; label: string } | null;
	actionability_model_version?: string | null;
	actionability_snapshot_id?: string | null;
	semantic_feature_status?: 'succeeded' | 'missing' | 'invalid';
	actionability_score_status?: 'scored' | 'fallback' | 'disabled';
	actionability_mode?: 'disabled' | 'shadow' | 'enforced';
	actionability?: AttentionActionabilityCard;
	grouping?: AttentionGroupingMetadata;
	/** Client-copied page policy; item grouping alone never authorizes collapse. */
	grouping_page?: AttentionGroupingPage;
	decision_item?: AttentionDecisionItem;
	/** Server-authored contextual-bandit diagnostics. The browser displays this
	 * decision but never reorders or samples cards. */
	bandit_decision?: AttentionBanditDecision;
	/** Client-copied decision, visibility policy, and complete-universe health. */
	routing_page?: AttentionRoutingPage;
	/** Client-copied page policy state so shared row actions can label rank
	 *  evidence as active ordering versus an observe-only preview. */
	semantic_ranking_enabled?: boolean;
}

/** A channel-execution action the adapter declares for this follow-up (e.g.
 *  iMessage's "Reply"). Rendered generically — the UI is fully descriptor-driven
 *  and knows nothing channel-specific. */
export interface ChannelActionDescriptor {
	id: string;
	label: string;
	/** Clicking opens a compose modal to draft/edit the outgoing body first. */
	needs_compose: boolean;
	/** The send step is an explicit confirmation (guard destructive actions). */
	confirm: boolean;
	icon?: string;
}

export interface ChannelFollowUpPage {
	items: ChannelFollowUp[];
	/** Total Follow-ups across all pages (for counts + "show more"). */
	total: number;
	limit: number;
	cursor: string | null;
	next_cursor: string | null;
	has_more: boolean;
	ok: boolean;
	/** Full active-candidate health, independent from this page's visible rows. */
	health: ChannelFollowUpLearningHealth | null;
	/** Page-level actionability coverage. Null means the additive Slice 2
	 *  contract was absent or malformed, so the UI stays on Slice 1 behavior. */
	actionability: AttentionActionabilityPage | null;
	actionability_training: AttentionActionabilityTrainingStatus | null;
	grouping: AttentionGroupingPage | null;
	routing: AttentionRoutingPage | null;
	bandit: AttentionBanditHealth | null;
	/** Full active-universe server extraction/backfill diagnostics. */
	semantic_extraction: AttentionSemanticExtractionHealth | null;
	/** Strict all-or-nothing union. Null keeps both destination lanes on their
	 * legacy sources; individual canonical rows are never partially adopted. */
	canonical_attention_projection: CanonicalAttentionProjection | null;
	/** True only when the learned rank, rather than baseline rank, orders items. */
	semantic_ranking_enabled: boolean;
	latency?: {
		projection_ms: number | null;
		end_to_end_ms: number;
		budget_ms: number;
		within_budget: boolean;
	};
	error?: string;
}

export interface ChannelWritingPreference {
	id: string;
	provider: string;
	account_alias: string;
	scope_kind: 'sender' | 'domain';
	scope_value: string;
	statement: string;
	status: 'candidate' | 'promoted' | 'dismissed';
	evidence_count: number;
	updated_at: number;
}

/** One page of follow-ups awaiting the owner. Errors carry `ok=false`. */
export async function fetchChannelFollowUpsPage(
	limit = 50,
	cursor: string | null = null,
	options: {
		signal?: AbortSignal;
		/** Today already owns the complete union through the canonical
		 * projection store. Its card lookups opt out so pagination cannot
		 * rebuild that universe a second time. */
		includeCanonicalProjection?: boolean;
	} = {}
): Promise<ChannelFollowUpPage> {
	const requestStarted = typeof performance !== 'undefined' ? performance.now() : Date.now();
	const empty = (error: string): ChannelFollowUpPage => ({
		items: [],
		total: 0,
		limit,
		cursor,
		next_cursor: null,
		has_more: false,
		ok: false,
		health: null,
		actionability: null,
		actionability_training: null,
		grouping: null,
		routing: null,
		bandit: null,
		semantic_extraction: null,
		canonical_attention_projection: null,
		semantic_ranking_enabled: false,
		error
	});
	try {
		const params = new URLSearchParams({ limit: String(Math.max(1, Math.floor(limit))) });
		if (cursor) params.set('cursor', cursor);
		if (options.includeCanonicalProjection === false) params.set('include_projection', 'false');
		const res = await fetch(`/api/magician/v2/channel-assist/follow-ups?${params.toString()}`, {
			signal: options.signal
		});
		if (!res.ok) {
			const body = await res.json().catch(() => ({}));
			return empty(body?.error || `HTTP ${res.status}`);
		}
		const body = await res.json();
		const endToEndMs =
			(typeof performance !== 'undefined' ? performance.now() : Date.now()) - requestStarted;
		const health = parseChannelFollowUpLearningHealth(body?.health);
		const actionability = parseAttentionActionabilityPage(body);
		const actionabilityTraining = parseAttentionActionabilityTraining(body);
		const grouping = parseAttentionGroupingPage(body);
		const parsedRouting = parseAttentionRoutingPage(body);
		const routing =
			parsedRouting?.decision.surface === 'follow_up' ||
			parsedRouting?.decision.complete_cross_lane_universe
				? parsedRouting
				: null;
		const bandit = parseAttentionBanditHealth(body);
		const semanticExtraction = parseAttentionSemanticExtractionHealth(body);
		const canonicalAttentionProjection = canonicalAttentionProjectionFromResponse(body);
		const semanticRankingEnabled =
			body?.semantic_ranking_enabled === true || health?.semantic_ranking_enabled === true;
		const items = Array.isArray(body?.items)
			? (body.items as ChannelFollowUp[]).map((item) => {
					const itemActionability = parseAttentionActionabilityCard(item);
					const itemGrouping = parseAttentionGroupingMetadata(item.grouping);
					const candidateId =
						typeof item.candidate_id === 'string' && item.candidate_id.trim()
							? item.candidate_id.trim()
							: null;
					const hasRevision = Object.prototype.hasOwnProperty.call(item, 'source_revision');
					const itemDecision =
						routing && candidateId && hasRevision
							? parseAttentionDecisionItem(item.decision_item, {
									decision_id: routing.decision.decision_id,
									// The ledger keys decision items by the canonical
									// surface-qualified id while these rows carry the raw one.
									// Comparing them unnormalized rejects the item as belonging
									// to a different row, and the impression observer never arms.
									candidate_id: canonicalCandidateId('follow_up', candidateId),
									source_revision: item.source_revision ?? null,
									routing_mode: routing.decision.routing_mode,
									routing_snapshot_id: routing.decision.routing_snapshot_id
								})
							: null;
					const banditDecision = itemDecision
						? parseAttentionBanditDecision(item.bandit_decision, itemDecision)
						: null;
					return {
						...item,
						semantic_ranking_enabled: semanticRankingEnabled,
						...(itemActionability ? { actionability: itemActionability } : {}),
						...(itemGrouping ? { grouping: itemGrouping } : {}),
						...(grouping ? { grouping_page: grouping } : {}),
						...(itemDecision ? { decision_item: itemDecision } : {}),
						...(banditDecision ? { bandit_decision: banditDecision } : {}),
						...(routing ? { routing_page: routing } : {})
					};
			  })
			: [];
		const budgetMs =
			typeof body?.latency?.budget_ms === 'number' ? body.latency.budget_ms : 250;
		const withinBudget = body?.latency?.within_budget === true && endToEndMs <= budgetMs;
		if (!withinBudget && typeof console !== 'undefined') {
			console.warn(
				`[Magician] Today channel projection missed ${budgetMs}ms budget (${endToEndMs.toFixed(1)}ms end-to-end)`
			);
		}
		return {
			items,
			total: typeof body?.total === 'number' ? body.total : items.length,
			limit: typeof body?.limit === 'number' ? body.limit : limit,
			cursor: typeof body?.cursor === 'string' ? body.cursor : cursor,
			next_cursor: typeof body?.next_cursor === 'string' ? body.next_cursor : null,
			has_more: body?.has_more === true,
			ok: true,
			health,
			actionability,
			actionability_training: actionabilityTraining,
			grouping,
			routing,
			bandit,
			semantic_extraction: semanticExtraction,
			canonical_attention_projection: canonicalAttentionProjection,
			semantic_ranking_enabled: semanticRankingEnabled,
			latency: {
				projection_ms:
					typeof body?.latency?.projection_ms === 'number'
						? body.latency.projection_ms
						: null,
				end_to_end_ms: endToEndMs,
				budget_ms: budgetMs,
				within_budget: withinBudget
			}
		};
	} catch (e) {
		return empty(e instanceof Error ? e.message : String(e));
	}
}

export interface ChannelFollowUpGroupMembersResult {
	ok: boolean;
	items: ChannelFollowUp[];
	total: number;
	cluster: AttentionGroupingMetadata | null;
	error?: string;
}

/** Fetches the complete members of one learned group. The caller must compare
 * `total`, returned length, and representative `member_count` before claiming
 * that expansion is complete. */
export async function fetchChannelFollowUpGroupMembers(
	clusterId: string,
	semanticRankingEnabled = false,
	groupingPage: AttentionGroupingPage | null = null,
	routingPage: AttentionRoutingPage | null = null
): Promise<ChannelFollowUpGroupMembersResult> {
	try {
		const response = await fetch(
			`/api/magician/v2/channel-assist/follow-ups/groups/${encodeURIComponent(clusterId)}/members`
		);
		const body: unknown = await response.json().catch(() => null);
		const record =
			body !== null && typeof body === 'object' && !Array.isArray(body)
				? (body as Record<string, unknown>)
				: null;
		if (!response.ok || !record) {
			return {
				ok: false,
				items: [],
				total: 0,
				cluster: null,
				error:
					typeof record?.error === 'string' ? record.error : `HTTP ${response.status}`
			};
		}
		const items = Array.isArray(record.items)
			? (record.items as ChannelFollowUp[]).map((item) => {
					const itemActionability = parseAttentionActionabilityCard(item);
					const itemGrouping = parseAttentionGroupingMetadata(item.grouping);
					const candidateId =
						typeof item.candidate_id === 'string' && item.candidate_id.trim()
							? item.candidate_id.trim()
							: null;
					const hasRevision = Object.prototype.hasOwnProperty.call(item, 'source_revision');
					const itemDecision =
						routingPage && candidateId && hasRevision
							? parseAttentionDecisionItem(item.decision_item, {
									decision_id: routingPage.decision.decision_id,
									// The ledger keys decision items by the canonical
									// surface-qualified id while these rows carry the raw one.
									// Comparing them unnormalized rejects the item as belonging
									// to a different row, and the impression observer never arms.
									candidate_id: canonicalCandidateId('follow_up', candidateId),
									source_revision: item.source_revision ?? null,
									routing_mode: routingPage.decision.routing_mode,
									routing_snapshot_id: routingPage.decision.routing_snapshot_id
								})
							: null;
					const banditDecision = itemDecision
						? parseAttentionBanditDecision(item.bandit_decision, itemDecision)
						: null;
					return {
						...item,
						semantic_ranking_enabled: semanticRankingEnabled,
						...(itemActionability ? { actionability: itemActionability } : {}),
						...(itemGrouping ? { grouping: itemGrouping } : {}),
						...(groupingPage ? { grouping_page: groupingPage } : {}),
						...(itemDecision ? { decision_item: itemDecision } : {}),
						...(banditDecision ? { bandit_decision: banditDecision } : {}),
						...(routingPage ? { routing_page: routingPage } : {})
					};
			  })
			: [];
		const total =
			typeof record.total === 'number' &&
			Number.isSafeInteger(record.total) &&
			record.total >= 0
				? record.total
				: items.length;
		const clusterRecord =
			record.cluster !== null &&
			typeof record.cluster === 'object' &&
			!Array.isArray(record.cluster)
				? (record.cluster as Record<string, unknown>)
				: null;
		return {
			ok: true,
			items,
			total,
			cluster: parseAttentionGroupingMetadata(clusterRecord?.grouping ?? clusterRecord)
		};
	} catch (error) {
		return {
			ok: false,
			items: [],
			total: 0,
			cluster: null,
			error: error instanceof Error ? error.message : String(error)
		};
	}
}

export type ChannelFollowUpActionResult =
	| { ok: true; taskId?: string; feedbackReceipt: AttentionFeedbackReceipt | null }
	| { ok: false; error: string };

async function postAction(
	annotationId: string,
	action: string,
	payload?: Record<string, unknown>,
	attribution?: AttentionFeedbackAttribution | null
): Promise<ChannelFollowUpActionResult> {
	try {
		const canonicalCandidateId = `follow_up:${annotationId}`;
		const exactAttribution =
			attribution &&
			(attribution.candidate_id === annotationId ||
				attribution.candidate_id === canonicalCandidateId)
				? attribution
				: null;
		const requestBody = {
			...(payload ?? {}),
			...(exactAttribution ? { attribution: exactAttribution } : {})
		};
		const hasRequestBody = Object.keys(requestBody).length > 0;
		const res = await fetch(
			`/api/magician/v2/channel-assist/annotations/${encodeURIComponent(annotationId)}/${action}`,
			{
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: hasRequestBody ? JSON.stringify(requestBody) : undefined
			}
		);
		const body = await res.json().catch(() => ({}));
		if (!res.ok) return { ok: false, error: body?.error || `HTTP ${res.status}` };
		const feedbackReceipt = parseAttentionFeedbackReceipt(body?.feedback_receipt);
		attentionRankRecomputeStore.track(feedbackReceipt, {
			raw_candidate_id: annotationId,
			...(exactAttribution && Object.prototype.hasOwnProperty.call(exactAttribution, 'source_revision')
				? { source_revision: exactAttribution.source_revision }
				: {}),
			attribution: exactAttribution
		});
		return {
			ok: true,
			taskId: typeof body?.task_id === 'string' ? body.task_id : undefined,
			feedbackReceipt
		};
	} catch (e) {
		return { ok: false, error: e instanceof Error ? e.message : String(e) };
	}
}

/** Draft the body for a channel-execution action (e.g. iMessage "Reply"). The
 *  server returns a `compose_id` + generated `text`; passing an optional `hint`
 *  re-drafts. Generic across every adapter-declared action. */
export async function composeChannelAction(
	annotationId: string,
	actionId: string,
	hint?: string
): Promise<{ ok: boolean; composeId?: string; text?: string; error?: string }> {
	try {
		const res = await fetch(
			`/api/magician/v2/channel-assist/annotations/${encodeURIComponent(annotationId)}/action/${encodeURIComponent(actionId)}/compose`,
			{
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: hint && hint.trim() ? JSON.stringify({ hint: hint.trim() }) : undefined
			}
		);
		const body = await res.json().catch(() => ({}));
		if (!res.ok) return { ok: false, error: body?.error || `HTTP ${res.status}` };
		return {
			ok: true,
			composeId: typeof body?.compose_id === 'string' ? body.compose_id : undefined,
			text: typeof body?.text === 'string' ? body.text : undefined
		};
	} catch (error) {
		return { ok: false, error: error instanceof Error ? error.message : String(error) };
	}
}

/** Commit a channel-execution action. `body` (the user's edited draft) wins if
 *  provided; otherwise the server sends the composed `compose_id` text. Generic
 *  across every adapter-declared action. */
export async function commitChannelAction(
	annotationId: string,
	actionId: string,
	payload: { compose_id?: string; body?: string },
	attribution?: AttentionFeedbackAttribution | null
): Promise<ChannelFollowUpActionResult> {
	try {
		const canonicalCandidateId = `follow_up:${annotationId}`;
		const exactAttribution =
			attribution &&
			(attribution.candidate_id === annotationId ||
				attribution.candidate_id === canonicalCandidateId)
				? attribution
				: null;
		const eventId =
			typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
				? crypto.randomUUID()
				: `channel-action-${Date.now()}-${Math.random().toString(36).slice(2)}`;
		const res = await fetch(
			`/api/magician/v2/channel-assist/annotations/${encodeURIComponent(annotationId)}/action/${encodeURIComponent(actionId)}/commit`,
			{
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({
					...(payload ?? {}),
					event_id: eventId,
					...(exactAttribution ? { attribution: exactAttribution } : {})
				})
			}
		);
		const body = await res.json().catch(() => ({}));
		if (!res.ok) return { ok: false, error: body?.error || `HTTP ${res.status}` };
		const feedbackReceipt = parseAttentionFeedbackReceipt(body?.feedback_receipt);
		attentionRankRecomputeStore.track(feedbackReceipt, {
			raw_candidate_id: annotationId,
			...(exactAttribution && Object.prototype.hasOwnProperty.call(exactAttribution, 'source_revision')
				? { source_revision: exactAttribution.source_revision }
				: {}),
			attribution: exactAttribution
		});
		return { ok: true, feedbackReceipt };
	} catch (error) {
		return { ok: false, error: error instanceof Error ? error.message : String(error) };
	}
}

export interface ChannelEvidenceMessage {
	message_id: string | null;
	body: string | null;
	summary: string | null;
	subject: string | null;
	received_at: number | null;
}

/** The message a card's summary/classification was actually derived from — the
 *  newest DISTILLED message (not the max-date one), with its stored summary so
 *  the input (body) ↔ output (summary) is unambiguous. */
export interface ChannelMessageView {
	/** The actual message body, fetched live server-side (never stored). */
	body: string | null;
	/** The stored summary this message was distilled into — what the classifier
	 *  acted on. */
	summary: string | null;
	subject: string | null;
	/** True if a NEWER message arrived after the one we summarized. */
	has_newer: boolean;
	/** All messages that fed a coalesced distillation; single-message rows carry one. */
	evidence_messages: ChannelEvidenceMessage[];
}

/** Fetch the summarized message on demand. Returns null on error. */
export async function fetchChannelMessage(
	annotationId: string
): Promise<ChannelMessageView | null> {
	try {
		const res = await fetch(
			`/api/magician/v2/channel-assist/annotations/${encodeURIComponent(annotationId)}/message`
		);
		if (!res.ok) return null;
		const b = await res.json();
		return {
			body: typeof b?.body === 'string' ? b.body : null,
			summary: typeof b?.summary === 'string' ? b.summary : null,
			subject: typeof b?.subject === 'string' ? b.subject : null,
			has_newer: b?.has_newer === true,
			evidence_messages: Array.isArray(b?.evidence_messages)
				? b.evidence_messages.map((m: Record<string, unknown>) => ({
						message_id: typeof m?.message_id === 'string' ? m.message_id : null,
						body: typeof m?.body === 'string' ? m.body : null,
						summary: typeof m?.summary === 'string' ? m.summary : null,
						subject: typeof m?.subject === 'string' ? m.subject : null,
						received_at: typeof m?.received_at === 'number' ? m.received_at : null
				  }))
				: []
		};
	} catch {
		return null;
	}
}

/** "Do it" — creates a follow-up task linked to the thread. Returns its id.
 *  (Positive response WITH immediate action.) Optional `hint` = the owner's
 *  instruction for the agent. */
export const approveChannelFollowUp = (
	id: string,
	hint?: string,
	attribution?: AttentionFeedbackAttribution | null
) => postAction(id, 'approve', hint && hint.trim() ? { hint: hint.trim() } : undefined, attribution);
/** Dismiss with an optional reason (spam / already_handled / …) — feeds the
 *  negative feedback signal. */
export const dismissChannelFollowUpWithReason = (
	id: string,
	reason?: ChannelFollowUpDismissReason,
	attribution?: AttentionFeedbackAttribution | null
) =>
	postAction(id, 'dismiss', reason ? { reason } : undefined, attribution);
/** "Useful" — POSITIVE response with no immediate action (annotation →
 *  acknowledged, logs a positive `helpful` learning signal, no task). */
export const usefulChannelFollowUp = (
	id: string,
	attribution?: AttentionFeedbackAttribution | null
) => postAction(id, 'useful', undefined, attribution);
/** "Acknowledge" — NEUTRAL "seen, no action needed" (annotation → acknowledged,
 *  no task and NO learning signal — the mirror of Worth-a-look's Acknowledge). */
export const acknowledgeChannelFollowUp = (
	id: string,
	attribution?: AttentionFeedbackAttribution | null
) => postAction(id, 'acknowledge', undefined, attribution);
/** Snooze — drops the card without deleting (annotation → classified). */
export const snoozeChannelFollowUp = (
	id: string,
	attribution?: AttentionFeedbackAttribution | null
) => postAction(id, 'snooze', undefined, attribution);
/** Re-open a stale draft recommendation after reviewing the newer message. */
export const reviewStaleChannelFollowUp = (id: string) => postAction(id, 'review');

export async function fetchChannelWritingPreferences(
	annotationId: string
): Promise<ChannelWritingPreference[]> {
	try {
		const res = await fetch(
			`/api/magician/v2/channel-assist/annotations/${encodeURIComponent(annotationId)}/writing-preferences`
		);
		if (!res.ok) return [];
		const body = await res.json();
		return Array.isArray(body?.items) ? (body.items as ChannelWritingPreference[]) : [];
	} catch {
		return [];
	}
}

export async function learnChannelWritingPreference(
	annotationId: string,
	scope: 'sender' | 'domain',
	statement: string,
	promote = false
): Promise<{ ok: boolean; items?: ChannelWritingPreference[]; error?: string }> {
	try {
		const res = await fetch(
			`/api/magician/v2/channel-assist/annotations/${encodeURIComponent(annotationId)}/writing-preferences`,
			{
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ scope, statement, promote })
			}
		);
		const body = await res.json().catch(() => ({}));
		if (!res.ok) return { ok: false, error: body?.error || `HTTP ${res.status}` };
		return { ok: true, items: Array.isArray(body?.items) ? body.items : [] };
	} catch (error) {
		return { ok: false, error: error instanceof Error ? error.message : String(error) };
	}
}

export async function updateChannelWritingPreference(
	id: string,
	action: 'promote' | 'dismiss'
): Promise<{ ok: boolean; item?: ChannelWritingPreference; error?: string }> {
	try {
		const res = await fetch(
			`/api/magician/v2/channel-assist/writing-preferences/${encodeURIComponent(id)}/${action}`,
			{ method: 'POST' }
		);
		const body = await res.json().catch(() => ({}));
		if (!res.ok) return { ok: false, error: body?.error || `HTTP ${res.status}` };
		return { ok: true, item: body as ChannelWritingPreference };
	} catch (error) {
		return { ok: false, error: error instanceof Error ? error.message : String(error) };
	}
}
/** Dismiss — negative response; the thread won't resurface (annotation →
 *  dismissed). */
export const dismissChannelFollowUp = (
	id: string,
	attribution?: AttentionFeedbackAttribution | null
) => postAction(id, 'dismiss', undefined, attribution);

/** Local date + time an item arrived, e.g. "Jul 6, 2026, 2:14 PM". Empty for null. */
export function channelReceivedLabel(ms: number | null): string {
	if (!ms) return '';
	try {
		return new Date(ms).toLocaleString(undefined, {
			month: 'short',
			day: 'numeric',
			year: 'numeric',
			hour: 'numeric',
			minute: '2-digit'
		});
	} catch {
		return '';
	}
}

/** Human label for the classifier label. */
export function channelLabelText(label: string | null): string {
	switch (label) {
		case 'needs_reply':
			return 'Needs reply';
		case 'follow_up':
			return 'Follow up';
		default:
			return label ?? 'Follow up';
	}
}

function actionString(action: unknown, key: string): string | null {
	if (!action || typeof action !== 'object' || Array.isArray(action)) return null;
	const value = (action as Record<string, unknown>)[key];
	return typeof value === 'string' && value.trim() ? value.trim() : null;
}

function actionStringArray(action: unknown, key: string): string[] {
	if (!action || typeof action !== 'object' || Array.isArray(action)) return [];
	const value = (action as Record<string, unknown>)[key];
	if (Array.isArray(value)) {
		return value
			.filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
			.map((item) => item.trim())
			.slice(0, 6);
	}
	if (typeof value === 'string' && value.trim()) return [value.trim()];
	return [];
}

function actionKindLabel(value: string): string {
	return value.replace(/_/g, ' ').replace(/\b\w/g, (match) => match.toUpperCase());
}

export function channelActionDetails(followUp: ChannelFollowUp): string[] {
	return actionStringArray(followUp.proposed_action, 'key_details');
}

/** Explicit due/expected-follow-up text from the classifier, if any. */
export function channelDueLabel(followUp: ChannelFollowUp): string | null {
	const due = actionString(followUp.proposed_action, 'due_text');
	return due && due.trim() ? due.trim() : null;
}

/** Compact human summary of the classifier's structured follow-up signal. */
export function channelActionSummary(followUp: ChannelFollowUp): string | null {
	const kind = actionString(followUp.proposed_action, 'follow_up_kind');
	const owner = actionString(followUp.proposed_action, 'action_owner');
	const due = actionString(followUp.proposed_action, 'due_text');
	const urgency = actionString(followUp.proposed_action, 'urgency');
	const details = channelActionDetails(followUp).slice(0, 3);
	const parts = [
		kind ? actionKindLabel(kind) : null,
		owner && owner !== 'unknown' ? `Owner: ${actionKindLabel(owner)}` : null,
		due ? `Due: ${due}` : null,
		urgency && urgency !== 'normal' ? actionKindLabel(urgency) : null,
		details.length > 0 ? `Details: ${details.join(' · ')}` : null
	].filter((part): part is string => Boolean(part));
	return parts.length > 0 ? parts.join(' · ') : null;
}

/** Short provider label for the row. */
export function channelProviderText(provider: string): string {
	switch (provider) {
		case 'gmail':
			return 'Gmail';
		case 'agentmail':
			return 'AgentMail';
		case 'whatsapp':
			return 'WhatsApp';
		case 'whatsapp_kapso':
			return 'WhatsApp (Presto)';
		default:
			return provider;
	}
}

/** Lane badge label. */
export function channelLaneText(lane: ChannelFollowUpLane): string {
	return lane === 'envoy' ? 'Presto' : 'You';
}
