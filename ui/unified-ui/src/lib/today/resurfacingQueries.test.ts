import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	fetchResurfacingDetail,
	fetchResurfacingGroupMembers,
	fetchResurfacingOriginal,
	fetchResurfacingTodayPage,
	mapResurfacingDetailPayload,
	mapResurfacingPagePayload,
	mapResurfacingPayload,
	parseResurfacingCrossLaneReconciliation,
	postResurfacingAction,
	postResurfacingContextualAction,
	postResurfacingRecommendationEvent,
	ResurfacingApiError,
	type ResurfacingCard
} from './resurfacingQueries';

afterEach(() => {
	vi.unstubAllGlobals();
});

function crossLaneReconciliation(
	visibleSourceTotal: number,
	visiblePageTotal: number,
	duplicateHiddenTotal = 0,
	duplicateHiddenPageTotal = duplicateHiddenTotal
) {
	const rawSourceTotal = visibleSourceTotal + duplicateHiddenTotal;
	return {
		schema_version: 1,
		status: 'succeeded',
		reason: null,
		authoritative_lane: 'follow_up',
		principal: 'anonymous',
		workspace: 'default',
		follow_up_source_total: 4,
		worth_a_look_source_total: rawSourceTotal,
		raw_source_total: rawSourceTotal,
		visible_source_total: visibleSourceTotal,
		duplicate_hidden_total: duplicateHiddenTotal,
		raw_scanned_total: visiblePageTotal + duplicateHiddenPageTotal,
		visible_page_total: visiblePageTotal,
		duplicate_hidden_page_total: duplicateHiddenPageTotal,
		reconciliation_digest: 'reconciliation-1'
	};
}

function semanticExtractionHealth() {
	return {
		schema_version: 1,
		enabled: true,
		paused: false,
		pause_reason: null,
		degradation_reason: null,
		contract: {
			semantic_schema_version: 2,
			extractor_contract: 'attention-semantic-v2',
			prompt_version: 'attention-extract-v4',
			model: 'gpt-5-mini',
			profile: 'foreground-safe'
		},
		checkpoint: {
			cursor: 'candidate-10',
			updated_at: 1_725_000_000_000,
			lease_owner: null,
			lease_expires_at: null
		},
		totals: {
			active_total: 10,
			compatible_revision: 7,
			succeeded: 7,
			missing: 1,
			invalid: 1,
			pending: 0,
			in_flight: 0,
			retry: 1,
			dead: 0,
			coverage: 0.7
		},
		surfaces: {
		follow_up: {
			active_total: 6,
			compatible_revision: 4,
			succeeded: 4,
			missing: 1,
			invalid: 0,
			pending: 0,
			in_flight: 0,
				retry: 1,
				dead: 0,
				coverage: 4 / 6
			},
		worth_a_look: {
			active_total: 4,
			compatible_revision: 3,
			succeeded: 3,
			missing: 0,
			invalid: 1,
			pending: 0,
			in_flight: 0,
				retry: 0,
				dead: 0,
				coverage: 0.75
			}
		},
		queue: { pending: 2, in_flight: 1, retry: 1, dead: 0, next_retry_at: null }
	};
}

