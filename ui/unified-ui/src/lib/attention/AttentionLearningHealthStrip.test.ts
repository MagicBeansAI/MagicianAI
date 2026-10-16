import { render } from 'svelte/server';
import { describe, expect, it } from 'vitest';

import AttentionLearningHealthStrip from './AttentionLearningHealthStrip.svelte';

describe('AttentionLearningHealthStrip', () => {
	it('keeps complete candidate volume visible beside learning coverage', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				surfaceLabel: 'Follow-up',
				semanticRankingEnabled: true,
				health: {
					total_active: 1014,
					source_family_counts: { promise: 961, comms_ingest: 53 },
					embedded_candidates: 810,
					embedding_coverage: 0.7988,
					learned_rank_changes: 64
				}
			}
		});

		expect(body).toContain('1,014');
		expect(body).toContain('80%');
		expect(body).toContain('Promise 961');
		expect(body).toContain('Learned order active');
		expect(body).toContain('learned ordering does not remove candidates');
	});

	it('shows shadow actionability coverage as preview-only and retains fallback volume', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				surfaceLabel: 'Worth a look',
				semanticRankingEnabled: true,
				actionability: {
					mode: 'shadow',
					snapshot_id: 'snapshot-42',
					semantic_extraction_coverage: 0.82,
					scored_count: 82,
					fallback_count: 18
				}
			}
		});

		expect(body).toContain('Actionability preview');
		expect(body).toContain('Semantic extraction');
		expect(body).toContain('82%');
		expect(body).toContain('Slice 1 fallback');
		expect(body).toContain('preview-only and does not change ordering');
		expect(body).not.toContain('Actionability ordering active');
	});

	it('labels only enforced actionability ordering as active', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				actionability: {
					mode: 'enforced',
					snapshot_id: null,
					semantic_extraction_coverage: 1,
					scored_count: 12,
					fallback_count: 2
				}
			}
		});
		expect(body).toContain('Actionability ordering active');
		expect(body).toContain('unscored candidates retain Slice 1 fallback ordering');
	});

	it('reconciles all candidates, members, clusters, and representatives before grouping', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				grouping: {
					mode: 'enforced',
					snapshot_id: 'group-snapshot-1',
					generation: 7,
					scope: 'eligible_universe',
					health: {
						candidate_total: 148,
						member_total: 148,
						cluster_total: 11,
						representative_total: 11,
						collapsed_member_total: 137,
						scored_pair_total: 210,
						cannot_link_total: 3,
						fallback_ungrouped_total: 2,
						totals_reconcile: true
					}
				}
			}
		});
		expect(body).toContain('Grouping active');
		expect(body).toContain('All candidates / members');
		expect(body).toContain('148 / 148');
		expect(body).toContain('11 / 11');
		expect(body).toContain('Exact');
		expect(body).toContain('expandable to every member and its lifecycle');
	});

	it('keeps shadow grouping preview-only without collapsing current cards', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				grouping: {
					mode: 'shadow',
					snapshot_id: 'group-snapshot-1',
					generation: 7,
					scope: 'eligible_universe',
					health: {
						candidate_total: 2,
						member_total: 2,
						cluster_total: 1,
						representative_total: 1,
						collapsed_member_total: 1,
						scored_pair_total: 1,
						cannot_link_total: 0,
						fallback_ungrouped_total: 0,
						totals_reconcile: true
					}
				}
			}
		});
		expect(body).toContain('Grouping preview');
		expect(body).toContain('every current card remains separate');
		expect(body).not.toContain('Grouping active');
	});

	it('shows exceeded pair-budget counts while retaining singleton totals', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				grouping: {
					mode: 'disabled',
					snapshot_id: null,
					generation: 7,
					scope: 'eligible_universe',
					health: {
						candidate_total: 12,
						member_total: 12,
						cluster_total: 12,
						representative_total: 12,
						collapsed_member_total: 0,
						scored_pair_total: 0,
						cannot_link_total: 0,
						fallback_ungrouped_total: 12,
						totals_reconcile: true,
						pair_evaluation_budget: 50,
						required_pair_evaluations: 66,
						budget_exceeded: true
					}
				}
			}
		});

		expect(body).toContain('Grouping inactive · pair budget exceeded');
		expect(body).toContain('Pair evaluations required / budget');
		expect(body).toContain('66 / 50');
		expect(body).toContain('12 / 12');
		expect(body).toContain('every original row remains visible as a singleton');
		expect(body).not.toContain('Grouping active');
	});

	it('keeps all-candidate routing and verified-impression diagnostics visible', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				routing: {
					decision: {
						decision_id: 'decision-1',
						decided_at: 1,
						surface: 'follow_up',
						routing_mode: 'shadow',
						routing_snapshot_id: 'routing-snapshot-1',
						eligible_item_count: 120,
						selected_item_count: 12,
						returned_item_count: 12,
						complete_universe_recorded: true,
						degradation_reason: 'impression_health_stale'
					},
					impression_policy: {
						min_visible_ms: 1_000,
						visibility_rule_version: 'visible-50-v1'
					},
					health: {
						routing_snapshot_valid: true,
						evaluated_count: 120,
						learned_route_count: 20,
						applied_route_count: 0,
						baseline_retained_count: 120,
						impression_eligible_count: 12,
						decision_item_coverage: 1,
						all_candidates_path: '/all-candidates/decision-1',
						verified_impression_coverage: 0.99,
						impression_dedupe_count: 4
					}
				}
			}
		});

		expect(body).toContain('Lane learning preview');
		expect(body).toContain('Evaluated / selected / returned');
		expect(body).toContain('120 / 12 / 12');
		expect(body).toContain('Verified impression coverage');
		expect(body).toContain('99%');
		expect(body).toContain('Impressions deduplicated');
		expect(body).toContain('All candidates');
		expect(body).toContain('API return and no interaction are not impressions or negative labels');
		expect(body).toContain('Routing degradation: impression health stale');
		expect(body).not.toContain('Lane-routing canary active');
	});

	it('shows bounded bandit health without claiming page-wide canary application', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				bandit: {
					mode: 'canary',
					policy_snapshot_id: 'policy-1',
					posterior_version: 13,
					posterior_update_count: 7,
					propensity_coverage: 0.96,
					exploration_rate: 0.04,
					support_ok: true,
					first_page_bounded: true,
					degradation_reason: null
				}
			}
		});

		expect(body).toContain('Personal ranking on');
		expect(body).toContain('Posterior version / updates');
		expect(body).toContain('13 / 7');
		expect(body).toContain('Propensity coverage');
		expect(body).toContain('96%');
		expect(body).toContain('Exploration rate');
		expect(body).toContain('4%');
		expect(body).toContain('First page only');
		expect(body).toContain('browser never samples or reorders cards');
		expect(body).not.toContain('Personal canary active');
	});

	it('shows full-universe semantic extraction and backfill diagnostics without hiding candidates', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				surfaceLabel: 'Follow-up',
				semanticExtraction: {
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
					queue: {
						pending: 2,
						in_flight: 1,
						active_in_flight: 1,
						expired_in_flight: 0,
						retry: 1,
						dead: 0,
						next_retry_at: null,
						oldest_ready_at: null,
						last_succeeded_at: 1_725_000_000_000
					}
				}
			}
		});

		expect(body).toContain('Semantic backfill active');
		expect(body).toContain('Semantic active / compatible');
		expect(body).toContain('10 / 7 · 70%');
		expect(body).toContain('Follow-up extraction states');
		expect(body).toContain('Worth extraction states');
		expect(body).toContain('missing 1 · invalid 0 · pending 0 · in-flight 0 · retry 1 · dead 0');
		expect(body).toContain('Backfill queue P / active / expired / R / dead');
		expect(body).toContain('2 / 1 / 0 / 1 / 0');
		expect(body).toContain('Extraction reconciliation');
		expect(body).toContain('Exact');
		expect(body).toContain('Schema 2 · attention-semantic-v2 · prompt attention-extract-v4');
		expect(body).toContain('gpt-5-mini / foreground-safe');
		expect(body).toContain('candidate-10 · 1725000000000');
		expect(body).toContain('full active scoped universe, not only this page');
		expect(body).toContain('browser performs no extraction or render-time LLM work');
		expect(body).toContain('remain visible in Slice 1 fallback order and are never filtered');
	});

	it('surfaces paused and degraded backfill state as system health only', () => {
		const emptyCounts = {
			active_total: 0,
			compatible_revision: 0,
			succeeded: 0,
			missing: 0,
			invalid: 0,
			pending: 0,
			in_flight: 0,
			retry: 0,
			dead: 0,
			coverage: 1
		};
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				semanticExtraction: {
					schema_version: 1,
					enabled: true,
					paused: true,
					pause_reason: 'foreground_pressure',
					degradation_reason: 'checkpoint_lease_expired',
					contract: {
						semantic_schema_version: 2,
						extractor_contract: 'attention-semantic-v2',
						prompt_version: 'attention-extract-v4',
						model: null,
						profile: null
					},
					checkpoint: {
						cursor: null,
						updated_at: null,
						lease_owner: null,
						lease_expires_at: null
					},
					totals: emptyCounts,
					surfaces: { follow_up: emptyCounts, worth_a_look: emptyCounts },
					queue: {
						pending: 0,
						in_flight: 0,
						active_in_flight: 0,
						expired_in_flight: 0,
						retry: 0,
						dead: 0,
						next_retry_at: null,
						oldest_ready_at: null,
						last_succeeded_at: null
					}
				}
			}
		});

		expect(body).toContain('Semantic backfill paused');
		expect(body).toContain('Pause reason');
		expect(body).toContain('Foreground Pressure');
		expect(body).toContain('Extraction degradation');
		expect(body).toContain('Checkpoint Lease Expired');
		expect(body).not.toContain('Semantic backfill active');
	});

	it('labels the shipped disabled state distinctly from a runtime pause', () => {
		const emptyCounts = {
			active_total: 0,
			compatible_revision: 0,
			succeeded: 0,
			missing: 0,
			invalid: 0,
			pending: 0,
			in_flight: 0,
			retry: 0,
			dead: 0,
			coverage: 1
		};
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				semanticExtraction: {
					schema_version: 1,
					enabled: false,
					paused: true,
					pause_reason: 'disabled',
					degradation_reason: null,
					contract: {
						semantic_schema_version: 1,
						extractor_contract: 'channel_attention_semantics_v1',
						prompt_version: '1.1.0',
						model: null,
						profile: null
					},
					checkpoint: {
						cursor: null,
						updated_at: null,
						lease_owner: null,
						lease_expires_at: null
					},
					totals: emptyCounts,
					surfaces: { follow_up: emptyCounts, worth_a_look: emptyCounts },
					queue: {
						pending: 0,
						in_flight: 0,
						active_in_flight: 0,
						expired_in_flight: 0,
						retry: 0,
						dead: 0,
						next_retry_at: null,
						oldest_ready_at: null,
						last_succeeded_at: null
					}
				}
			}
		});

		expect(body).toContain('Semantic backfill disabled');
		expect(body).not.toContain('Semantic backfill paused');
		expect(body).not.toContain('Semantic backfill active');
	});

	it('shows expired inference leases as recovery rather than healthy active work', () => {
		const emptyCounts = {
			active_total: 0,
			compatible_revision: 0,
			succeeded: 0,
			missing: 0,
			invalid: 0,
			pending: 0,
			in_flight: 0,
			retry: 0,
			dead: 0,
			coverage: 1
		};
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				semanticExtraction: {
					schema_version: 1,
					enabled: true,
					paused: false,
					pause_reason: null,
					degradation_reason: 'semantic_extraction_expired_leases',
					contract: {
						semantic_schema_version: 1,
						extractor_contract: 'channel_attention_semantics_v1',
						prompt_version: '1.1.0',
						model: 'gemma4:12b',
						profile: 'op-channel-classify-local'
					},
					checkpoint: {
						cursor: null,
						updated_at: null,
						lease_owner: null,
						lease_expires_at: null
					},
					totals: emptyCounts,
					surfaces: { follow_up: emptyCounts, worth_a_look: emptyCounts },
					queue: {
						pending: 12,
						in_flight: 4,
						active_in_flight: 1,
						expired_in_flight: 3,
						retry: 2,
						dead: 0,
						next_retry_at: null,
						oldest_ready_at: 1_725_000_000_000,
						last_succeeded_at: null
					}
				}
			}
		});

		expect(body).toContain('Semantic backfill recovering');
		expect(body).toContain('12 / 1 / 3 / 2 / 0');
		expect(body).toContain('gemma4:12b / op-channel-classify-local');
		expect(body).not.toContain('Semantic backfill active');
	});

	it('separates a blocked stage from one that was never enabled', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				surfaceLabel: 'Follow-up',
				semanticRankingEnabled: true,
				health: {
					total_active: 117,
					source_family_counts: { comms_ingest: 117 },
					embedded_candidates: 117,
					embedding_coverage: 1,
					learned_rank_changes: 4
				},
				routing: {
					decision: {
						decision_id: 'decision-blocked',
						decided_at: 1,
						surface: 'follow_up',
						routing_mode: 'baseline',
						routing_snapshot_id: null,
						eligible_item_count: 233,
						selected_item_count: 25,
						returned_item_count: 25,
						complete_universe_recorded: true,
						degradation_reason: null
					},
					impression_policy: {
						min_visible_ms: 1_000,
						visibility_rule_version: 'attention-visible-dwell-v1'
					},
					health: {
						routing_snapshot_valid: true,
						all_candidates_path: '/all-candidates/decision-blocked',
						evaluated_count: 233,
						learned_route_count: 0,
						applied_route_count: 0,
						baseline_retained_count: 233,
						impression_eligible_count: 233,
						decision_item_coverage: 1,
						verified_impression_coverage: 0
					}
				},
				bandit: {
					mode: 'disabled',
					policy_snapshot_id: null,
					posterior_version: 0,
					posterior_update_count: 0,
					propensity_coverage: 1,
					exploration_rate: 0,
					support_ok: true,
					first_page_bounded: true,
					degradation_reason: null
				}
			}
		});

		// Eligible impressions with none recorded is a break, not a disabled
		// feature: every stage downstream of it starves without a reward signal.
		expect(body).toContain('Verified impressions');
		expect(body).toContain('Blocked');
		expect(body).toContain('233');
		expect(body).toContain('Lane learning');
		expect(body).not.toContain('Personal bandit');
		expect(body).not.toContain('Ledger only');
	});

	it('still lists the bandit and grouping stages when the surface sends no data for them', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				// These assert on the pipeline and metrics, which the strip keeps
				// collapsed by default; the disclosure itself is covered separately.
				expanded: true,
				surfaceLabel: 'Follow-up',
				semanticRankingEnabled: true,
				health: {
					total_active: 138,
					source_family_counts: { follow_up: 138 },
					embedded_candidates: 0,
					embedding_coverage: 0,
					learned_rank_changes: 138
				}
			}
		});

		expect(body).toContain('Lane learning');
		expect(body).toContain('Ranked order');
		expect(body).not.toContain('Personal bandit');
		expect(body).not.toContain('Duplicate grouping');
	});

	it('holds the row with a skeleton while the health payload is still loading', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: { surfaceLabel: 'Attention', loading: true }
		});

		// Rendering nothing and then appearing shifts everything below it, so the
		// strip reserves its row until the first payload lands.
		expect(body).toContain('attention-learning-health__skeleton');
		expect(body).toContain('aria-busy="true"');
		expect(body).not.toContain('System health');
	});

	it('renders nothing when there is no data and nothing is loading', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: { surfaceLabel: 'Attention' }
		});
		expect(body).not.toContain('attention-learning-health');
	});

	it('collapses the diagnostic but still reports how many stages need attention', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				surfaceLabel: 'Follow-up',
				// Observe mode makes Ranked order degraded, so exactly one stage
				// needs attention while the per-stage verdicts stay hidden.
				semanticRankingEnabled: false,
				health: {
					total_active: 138,
					source_family_counts: { follow_up: 138 },
					embedded_candidates: 138,
					embedding_coverage: 1,
					learned_rank_changes: 0
				}
			}
		});

		expect(body).toContain('System health');
		expect(body).toContain('aria-expanded="false"');
		// Collapsing hides the verdicts, so a real problem has to survive into
		// the header or the strip becomes a place problems go to hide.
		expect(body).toContain('1 needs attention');
		expect(body).not.toContain('Ranked order');
		expect(body).not.toContain('Active candidates');
	});
	it('does not infer observe mode from an unavailable ranked projection', () => {
		const { body } = render(AttentionLearningHealthStrip, {props: {
			expanded: true, semanticRankingEnabled: null,
			health: {total_active: 1, source_family_counts: {}, embedded_candidates: 1, embedding_coverage: 1, learned_rank_changes: 0}
		}});
		expect(body).toContain('Waiting for a ranked projection');
		expect(body).not.toContain('observe mode');
	});

	it('keeps historical dead letters visible without degrading healthy active features', () => {
		const counts = {active_total: 10, compatible_revision: 10, succeeded: 10, missing: 0, invalid: 0, pending: 0, in_flight: 0, retry: 0, dead: 0, coverage: 1};
		const { body } = render(AttentionLearningHealthStrip, {props: {
			expanded: true, semanticRankingEnabled: true,
			semanticExtraction: {
				schema_version: 1, enabled: true, paused: false, pause_reason: null, degradation_reason: null,
				contract: {semantic_schema_version: 1, extractor_contract: 'channel_attention_semantics_v1', prompt_version: '1.1.0', model: 'luna', profile: 'remote'},
				checkpoint: {cursor: null, updated_at: null, lease_owner: null, lease_expires_at: null},
				totals: counts, surfaces: {follow_up: counts, worth_a_look: {...counts, active_total: 0, compatible_revision: 0, succeeded: 0}},
				queue: {pending: 0, in_flight: 0, active_in_flight: 0, expired_in_flight: 0, retry: 0, dead: 94, next_retry_at: null, oldest_ready_at: null, last_succeeded_at: null}
			}
		}});
		expect(body).toContain('100% covered');
		expect(body).toContain('0 / 0 / 0 / 0 / 94');
		expect(body).not.toContain('94 dead-letter rows');
		expect(body).not.toContain('needs attention');
	});

	const rankQueue = {
		schema_version: 1 as const,
		enabled: true,
		paused: false,
		pause_reason: null,
		queue: {
			pending: 2,
			in_flight: 1,
			retry: 0,
			succeeded: 331,
			stale: 64,
			dead: 0,
			next_retry_at: null,
			oldest_pending_at: 1_000
		},
		worker: {
			batch_size: 20,
			concurrency: 2,
			interval_secs: 60,
			max_retries: 5,
			lease_secs: 120,
			retention_days: 30
		}
	};

	it('shows the shared rank queue inside the expanded learning-health strip', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				expanded: true,
				semanticRankingEnabled: true,
				health: {
					total_active: 4,
					source_family_counts: {},
					embedded_candidates: 4,
					embedding_coverage: 1,
					learned_rank_changes: 0
				},
				rankRecompute: rankQueue
			}
		});

		expect(body).toContain('Rank recompute');
		expect(body).toContain('2 pending · 1 active');
		expect(body).toContain('Rank queue pending / active / retry');
		expect(body).toContain('2 / 1 / 0');
		expect(body).toContain('Rank outcomes succeeded / stale / dead');
		expect(body).toContain('331 / 64 / 0');
		expect(body).toContain('Card order changes on the next list load.');
		expect(body).not.toContain('needs attention');
	});

	it('names a retrying rank queue in the collapsed summary and hides the counts', () => {
		const { body } = render(AttentionLearningHealthStrip, {
			props: {
				semanticRankingEnabled: true,
				health: {
					total_active: 4,
					source_family_counts: {},
					embedded_candidates: 4,
					embedding_coverage: 1,
					learned_rank_changes: 0
				},
				rankRecompute: {
					...rankQueue,
					queue: { ...rankQueue.queue, pending: 0, in_flight: 0, retry: 3, dead: 1 }
				}
			}
		});

		expect(body).toContain('1 needs attention');
		expect(body).not.toContain('Rank queue pending / active / retry');
		expect(body).not.toContain('0 / 0 / 3');
	});

});
