import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	acknowledgeChannelFollowUp,
	commitChannelAction,
	dismissChannelFollowUpWithReason,
	fetchChannelFollowUpGroupMembers,
	fetchChannelFollowUpsPage,
	learnChannelWritingPreference,
	updateChannelWritingPreference
} from './channelNeedsYouStore';

function jsonResponse(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json' }
	});
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

afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe('channel follow-up quality controls', () => {
	it('lets projection-owning pages opt out and forwards cancellation', async () => {
		const controller = new AbortController();
		const fetchMock = vi.fn().mockResolvedValue(jsonResponse({
			items: [], total: 0, limit: 25, has_more: false
		}));
		vi.stubGlobal('fetch', fetchMock);

		await fetchChannelFollowUpsPage(25, 'cursor-1', {
			signal: controller.signal,
			includeCanonicalProjection: false
		});
		const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
		expect(url).toContain('cursor=cursor-1');
		expect(url).toContain('include_projection=false');
		expect(init.signal).toBe(controller.signal);
	});

	it('preserves server and end-to-end latency budget results', async () => {
		vi.spyOn(console, 'warn').mockImplementation(() => undefined);
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				jsonResponse({
					items: [],
					total: 0,
					limit: 50,
					has_more: false,
					latency: { projection_ms: 12, budget_ms: 250, within_budget: true }
				})
			)
		);
		const page = await fetchChannelFollowUpsPage();
		expect(page.ok).toBe(true);
		expect(page.latency?.projection_ms).toBe(12);
		expect(page.latency?.budget_ms).toBe(250);
		expect(page.latency?.within_budget).toBe(true);
	});

	it('preserves complete candidate health and learned-versus-baseline rank evidence', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				jsonResponse({
					items: [
						{
							annotation_id: 'ann-1',
							baseline_rank: 2,
							learned_rank: 9,
							rank_delta: -7,
							learning_score: 0.14
						}
					],
					total: 1014,
					limit: 25,
					has_more: true,
					semantic_ranking_enabled: true,
					health: {
						total_active: 1014,
						source_family_counts: { promise: 961, comms_ingest: 53 },
						embedded_candidates: 810,
						embedding_coverage: 0.7988,
						learned_rank_changes: 64
					}
				})
			)
		);

		const page = await fetchChannelFollowUpsPage(25);
		expect(page.total).toBe(1014);
		expect(page.health?.source_family_counts.promise).toBe(961);
		expect(page.semantic_ranking_enabled).toBe(true);
		expect(page.items[0]).toMatchObject({ baseline_rank: 2, learned_rank: 9, rank_delta: -7 });
	});

	it('normalizes additive shadow actionability without changing candidate visibility', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				jsonResponse({
					items: [
						{
							annotation_id: 'ann-1',
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
						},
						{ annotation_id: 'ann-old' }
					],
					total: 2,
					limit: 25,
					has_more: false,
					actionability_mode: 'shadow',
					actionability_snapshot_id: 'snapshot-42',
					semantic_extraction_coverage: 0.5,
					actionability_scored_count: 1,
					actionability_fallback_count: 1,
					semantic_extraction_health: semanticExtractionHealth()
				})
			)
		);

		const page = await fetchChannelFollowUpsPage(25);
		expect(page.items.map((item) => item.annotation_id)).toEqual(['ann-1', 'ann-old']);
		expect(page.actionability).toMatchObject({ mode: 'shadow', scored_count: 1 });
		expect(page.items[0].actionability).toMatchObject({
			probability: 0.84,
			mode: 'shadow',
			score_status: 'scored'
		});
		expect(page.items[1].actionability).toBeUndefined();
		expect(page.semantic_extraction).toMatchObject({
			totals: { active_total: 10, compatible_revision: 7, coverage: 0.7 },
			queue: { pending: 2, in_flight: 1, retry: 1, dead: 0 }
		});
	});

	it('fails malformed semantic diagnostics closed without dropping Follow-up rows', async () => {
		const malformedHealth = semanticExtractionHealth();
		malformedHealth.totals.pending = 3;
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				jsonResponse({
					items: [{ annotation_id: 'ann-1' }, { annotation_id: 'ann-2' }],
					total: 2,
					limit: 25,
					has_more: false,
					semantic_extraction_health: malformedHealth
				})
			)
		);

		const page = await fetchChannelFollowUpsPage(25);
		expect(page.items.map((item) => item.annotation_id)).toEqual(['ann-1', 'ann-2']);
		expect(page.semantic_extraction).toBeNull();
	});

	it('binds routing decision items to exact candidate revisions without filtering rows', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn().mockResolvedValue(
				jsonResponse({
					items: [
						{
							annotation_id: 'ann-1',
							candidate_id: 'ann-1',
							source_revision: 'distill:revision-1',
							decision_item: {
								decision_id: 'decision-1',
								// Rows carry the raw id; the decision ledger keys items by the
								// canonical surface-qualified form.
								candidate_id: 'follow_up:ann-1',
								source_revision: 'distill:revision-1',
								baseline_route: 'follow_up',
								learned_route: 'worth_a_look',
								served_route: 'follow_up',
								routing_mode: 'shadow',
								routing_snapshot_id: 'routing-snapshot-1',
								routing_model_version: 'lane-router-v1',
								learned_route_confidence: 0.93,
								utility_margin: 0.25,
								route_reason: 'shadow_only',
								served_rank: 1,
								selected: true,
								route_applied: false,
								canary_assigned: false
							},
							bandit_decision: {
								schema_version: 1,
								mode: 'shadow',
								policy_snapshot_id: 'policy-1',
								policy_model_version: 'contextual-ts-v1',
								posterior_version: 12,
								posterior_uncertainty: 0.31,
								proposed_position: 2,
								served_position: 1,
								served_propensity: 1,
								posterior_draw_count: 16,
								seed_identity: 'seed-1',
								support: true,
								exploration: false,
								applied: false,
								degradation_reason: null
							}
						},
						{ annotation_id: 'legacy-row' }
					],
					total: 2,
					limit: 25,
					has_more: false,
					decision: {
						decision_id: 'decision-1',
						decided_at: 1_725_000_000_000,
						surface: 'follow_up',
						routing_mode: 'shadow',
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
						applied_route_count: 0,
						baseline_retained_count: 2,
						impression_eligible_count: 1,
						decision_item_coverage: 0.5,
						all_candidates_path: '/all-candidates/decision-1'
					},
					bandit_health: {
						mode: 'shadow',
						policy_snapshot_id: 'policy-1',
						posterior_version: 12,
						posterior_update_count: 7,
						propensity_coverage: 0.5,
						exploration_rate: 0,
						support_ok: true,
						first_page_bounded: true,
						degradation_reason: null
					}
				})
			)
		);

		const page = await fetchChannelFollowUpsPage(25);
		expect(page.items.map((item) => item.annotation_id)).toEqual(['ann-1', 'legacy-row']);
		expect(page.routing?.health.evaluated_count).toBe(2);
		expect(page.items[0].decision_item).toMatchObject({
			candidate_id: 'follow_up:ann-1',
			source_revision: 'distill:revision-1',
			routing_mode: 'shadow',
			route_applied: false
		});
		expect(page.bandit).toMatchObject({ mode: 'shadow', posterior_update_count: 7 });
		expect(page.items[0].bandit_decision).toMatchObject({
			mode: 'shadow',
			proposed_position: 2,
			served_position: 1,
			applied: false
		});
		expect(page.items[1].decision_item).toBeUndefined();
	});

	it('keeps shadow groups flat and normalizes full enforced member expansions', async () => {
		const groupingHealth = {
			candidate_total: 2,
			member_total: 2,
			cluster_total: 1,
			representative_total: 1,
			collapsed_member_total: 1,
			scored_pair_total: 1,
			cannot_link_total: 0,
			fallback_ungrouped_total: 0,
			totals_reconcile: true
		};
		const grouping = (candidateId: string, isRepresentative: boolean) => ({
			cluster_id: 'cluster-1',
			representative_id: 'ann-1',
			is_representative: isRepresentative,
			member_count: 2,
			related_count: 1,
			model_version: 'pair-gbt-v1',
			snapshot_id: 'group-snapshot-1',
			merge_probability: candidateId === 'ann-1' ? null : 0.98
		});
		const items = [
			{
				annotation_id: 'ann-1',
				candidate_id: 'ann-1',
				source_revision: 'distill:rev-1',
				grouping: grouping('ann-1', true)
			},
			{
				annotation_id: 'ann-2',
				candidate_id: 'ann-2',
				source_revision: 'distill:rev-2',
				grouping: grouping('ann-2', false)
			}
		];
		const fetchMock = vi
			.fn()
			.mockResolvedValueOnce(
				jsonResponse({
					items,
					total: 2,
					grouping_mode: 'shadow',
					grouping_snapshot_id: 'group-snapshot-1',
					grouping_generation: 7,
					grouping_scope: 'eligible_universe',
					grouping_health: groupingHealth
				})
			)
			.mockResolvedValueOnce(jsonResponse({ cluster: grouping('ann-1', true), items, total: 2 }));
		vi.stubGlobal('fetch', fetchMock);

		const page = await fetchChannelFollowUpsPage(25);
		expect(page.items.map((item) => item.annotation_id)).toEqual(['ann-1', 'ann-2']);
		expect(page.grouping?.mode).toBe('shadow');
		expect(page.items[1].grouping).toMatchObject({ is_representative: false });

		const expanded = await fetchChannelFollowUpGroupMembers(
			'cluster-1',
			false,
			page.grouping
		);
		expect(expanded.ok).toBe(true);
		expect(expanded.total).toBe(2);
		expect(expanded.items.map((item) => item.source_revision)).toEqual([
			'distill:rev-1',
			'distill:rev-2'
		]);
		expect(fetchMock.mock.calls[1]?.[0]).toContain('/follow-ups/groups/cluster-1/members');
	});

	it('returns the typed feedback receipt from a reasoned dismissal', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			jsonResponse({
				feedback_receipt: {
					outcome_id: 'outcome-1',
					outcome: 'irrelevant',
					surface: 'follow_up',
					feedback_recorded: true,
					affected_candidates: 37,
					rescore_status: 'completed',
					embedding_contract: 'all-minilm-l6-v2',
					diagnostic_href: '/attention?outcome=outcome-1'
				}
			})
		);
		vi.stubGlobal('fetch', fetchMock);

		const result = await dismissChannelFollowUpWithReason('ann/1', 'spam');
		expect(result.ok && result.feedbackReceipt?.affected_candidates).toBe(37);
		expect(fetchMock.mock.calls[0]?.[0]).toContain('annotations/ann%2F1/dismiss');
		expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({ reason: 'spam' });
	});

	it('sends exact decision attribution for neutral acknowledge without requiring an impression', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			jsonResponse({
				feedback_receipt: {
					outcome_id: 'outcome-neutral-1',
					outcome: 'neutral_seen',
					surface: 'follow_up',
					feedback_recorded: true,
					affected_candidates: 0,
					rescore_status: 'degraded_no_candidates',
					posterior_update: {
						status: 'neutral',
						policy_snapshot_id: 'policy-1',
						posterior_version_before: 12,
						posterior_version_after: 12,
						attribution_quality: 'decision_only',
						degradation_reason: null,
						uncertainty_before: 0.31,
						uncertainty_after: 0.31,
						affected_rank_before: null,
						affected_rank_after: null,
						affected_rank_delta: null,
						rescore_scheduled: false
					}
				}
			})
		);
		vi.stubGlobal('fetch', fetchMock);

		const result = await acknowledgeChannelFollowUp('ann/1', {
			decision_id: 'decision-1',
			candidate_id: 'ann/1',
			source_revision: 'revision-1'
		});
		expect(result.ok && result.feedbackReceipt?.posterior_update?.status).toBe('neutral');
		expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({
			attribution: {
				decision_id: 'decision-1',
				candidate_id: 'ann/1',
				source_revision: 'revision-1'
			}
		});

		const mismatch = await acknowledgeChannelFollowUp('ann/1', {
			decision_id: 'decision-1',
			candidate_id: 'different-candidate',
			source_revision: 'revision-1',
			impression_id: 'impression-should-not-leak'
		});
		expect(mismatch.ok).toBe(true);
		expect(fetchMock.mock.calls[1]?.[1]?.body).toBeUndefined();
	});

	it('binds adapter-declared channel commits to the exact served decision', async () => {
		vi.stubGlobal('crypto', { randomUUID: () => 'event-channel-1' });
		const fetchMock = vi.fn().mockResolvedValue(
			jsonResponse({
				feedback_receipt: {
					outcome_id: 'event-channel-1',
					outcome: 'action_completed',
					surface: 'follow_up',
					feedback_recorded: true,
					affected_candidates: 4,
					rescore_status: 'completed',
					posterior_update: {
						status: 'updated',
						policy_snapshot_id: 'policy-1',
						posterior_version_before: 7,
						posterior_version_after: 8,
						attribution_quality: 'decision_only',
						degradation_reason: null,
						uncertainty_before: 0.4,
						uncertainty_after: 0.3,
						affected_rank_before: 3,
						affected_rank_after: null,
						affected_rank_delta: null,
						rescore_scheduled: true
					}
				}
			})
		);
		vi.stubGlobal('fetch', fetchMock);

		const result = await commitChannelAction(
			'ann/1',
			'reply',
			{ body: 'On it.' },
			{
				decision_id: 'decision-1',
				candidate_id: 'follow_up:ann/1',
				source_revision: 'distill:7',
				delivery_id: 'delivery-1'
			}
		);

		expect(result.ok && result.feedbackReceipt?.posterior_update?.status).toBe('updated');
		expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({
			body: 'On it.',
			event_id: 'event-channel-1',
			attribution: {
				decision_id: 'decision-1',
				candidate_id: 'follow_up:ann/1',
				source_revision: 'distill:7',
				delivery_id: 'delivery-1'
			}
		});
	});

	it('sends exact sender/domain preference feedback and promotion actions', async () => {
		const fetchMock = vi
			.fn()
			.mockResolvedValueOnce(
				jsonResponse({
					items: [
						{
							id: 'pref-1',
							provider: 'gmail',
							account_alias: 'work',
							scope_kind: 'domain',
							scope_value: 'example.com',
							statement: 'Keep replies concise.',
							status: 'candidate',
							evidence_count: 1,
							updated_at: 1
						}
					]
				}, 201)
			)
			.mockResolvedValueOnce(jsonResponse({ id: 'pref-1', status: 'promoted' }));
		vi.stubGlobal('fetch', fetchMock);

		const learned = await learnChannelWritingPreference(
			'ann/1',
			'domain',
			'Keep replies concise.',
			false
		);
		expect(learned.ok).toBe(true);
		expect(fetchMock.mock.calls[0]?.[0]).toContain('ann%2F1/writing-preferences');
		expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({
			scope: 'domain',
			statement: 'Keep replies concise.',
			promote: false
		});

		const promoted = await updateChannelWritingPreference('pref-1', 'promote');
		expect(promoted.ok).toBe(true);
		expect(fetchMock.mock.calls[1]?.[0]).toContain('writing-preferences/pref-1/promote');
	});
});