describe('mapResurfacingPayload', () => {
	it('maps a { cards: [...] } payload to typed cards', () => {
		const cards = mapResurfacingPayload({
			cards: [
				{
					candidate_id: 'cand-1',
					line: 'Circle back with Dana about the venue',
					why_now: 'You said "next week" eight days ago',
					source_title: 'Venue thread',
					summary: 'Dana shared venue availability and asked for confirmation.',
					source_kind: 'memory',
					source_ref: 'user.knowledge:42',
					detail_label: 'Memory summary',
					temporal_anchor_at: 1_720_000_000_000
				},
				{
					candidate_id: 'cand-2',
					line: 'Renew the domain before it lapses',
					why_now: 'Expires in 3 days',
					source_title: 'Domain renewal',
					summary: 'The domain renewal task is nearing its due date.',
					source_kind: 'reminder',
					source_ref: 'user.tasks:7',
					detail_label: 'Task summary',
					temporal_anchor_at: null
				}
			]
		});
		expect(cards).toHaveLength(2);
			expect(cards[0]).toEqual({
			candidate_id: 'cand-1',
			line: 'Circle back with Dana about the venue',
			why_now: 'You said "next week" eight days ago',
			source_title: 'Venue thread',
			summary: 'Dana shared venue availability and asked for confirmation.',
			source_kind: 'memory',
			source_ref: 'user.knowledge:42',
			detail_label: 'Memory summary',
			temporal_anchor_at: 1_720_000_000_000,
			brief: null,
			brief_status: 'legacy',
			content_revision: null,
			source_updated: false,
			recommended_action: null,
			actions: []
		} satisfies ResurfacingCard);
		expect(cards[1].temporal_anchor_at).toBeNull();
	});

	it('returns [] for empty, missing, or non-array cards (no throw)', () => {
		expect(mapResurfacingPayload({ cards: [] })).toEqual([]);
		expect(mapResurfacingPayload({})).toEqual([]);
		expect(mapResurfacingPayload({ cards: null })).toEqual([]);
		expect(mapResurfacingPayload({ cards: 'nope' })).toEqual([]);
		expect(mapResurfacingPayload(null)).toEqual([]);
		expect(mapResurfacingPayload(undefined)).toEqual([]);
		expect(mapResurfacingPayload('not-an-object')).toEqual([]);
	});

	it('drops malformed rows and coerces missing/invalid fields defensively', () => {
		const cards = mapResurfacingPayload({
			cards: [
				null,
				'not-an-object',
				{ line: 'no candidate id — dropped' },
				{ candidate_id: '   ' }, // blank id — dropped
				{ candidate_id: 'ok', temporal_anchor_at: 'not-a-number' }
			]
		});
		expect(cards).toHaveLength(1);
		expect(cards[0]).toEqual({
			candidate_id: 'ok',
			line: '',
			why_now: '',
			source_title: '',
			summary: '',
			source_kind: '',
			source_ref: '',
			detail_label: '',
			temporal_anchor_at: null,
			brief: null,
			brief_status: 'legacy',
			content_revision: null,
			source_updated: false,
			recommended_action: null,
			actions: []
		} satisfies ResurfacingCard);
	});

	it('maps a rich brief, recommendation, and only valid capabilities', () => {
		const [card] = mapResurfacingPayload({
			cards: [
				{
					candidate_id: 'lpg-change',
					line: 'LPG booking rules changed',
					summary: 'Bookings now require OTP confirmation from July 15.',
					content_revision: 'rev-2',
					source_updated: true,
					brief: {
						schema_version: 2,
						key_facts: ['OTP confirmation is required', '', 42],
						changes: [
							{
								aspect: 'Booking confirmation',
								before: 'No OTP',
								after: 'OTP required',
								effective_text: 'July 15'
							},
							{ aspect: '', before: null, after: null }
						],
						temporal_facts: [
							{ kind: 'effective', text: 'July 15', at_ms: 1_720, timezone: 'Asia/Kolkata' },
							{ kind: 'deadline', text: '' }
						],
						detail_status: 'source_omits_details',
						missing_details: ['Whether existing bookings are affected']
					},
					recommended_action: {
						kind: 'create_reminder',
						label: 'Remind me before July 15',
						rationale: 'The rule has an effective date.',
						confidence: 1.4,
						content_revision: 'rev-2',
						source: 'curator'
					},
					actions: [
						{
							kind: 'create_reminder',
							label: 'Create reminder',
							requires_input: true,
							side_effect: 'creates_reminder'
						},
						{ kind: 'invented_action', label: 'Nope' }
					]
				}
			]
		});

		expect(card.brief).toEqual({
			schema_version: 2,
			key_facts: ['OTP confirmation is required'],
			changes: [
				{
					aspect: 'Booking confirmation',
					before: 'No OTP',
					after: 'OTP required',
					effective_text: 'July 15'
				}
			],
			temporal_facts: [
				{ kind: 'effective', text: 'July 15', at_ms: 1_720, timezone: 'Asia/Kolkata' }
			],
			detail_status: 'source_omits_details',
			missing_details: ['Whether existing bookings are affected']
		});
		expect(card.recommended_action).toMatchObject({
			kind: 'create_reminder',
			confidence: 1,
			content_revision: 'rev-2'
		});
		expect(card.actions.map((action) => action.kind)).toEqual(['create_reminder']);
	});
});

describe('mapResurfacingPagePayload', () => {
	it('maps server-side pagination metadata and next cursor', () => {
		const page = mapResurfacingPagePayload({
			cards: [
				{
					candidate_id: 'cand-1',
					line: 'A',
					actionability_probability: 0.84,
					actionability_explanation: {
						code: 'direct_owner_request',
						label: 'Direct request to you'
					},
					actionability_model_version: 'actionability-gbt-v1',
					actionability_snapshot_id: 'snapshot-42',
					semantic_feature_status: 'succeeded',
					actionability_score_status: 'scored',
					actionability_mode: 'shadow'
				}
			],
			total: 3,
			limit: 1,
			offset: 0,
			has_more: true,
			semantic_ranking_enabled: true,
			semantic_extraction_health: semanticExtractionHealth(),
			actionability_mode: 'shadow',
			actionability_snapshot_id: 'snapshot-42',
			semantic_extraction_coverage: 0.82,
			actionability_scored_count: 82,
			actionability_fallback_count: 18,
			grouping_mode: 'shadow',
			grouping_snapshot_id: 'group-snapshot-1',
			grouping_generation: 7,
			grouping_scope: 'eligible_universe',
			grouping_health: {
				candidate_total: 3,
				member_total: 3,
				cluster_total: 2,
				representative_total: 2,
				collapsed_member_total: 1,
				scored_pair_total: 1,
				cannot_link_total: 0,
				fallback_ungrouped_total: 1,
				totals_reconcile: true
			},
			health: {
				total_active: 2014,
				source_family_counts: { comm: 1691, web: 323 },
				embedded_candidates: 1593,
				embedding_coverage: 0.7909,
				learned_rank_changes: 110
			},
			next_cursor: {
				surfaced_at: 1_720_000,
				score: 0.75,
				candidate_id: 'cand-1'
			},
			cross_lane_reconciliation: crossLaneReconciliation(3, 1)
		});
		expect(page.cards).toHaveLength(1);
		expect(page.total).toBe(3);
		expect(page.limit).toBe(1);
		expect(page.offset).toBe(0);
		expect(page.has_more).toBe(true);
		expect(page.semantic_ranking_enabled).toBe(true);
		expect(page.health?.total_active).toBe(2014);
		expect(page.semantic_extraction).toMatchObject({
			totals: { active_total: 10, compatible_revision: 7, coverage: 0.7 },
			surfaces: { worth_a_look: { active_total: 4, compatible_revision: 3 } }
		});
		expect(page.actionability).toMatchObject({
			mode: 'shadow',
			semantic_extraction_coverage: 0.82,
			fallback_count: 18
		});
		expect(page.grouping).toMatchObject({ mode: 'shadow', generation: 7 });
		expect(page.cards[0].actionability).toMatchObject({
			probability: 0.84,
			mode: 'shadow',
			explanation: { code: 'direct_owner_request', label: 'Direct request to you' }
		});
		expect(page.next_cursor).toEqual({
			surfaced_at: 1_720_000,
			score: 0.75,
			candidate_id: 'cand-1'
		});
	});

	it('returns an empty page for malformed payloads and drops malformed cursors', () => {
			expect(mapResurfacingPagePayload(null)).toEqual({
			cards: [],
			total: 0,
			limit: 0,
			offset: 0,
			has_more: false,
			next_cursor: null,
			health: null,
			actionability: null,
			actionability_training: null,
			grouping: null,
			routing: null,
			bandit: null,
			semantic_extraction: null,
			canonical_attention_projection: null,
			cross_lane_reconciliation: null,
			cross_lane_reconciliation_error: 'cross_lane_reconciliation_missing',
			semantic_ranking_enabled: false
		});

		const page = mapResurfacingPagePayload({
			cards: [{ candidate_id: 'cand-1' }],
			total: 'bad',
			has_more: 'yes',
			next_cursor: { candidate_id: '', surfaced_at: 'bad', score: null }
		});
		expect(page.cards).toHaveLength(0);
		expect(page.cross_lane_reconciliation_error).toBe('cross_lane_reconciliation_malformed');
		expect(page.total).toBe(0);
		expect(page.has_more).toBe(false);
		expect(page.next_cursor).toBeNull();
		expect(page.actionability).toBeNull();
		expect(page.semantic_extraction).toBeNull();
	});

	it('keeps invalid and missing Slice 2 Worth cards in Slice 1 fallback', () => {
		const malformedHealth = semanticExtractionHealth();
		malformedHealth.totals.coverage = 0.9;
		const page = mapResurfacingPagePayload({
			cards: [
				{
					candidate_id: 'cand-invalid',
					line: 'Still visible with invalid semantic features',
					actionability_probability: null,
					actionability_explanation: null,
					actionability_model_version: null,
					actionability_snapshot_id: 'snapshot-42',
					semantic_feature_status: 'invalid',
					actionability_score_status: 'fallback',
					actionability_mode: 'shadow'
				},
				{
					candidate_id: 'cand-missing',
					line: 'Still visible with no Slice 2 metadata'
				}
			],
			total: 2,
			limit: 2,
			has_more: false,
			semantic_extraction_health: malformedHealth,
			cross_lane_reconciliation: crossLaneReconciliation(2, 2)
		});

		expect(page.cards.map((card) => card.candidate_id)).toEqual([
			'cand-invalid',
			'cand-missing'
		]);
		expect(page.cards[0].actionability).toMatchObject({
			probability: null,
			semantic_feature_status: 'invalid',
			score_status: 'fallback',
			mode: 'shadow'
		});
		expect(page.cards[1].actionability).toBeUndefined();
		expect(page.semantic_extraction).toBeNull();
	});

	it('maps exact canary routing identity without changing the returned card set', () => {
		const page = mapResurfacingPagePayload({
			cards: [
				{
					candidate_id: 'cand-1',
					source_revision: 'revision-1',
					line: 'Safe curator line',
					decision_item: {
						decision_id: 'decision-worth-1',
						candidate_id: 'cand-1',
						source_revision: 'revision-1',
						baseline_route: 'follow_up',
						learned_route: 'worth_a_look',
						served_route: 'worth_a_look',
						routing_mode: 'canary',
						routing_snapshot_id: 'routing-snapshot-1',
						routing_model_version: 'lane-router-v1',
						learned_route_confidence: 0.97,
						utility_margin: 0.42,
						route_reason: 'learned_route_applied',
						served_rank: 1,
						selected: true,
						route_applied: true,
						canary_assigned: true
					},
					bandit_decision: {
						schema_version: 1,
						mode: 'canary',
						policy_snapshot_id: 'policy-1',
						policy_model_version: 'contextual-ts-v1',
						posterior_version: 12,
						posterior_uncertainty: 0.31,
						proposed_position: 1,
						served_position: 1,
						served_propensity: 0.65,
						posterior_draw_count: 16,
						seed_identity: 'seed-1',
						support: true,
						exploration: true,
						applied: true,
						degradation_reason: null
					}
				},
				{ candidate_id: 'legacy-card', line: 'Legacy card' }
			],
			total: 2,
			limit: 5,
			offset: 0,
			has_more: false,
			decision: {
				decision_id: 'decision-worth-1',
				decided_at: 1_725_000_000_000,
				surface: 'worth_a_look',
				routing_mode: 'canary',
				routing_snapshot_id: 'routing-snapshot-1',
				eligible_item_count: 2,
				selected_item_count: 2,
				returned_item_count: 2,
				complete_universe_recorded: true,
				degradation_reason: null
			},
			impression_policy: {
				min_visible_ms: 1_000,
				visibility_rule_version: 'visible-50-v1'
			},
			routing_health: {
				routing_snapshot_valid: true,
				evaluated_count: 2,
				learned_route_count: 1,
				applied_route_count: 1,
				baseline_retained_count: 1,
				impression_eligible_count: 1,
				decision_item_coverage: 0.5,
				all_candidates_path: '/all-candidates/decision-worth-1'
			},
			bandit_health: {
				mode: 'canary',
				policy_snapshot_id: 'policy-1',
				posterior_version: 12,
				posterior_update_count: 7,
				propensity_coverage: 1,
				exploration_rate: 0.1,
				support_ok: true,
				first_page_bounded: true,
				degradation_reason: null
			},
			cross_lane_reconciliation: crossLaneReconciliation(2, 2)
		});

		expect(page.cards.map((card) => card.candidate_id)).toEqual(['cand-1', 'legacy-card']);
		expect(page.routing?.health.applied_route_count).toBe(1);
		expect(page.cards[0].decision_item).toMatchObject({
			baseline_route: 'follow_up',
			learned_route: 'worth_a_look',
			served_route: 'worth_a_look',
			route_applied: true
		});
		expect(page.bandit).toMatchObject({ mode: 'canary', posterior_update_count: 7 });
		expect(page.cards[0].bandit_decision).toMatchObject({
			mode: 'canary',
			served_propensity: 0.65,
			exploration: true,
			applied: true
		});
		expect(page.cards[1].decision_item).toBeUndefined();
	});

	it('strictly parses succeeded reconciliation and rejects scope/count/status drift', () => {
		const succeeded = crossLaneReconciliation(3, 2, 1);
		expect(parseResurfacingCrossLaneReconciliation(succeeded, {
			principal: 'anonymous',
			workspace: 'default'
		})?.status).toBe('succeeded');
		expect(parseResurfacingCrossLaneReconciliation({
			...succeeded,
			visible_page_total: 3
		})).toBeNull();
		expect(parseResurfacingCrossLaneReconciliation(succeeded, {
			principal: 'another',
			workspace: 'default'
		})).toBeNull();
		expect(parseResurfacingCrossLaneReconciliation({ ...succeeded, status: 'disabled' })).toBeNull();
	});

	it('maps complete group members with explicit source route and revision metadata', async () => {
		const grouping = {
			cluster_id: 'cluster-1',
			representative_id: 'cand-1',
			is_representative: true,
			member_count: 2,
			related_count: 1,
			model_version: 'pair-gbt-v1',
			snapshot_id: 'group-snapshot-1',
			merge_probability: 0.98
		};
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				new Response(
					JSON.stringify({
						cluster: grouping,
						total: 2,
						cross_lane_reconciliation: crossLaneReconciliation(2, 2),
						items: [
							{
								candidate_id: 'cand-1',
								line: 'Representative',
								source_kind: 'comm',
								source_ref: 'comm:1',
								source_revision: 'rev-1',
								source_route: 'comm:1',
								open_url: 'https://mail.example/1',
								grouping
							},
							{
								candidate_id: 'cand-2',
								line: 'Related update',
								source_kind: 'web',
								source_ref: 'https://example.test/2',
								source_revision: 'rev-2',
								source_route: 'https://example.test/2',
								open_url: 'https://example.test/2',
								grouping: { ...grouping, is_representative: false }
							}
						]
					}),
					{ status: 200, headers: { 'Content-Type': 'application/json' } }
				)
			)
		);
		const result = await fetchResurfacingGroupMembers('cluster-1');
		expect(result.total).toBe(2);
		expect(result.items[1]).toMatchObject({
			source_revision: 'rev-2',
			source_route: 'https://example.test/2',
			open_url: 'https://example.test/2'
		});
	});

	it('fails closed when group expansion has no scoped reconciliation proof', async () => {
		vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
			cluster: {},
			total: 0,
			items: []
		}), { status: 200 })));
		await expect(fetchResurfacingGroupMembers('cluster-1')).rejects.toMatchObject({
			code: 'cross_lane_reconciliation_unavailable'
		});
	});
});

describe('mapResurfacingDetailPayload', () => {
	it('maps current source metadata and bounded original evidence defensively', () => {
		const detail = mapResurfacingDetailPayload({
			candidate_id: 'cand-1',
			source_kind: 'comm',
			status: 'newer_available',
			title: 'Card policy update',
			summary: 'The reward cap drops to 5,000 points on August 1.',
			has_newer: true,
			source_updated: true,
			source: {
				kind: 'comm',
				provider: 'gmail',
				account_alias: 'personal',
				account_email: 'owner@example.com',
				thread_id: 'thread-1',
				message_id: 'message-1',
				received_at: 123,
				evidence_message_ids: ['message-1']
			},
			original: {
				kind: 'comm',
				message_id: 'message-1',
				subject: 'Policy update',
				summary: 'Summary',
				received_at: 123,
				body: 'Bounded body',
				evidence_messages: [
					{
						message_id: 'message-1',
						body: 'Bounded body',
						truncated: true,
						attachment_count: 2
					},
					{ body: 'missing id' }
				]
			}
		});

		expect(detail?.status).toBe('newer_available');
		expect(detail?.source).toMatchObject({ kind: 'comm', provider: 'gmail' });
		expect(detail?.original).toMatchObject({ kind: 'comm', body: 'Bounded body' });
		if (detail?.original?.kind === 'comm') {
			expect(detail.original.evidence_messages).toHaveLength(1);
			expect(detail.original.evidence_messages[0].truncated).toBe(true);
		}
	});

	it('rejects missing ids and degrades unknown enums and malformed optional sections', () => {
		expect(mapResurfacingDetailPayload({ title: 'No id' })).toBeNull();
		const detail = mapResurfacingDetailPayload({
			candidate_id: 'cand-1',
			status: 'future_status',
			brief: { detail_status: 'future_status' },
			actions: 'bad',
			recommended_action: { kind: 'future_action', source: 'curator' }
		});
		expect(detail?.status).toBe('unavailable');
		expect(detail?.brief?.detail_status).toBe('partial');
		expect(detail?.actions).toEqual([]);
		expect(detail?.recommended_action).toBeNull();
	});

	it('preserves web source metadata and canonical open details', () => {
		const detail = mapResurfacingDetailPayload({
			candidate_id: 'web-1',
			source_kind: 'web',
			status: 'available',
			title: 'Useful launch',
			summary: 'A concise source summary',
			open_url: 'https://example.com/products/useful',
			source: { kind: 'web', url: 'https://example.com/products/useful' },
			original: {
				kind: 'web',
				url: 'https://example.com/products/useful',
				title: 'Useful launch',
				summary: 'A concise source summary'
			}
		});

		expect(detail?.open_url).toBe('https://example.com/products/useful');
		expect(detail?.source).toEqual({ kind: 'web', url: 'https://example.com/products/useful' });
		expect(detail?.original).toMatchObject({ kind: 'web', title: 'Useful launch' });
	});
});

describe('rich resurfacing API client', () => {
	it('uses distinct lazy detail and original endpoints', async () => {
		const fetchMock = vi
			.fn()
			.mockResolvedValueOnce(
				new Response(JSON.stringify({ candidate_id: 'cand/1', status: 'available' }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				})
			)
			.mockResolvedValueOnce(
				new Response(JSON.stringify({ candidate_id: 'cand/1', status: 'available' }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				})
			);
		vi.stubGlobal('fetch', fetchMock);

		await fetchResurfacingDetail('cand/1');
		await fetchResurfacingOriginal('cand/1');

		expect(fetchMock.mock.calls[0][0]).toContain('/cand%2F1/detail');
		expect(fetchMock.mock.calls[1][0]).toContain('/cand%2F1/original');
	});

	it('posts a bare dismiss, a reasoned dismiss, and never a reason on open/ack', async () => {
		const fetchMock = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
			new Response(JSON.stringify({
				ok: true,
				feedback_receipt: {
					outcome_id: 'outcome-1',
					outcome: 'irrelevant',
					surface: 'worth_a_look',
					feedback_recorded: true,
					affected_candidates: 18,
					rescore_status: 'completed'
				}
			}), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			})
		);
		vi.stubGlobal('fetch', fetchMock);
		const bodyOf = (call: number) => {
			// `fetch`'s init is optional, so prove the action actually sent a JSON
			// body rather than casting the absence away.
			const body = fetchMock.mock.calls[call][1]?.body;
			if (typeof body !== 'string') throw new Error(`fetch call ${call} sent no JSON body`);
			return JSON.parse(body);
		};

		await postResurfacingAction('cand/1', 'dismiss');
		expect(bodyOf(0)).toEqual({ action: 'dismiss' });

		const reasoned = await postResurfacingAction('cand/1', 'dismiss', 'not_relevant');
		expect(bodyOf(1)).toEqual({ action: 'dismiss', reason: 'not_relevant' });
		expect(reasoned.ok && reasoned.feedbackReceipt?.affected_candidates).toBe(18);

		// A reason on a non-dismiss action is dropped (bare body).
		await postResurfacingAction('cand/1', 'open', 'not_relevant');
		expect(bodyOf(2)).toEqual({ action: 'open' });

		await postResurfacingAction('cand/1', 'acknowledge', undefined, {
			decision_id: 'decision-1',
			candidate_id: 'cand/1',
			source_revision: 'revision-1',
			impression_id: 'impression-1'
		});
		expect(bodyOf(3)).toEqual({
			action: 'acknowledge',
			attribution: {
				decision_id: 'decision-1',
				candidate_id: 'cand/1',
				source_revision: 'revision-1',
				impression_id: 'impression-1'
			}
		});
	});

	it('uses offset directly for an arbitrary page request', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(JSON.stringify({
				cards: [],
				total: 100,
				limit: 5,
				offset: 95,
				cross_lane_reconciliation: crossLaneReconciliation(100, 0)
			}), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			})
		);
		vi.stubGlobal('fetch', fetchMock);
		await fetchResurfacingTodayPage({ limit: 5, offset: 95 });
		expect(String(fetchMock.mock.calls[0][0])).toContain('limit=5&offset=95');
	});

	it('posts idempotent contextual actions and maps the durable result', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(
				JSON.stringify({
					candidate_id: 'cand-1',
					action: 'create_task',
					result_ref: 'task-1',
					replayed: false,
					result: { kind: 'task', task_id: 'task-1', route: '/t/general/tasks?selected=task-1' }
				}),
				{ status: 200, headers: { 'Content-Type': 'application/json' } }
			)
		);
		vi.stubGlobal('fetch', fetchMock);

		const response = await postResurfacingContextualAction('cand-1', {
			kind: 'create_task',
			idempotency_key: '11111111-1111-4111-8111-111111111111',
			content_revision: 'rev-1',
			input: { title: 'Review change', instruction: 'Confirm impact' }
		});

		expect(response.result).toEqual({
			kind: 'task',
			task_id: 'task-1',
			route: '/t/general/tasks?selected=task-1'
		});
		const init = fetchMock.mock.calls[0][1] as RequestInit;
		expect(JSON.parse(String(init.body))).toMatchObject({
			kind: 'create_task',
			content_revision: 'rev-1',
			input: { title: 'Review change' }
		});
	});

	it('maps a native Apple Reminder receipt without inventing a task route', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(
				JSON.stringify({
					candidate_id: 'cand-1',
					action: 'create_reminder',
					result_ref: 'receipt-1',
					replayed: false,
					result: {
						kind: 'reminder',
						reminder_id: 'apple-reminder-1',
						provider: 'apple_reminders_macos',
						at: '2099-07-20T03:30:00Z',
						timezone: 'Asia/Kolkata'
					}
				}),
				{ status: 200, headers: { 'Content-Type': 'application/json' } }
			)
		);
		vi.stubGlobal('fetch', fetchMock);

		const response = await postResurfacingContextualAction('cand-1', {
			kind: 'create_reminder',
			idempotency_key: '11111111-1111-4111-8111-111111111112',
			content_revision: 'rev-1',
			input: {
				title: 'Review change',
				instruction: 'Confirm impact',
				at: '2099-07-20T03:30:00Z',
				timezone: 'Asia/Kolkata',
				delivery: 'host_apple_reminders'
			}
		});

		expect(response.result).toEqual({
			kind: 'reminder',
			reminder_id: 'apple-reminder-1',
			provider: 'apple_reminders_macos',
			app_url: null,
			task_id: null,
			route: null,
			at: '2099-07-20T03:30:00Z',
			timezone: 'Asia/Kolkata'
		});
	});

	it('preserves 409 status for stale-revision refresh behavior', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
					new Response(JSON.stringify({ error: 'content revision is stale', error_code: 'stale_revision' }), {
					status: 409,
					headers: { 'Content-Type': 'application/json' }
				})
			)
		);

		await expect(
			postResurfacingContextualAction('cand-1', {
				kind: 'save_to_memory',
				idempotency_key: '22222222-2222-4222-8222-222222222222',
				content_revision: 'old',
				input: { fact: 'A durable fact' }
			})
		).rejects.toMatchObject({
			status: 409,
			code: 'stale_revision'
		} satisfies Partial<ResurfacingApiError>);
	});

	it('parses stable conflict codes instead of inferring from status text', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				new Response(JSON.stringify({ error: 'still running', error_code: 'in_progress' }), {
					status: 409,
					headers: { 'Content-Type': 'application/json' }
				})
			)
		);
		await expect(
			postResurfacingContextualAction('cand-1', {
				kind: 'summarize_deeper',
				idempotency_key: '33333333-3333-4333-8333-333333333333'
			})
		).rejects.toMatchObject({ status: 409, code: 'in_progress' });
	});

	it('posts ordered read-recommendation telemetry', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ ok: true, recorded: true }), {
				status: 200,
				headers: { 'Content-Type': 'application/json' }
			})
		);
		vi.stubGlobal('fetch', fetchMock);

		await expect(
			postResurfacingRecommendationEvent('cand-1', {
				kind: 'view_details',
				content_revision: 'rev-1',
				event: 'selected'
			})
		).resolves.toEqual({ recorded: true });
		const init = fetchMock.mock.calls[0][1] as RequestInit;
		expect(JSON.parse(String(init.body))).toEqual({
			kind: 'view_details',
			content_revision: 'rev-1',
			event: 'selected'
		});
	});
});
