<!-- Memory observability dashboard backed by scoped Parquet `memory_events`. -->
<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import MetricCard from '$lib/magician/components/generative/MetricCard.svelte';
	import BarChart from '$lib/magician/components/generative/BarChart.svelte';
	import LineChart from '$lib/magician/components/generative/LineChart.svelte';
	import Table from '$lib/magician/components/generative/Table.svelte';
	import { timedFetch } from '$lib/shared/fetch';

	interface MemoryTemperatureLaneCount {
		lane: string;
		count: number;
	}

	interface MemoryTemperatureTierCount {
		lane: string;
		temperature_tier: string;
		count: number;
	}

	interface MemoryTemperatureUtilityLabelCount {
		label: string;
		count: number;
	}

	interface MemoryTemperatureEntrySummary {
		memory_candidate_key: string;
		lane: string;
		temperature_tier: string;
		temperature_score: number;
		confidence?: number | null;
		retrieved_count: number;
		selected_count: number;
		injected_count: number;
		successful_use_count: number;
		failed_use_count: number;
		last_used_at_ms?: number | null;
		last_utility_review_label?: string | null;
		last_utility_review_confidence?: number | null;
		last_utility_review_run_id?: string | null;
		last_utility_review_reason?: string | null;
		superseded_by?: string | null;
		superseded_at_ms?: number | null;
		supersession_reason?: string | null;
		supersession_confidence?: number | null;
		supersession_source?: string | null;
		supersedes: string[];
	}

	interface MemorySupersessionChainNode {
		memory_candidate_key: string;
		lane: string;
		superseded_by?: string | null;
		superseded_at_ms?: number | null;
		supersession_reason?: string | null;
		supersession_confidence?: number | null;
		supersession_source?: string | null;
	}

	interface MemorySupersessionChainSummary {
		root_memory_candidate_key: string;
		chain: MemorySupersessionChainNode[];
	}

	interface MemoryHotProjectionSummary {
		source_memory_candidate_key: string;
		lane: string;
		temperature_tier: string;
		active: boolean;
		compact_text_preview: string;
		source_text_hash: string;
		source_ids: string[];
		source_tier_name?: string | null;
		source_item_key?: string | null;
		review_run_id?: string | null;
		review_label?: string | null;
		reviewer_confidence?: number | null;
		promotion_reason?: string | null;
		projection_policy_version: number;
		last_regenerated_at_ms?: number | null;
		regeneration_count: number;
		deactivated_at_ms?: number | null;
		deactivation_reason?: string | null;
		last_lifecycle_check_at_ms?: number | null;
		injected_count: number;
		last_verified_at_ms?: number | null;
		last_injected_at_ms?: number | null;
		updated_at_ms: number;
	}

	interface MemoryTemperatureState {
		overlay_updated_at_ms: number;
		overlay_entry_count: number;
		current_entry_count: number;
		superseded_entry_count: number;
		projection_updated_at_ms: number;
		projection_count: number;
		active_projection_count: number;
		inactive_projection_count: number;
		utility_queue: {
			active: number;
			eligible: number;
			retrying: number;
			dead: number;
			oldest_pending_age_secs?: number | null;
		};
		lane_counts: MemoryTemperatureLaneCount[];
		tier_counts: MemoryTemperatureTierCount[];
		utility_label_counts: MemoryTemperatureUtilityLabelCount[];
		top_entries: MemoryTemperatureEntrySummary[];
		superseded_entries: MemoryTemperatureEntrySummary[];
		supersession_chains: MemorySupersessionChainSummary[];
		active_projections: MemoryHotProjectionSummary[];
		inactive_projections: MemoryHotProjectionSummary[];
	}

	let lastRefreshed = Date.now();
	let evalRunInFlight = false;
	let evalRunStatus: string | null = null;
	let evalRunError: string | null = null;
	let temperatureState: MemoryTemperatureState | null = null;
	let temperatureStateLoading = false;
	let temperatureStateError: string | null = null;
	let visibleAnalyticsLevel = 0;
	let revealTimers: ReturnType<typeof setTimeout>[] = [];

	function refresh(): void {
		lastRefreshed = Date.now();
		void loadTemperatureState();
		window.dispatchEvent(new CustomEvent('magician:dashboard-refresh'));
	}

	function clearRevealTimers(): void {
		revealTimers.forEach((timer) => clearTimeout(timer));
		revealTimers = [];
	}

	function scheduleAnalyticsReveal(): void {
		clearRevealTimers();
		visibleAnalyticsLevel = 0;
		[
			{ level: 1, delay: 0 },
			{ level: 2, delay: 500 },
			{ level: 3, delay: 1_000 },
			{ level: 4, delay: 1_600 },
			{ level: 5, delay: 2_400 },
			{ level: 6, delay: 3_400 }
		].forEach(({ level, delay }) => {
			revealTimers.push(
				setTimeout(() => {
					visibleAnalyticsLevel = Math.max(visibleAnalyticsLevel, level);
				}, delay)
			);
		});
	}

	onMount(() => {
		scheduleAnalyticsReveal();
		void loadTemperatureState();
	});

	onDestroy(() => {
		clearRevealTimers();
	});

	async function loadTemperatureState(): Promise<void> {
		temperatureStateLoading = true;
		temperatureStateError = null;
		try {
			const response = await timedFetch('/api/magician/v2/memory/temperature/status');
			const payload = await response.json().catch(() => ({}));
			if (!response.ok) {
				throw new Error(payload?.error || `Memory temperature status failed with ${response.status}`);
			}
			temperatureState = normalizeTemperatureState(payload as Partial<MemoryTemperatureState>);
		} catch (error) {
			temperatureStateError = error instanceof Error ? error.message : String(error);
		} finally {
			temperatureStateLoading = false;
		}
	}

	function formatTimestamp(value?: number | null): string {
		if (!value) return 'never';
		return new Date(value).toLocaleString();
	}

	function formatNumber(value?: number | null, digits = 2): string {
		if (typeof value !== 'number' || Number.isNaN(value)) return '-';
		return value.toFixed(digits);
	}

	function normalizeTemperatureState(payload: Partial<MemoryTemperatureState>): MemoryTemperatureState {
		const supersededEntries = normalizeTemperatureEntries(payload.superseded_entries);
		const topEntries = normalizeTemperatureEntries(payload.top_entries);
		const supersededCount = payload.superseded_entry_count ?? supersededEntries.length;
		const overlayCount = payload.overlay_entry_count ?? topEntries.length + supersededCount;
		return {
			overlay_updated_at_ms: payload.overlay_updated_at_ms ?? 0,
			overlay_entry_count: overlayCount,
			current_entry_count: payload.current_entry_count ?? Math.max(0, overlayCount - supersededCount),
			superseded_entry_count: supersededCount,
			projection_updated_at_ms: payload.projection_updated_at_ms ?? 0,
			projection_count: payload.projection_count ?? 0,
			active_projection_count: payload.active_projection_count ?? 0,
			inactive_projection_count: payload.inactive_projection_count ?? 0,
			utility_queue: {
				active: payload.utility_queue?.active ?? 0,
				eligible: payload.utility_queue?.eligible ?? 0,
				retrying: payload.utility_queue?.retrying ?? 0,
				dead: payload.utility_queue?.dead ?? 0,
				oldest_pending_age_secs: payload.utility_queue?.oldest_pending_age_secs ?? null
			},
			lane_counts: Array.isArray(payload.lane_counts) ? payload.lane_counts : [],
			tier_counts: Array.isArray(payload.tier_counts) ? payload.tier_counts : [],
			utility_label_counts: Array.isArray(payload.utility_label_counts)
				? payload.utility_label_counts
				: [],
			top_entries: topEntries,
			superseded_entries: supersededEntries,
			supersession_chains: Array.isArray(payload.supersession_chains)
				? payload.supersession_chains
				: [],
			active_projections: Array.isArray(payload.active_projections) ? payload.active_projections : [],
			inactive_projections: Array.isArray(payload.inactive_projections)
				? payload.inactive_projections
				: []
		};
	}

	function normalizeTemperatureEntries(
		entries?: MemoryTemperatureEntrySummary[] | null
	): MemoryTemperatureEntrySummary[] {
		if (!Array.isArray(entries)) return [];
		return entries.map((entry) => ({
			...entry,
			supersedes: Array.isArray(entry.supersedes) ? entry.supersedes : []
		}));
	}

	function shortKey(value: string): string {
		if (!value) return '-';
		if (value.length <= 42) return value;
		return `${value.slice(0, 20)}...${value.slice(-16)}`;
	}

	function projectionSourceLabel(projection: MemoryHotProjectionSummary): string {
		const tier = projection.source_tier_name || projection.lane;
		const item = projection.source_item_key || projection.source_memory_candidate_key;
		return `${tier}/${item}`;
	}

	async function runMemoryEvalsNow(): Promise<void> {
		evalRunInFlight = true;
		evalRunStatus = null;
		evalRunError = null;
		try {
			const response = await timedFetch('/api/magician/v2/analytics/memory_events/evals/run', {
				method: 'POST'
			});
			const payload = await response.json().catch(() => ({}));
			if (!response.ok) {
				throw new Error(payload?.error || `Memory eval run failed with ${response.status}`);
			}
			const status = payload?.regression_status?.status ? ` · ${payload.regression_status.status}` : '';
			evalRunStatus = `Ran ${payload.case_count ?? 0} cases: ${payload.passed_count ?? 0} passed, ${payload.failed_count ?? 0} failed${status}`;
			refresh();
		} catch (error) {
			evalRunError = error instanceof Error ? error.message : String(error);
		} finally {
			evalRunInFlight = false;
		}
	}

	const retrievals24hSql =
		"SELECT COUNT(*) FROM memory_events WHERE event_kind = 'retrieval' AND selected = true AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 1 DAY)";
	const injectedItems7dSql =
		"SELECT COUNT(*) FROM memory_events WHERE event_kind = 'retrieval' AND selected = true AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)";
	const consolidation7dSql =
		"SELECT COUNT(*) FROM memory_events WHERE event_kind = 'consolidation_transform' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)";
	const avgCandidates24hSql =
		"SELECT AVG(candidate_count) FROM memory_events WHERE event_kind = 'retrieval' AND selected = true AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 1 DAY)";
	const hybridFallbacks24hSql =
		"SELECT COUNT(*) FROM memory_events WHERE event_kind = 'memory_retrieval_fallback' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 1 DAY)";
	const indexRebuildFailures24hSql =
		"SELECT COUNT(*) FROM memory_events WHERE event_kind IN ('memory_index_rebuild_failed', 'memory_index_reconcile_failed') AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 1 DAY)";
	const indexRowChanges7dSql =
		"SELECT SUM(input_count) FROM memory_events WHERE event_kind IN ('memory_index_lancedb_rows_updated', 'memory_index_lancedb_replaced') AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)";
	const indexOptimizeFailures7dSql =
		"SELECT COUNT(*) FROM memory_events WHERE event_kind = 'memory_index_lancedb_optimize_failed' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)";
	const avgSelectedRelevance24hSql =
		"SELECT AVG(score) FROM memory_events WHERE event_kind = 'retrieval' AND selected = true AND score IS NOT NULL AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 1 DAY)";
	const selectionLift24hSql =
		"SELECT AVG(CASE WHEN selected = true THEN score ELSE NULL END) - AVG(CASE WHEN selected = false THEN score ELSE NULL END) " +
		"FROM memory_events WHERE event_kind = 'retrieval' AND score IS NOT NULL AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 1 DAY)";
	const evalPassRate7dSql =
		"SELECT CAST(SUM(CASE WHEN eval_pass THEN 1 ELSE 0 END) * 1.0 / NULLIF(COUNT(*), 0) AS DOUBLE) FROM memory_events WHERE event_kind = 'eval_case' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)";
	const evalCases7dSql =
		"SELECT COUNT(*) FROM memory_events WHERE event_kind = 'eval_case' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)";
	const regressionStatusSql =
		"SELECT status FROM memory_events WHERE event_kind = 'memory_regression_status' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) ORDER BY timestamp_ms DESC LIMIT 1";
	const regressionFailuresSql =
		"SELECT dropped_count FROM memory_events WHERE event_kind = 'memory_regression_status' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) ORDER BY timestamp_ms DESC LIMIT 1";
	const utilityReviews24hSql =
		"SELECT COUNT(*) FROM memory_events WHERE event_kind = 'memory_temperature_utility_review' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 1 DAY)";
	const loadBearing7dSql =
		"SELECT COUNT(*) FROM memory_events WHERE event_kind = 'memory_temperature_utility_judgement' AND target = 'load_bearing' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)";
	const temperaturePromotions7dSql =
		"SELECT COALESCE(SUM(input_count), 0) FROM memory_events WHERE event_kind = 'memory_temperature_utility_review' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)";
	const temperatureDemotions7dSql =
		"SELECT COALESCE(SUM(skipped_count), 0) FROM memory_events WHERE event_kind = 'memory_temperature_utility_review' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)";
	const hotProjectionUpserts7dSql =
		"SELECT COUNT(*) FROM memory_events WHERE event_kind = 'memory_hot_projection_upserted' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)";
	const evalBackendExpr =
		"COALESCE(json_extract_string(payload_json, '$.retrieval_backend'), 'unknown')";

	const eventTimelineSql =
		"SELECT (timestamp_ms / 3600000)::BIGINT * 3600000 AS bucket_ms, event_kind, COUNT(*) AS events " +
		"FROM memory_events WHERE timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) " +
		"GROUP BY 1, 2 ORDER BY 1, 2";

	const topInjectedTiersSql =
		"SELECT tier_name, COUNT(*) AS injected FROM memory_events " +
		"WHERE event_kind = 'retrieval' AND selected = true AND tier_name IS NOT NULL AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) " +
		"GROUP BY tier_name ORDER BY injected DESC LIMIT 10";

	const droppedByScopeSql =
		"SELECT scope, SUM(dropped_count) AS dropped FROM memory_events " +
		"WHERE event_kind = 'retrieval' AND selected = true AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) " +
		"GROUP BY scope ORDER BY dropped DESC LIMIT 10";

	const consolidationByRuleSql =
		"SELECT rule_name, SUM(output_count) AS emitted, SUM(skipped_count) AS skipped FROM memory_events " +
		"WHERE event_kind = 'consolidation_transform' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 14 DAYS) AND rule_name IS NOT NULL " +
		"GROUP BY rule_name ORDER BY emitted DESC NULLS LAST LIMIT 10";

	const retrievalQualitySql =
		"SELECT agent_id, scope, COUNT(*) AS selected_items, AVG(score) AS avg_score, AVG(candidate_count) AS avg_candidates, SUM(dropped_count) AS dropped " +
		"FROM memory_events WHERE event_kind = 'retrieval' AND selected = true AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) " +
		"GROUP BY agent_id, scope ORDER BY selected_items DESC LIMIT 20";

	const indexMaintenanceSql =
		"SELECT event_kind, status, COALESCE(source_kind, 'unknown') AS mode, COUNT(*) AS events, SUM(input_count) AS input_rows, SUM(output_count) AS output_rows " +
		"FROM memory_events WHERE event_kind LIKE 'memory_index_lancedb_%' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) " +
		"GROUP BY event_kind, status, mode ORDER BY events DESC, event_kind ASC";

	const relevanceTimelineSql =
		"SELECT (timestamp_ms / 3600000)::BIGINT * 3600000 AS bucket_ms, " +
		"CASE WHEN selected THEN 'selected' ELSE 'dropped' END AS selection, AVG(score) AS avg_score " +
		"FROM memory_events WHERE event_kind = 'retrieval' AND score IS NOT NULL AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) " +
		"GROUP BY 1, 2 ORDER BY 1, 2";

	const relevanceDistributionSql =
		"SELECT score_bucket || ' · ' || selection AS bucket_selection, COUNT(*) AS candidates FROM (" +
		"SELECT score, CASE WHEN selected THEN 'selected' ELSE 'dropped' END AS selection, " +
		"CASE WHEN score < 50 THEN '000-049' WHEN score < 100 THEN '050-099' WHEN score < 150 THEN '100-149' " +
		"WHEN score < 250 THEN '150-249' WHEN score < 400 THEN '250-399' ELSE '400+' END AS score_bucket, " +
		"CASE WHEN score < 50 THEN 1 WHEN score < 100 THEN 2 WHEN score < 150 THEN 3 WHEN score < 250 THEN 4 WHEN score < 400 THEN 5 ELSE 6 END AS bucket_order " +
		"FROM memory_events WHERE event_kind = 'retrieval' AND score IS NOT NULL AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS)" +
		") GROUP BY score_bucket, selection, bucket_order ORDER BY bucket_order, selection DESC";

	const relevanceByBackendSql =
		"SELECT COALESCE(retrieval_backend, 'unknown') AS backend, CASE WHEN selected THEN 'selected' ELSE 'dropped' END AS selection, " +
		"COUNT(*) AS candidates, AVG(score) AS avg_score, MIN(score) AS min_score, MAX(score) AS max_score " +
		"FROM memory_events WHERE event_kind = 'retrieval' AND score IS NOT NULL AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) " +
		"GROUP BY backend, selection ORDER BY backend, selection DESC";

	const relevanceByTierSql =
		"SELECT COALESCE(tier_name, 'unknown') AS tier_name, COALESCE(scope, 'unknown') AS scope, " +
		"SUM(CASE WHEN selected THEN 1 ELSE 0 END) AS selected_items, SUM(CASE WHEN selected THEN 0 ELSE 1 END) AS dropped_items, " +
		"AVG(CASE WHEN selected THEN score ELSE NULL END) AS avg_selected_score, AVG(CASE WHEN selected = false THEN score ELSE NULL END) AS avg_dropped_score " +
		"FROM memory_events WHERE event_kind = 'retrieval' AND score IS NOT NULL AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) " +
		"GROUP BY tier_name, scope ORDER BY selected_items DESC, avg_selected_score DESC LIMIT 25";

	const relevanceByTemperatureSql =
		"SELECT COALESCE(json_extract_string(payload_json, '$.semantic_memory_type'), 'unknown') AS lane, " +
		"COALESCE(json_extract_string(payload_json, '$.temperature_tier'), 'unknown') AS temperature, " +
		"SUM(CASE WHEN selected THEN 1 ELSE 0 END) AS selected_items, SUM(CASE WHEN selected THEN 0 ELSE 1 END) AS dropped_items, " +
		"AVG(CASE WHEN selected THEN score ELSE NULL END) AS avg_selected_score " +
		"FROM memory_events WHERE event_kind = 'retrieval' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) " +
		"GROUP BY lane, temperature ORDER BY selected_items DESC, avg_selected_score DESC LIMIT 32";

	const coldMemoryWinsSql =
		"SELECT timestamp_ms, agent_id, scope, tier_name, item_key, json_extract_string(payload_json, '$.semantic_memory_type') AS lane, " +
		"json_extract_string(payload_json, '$.temperature_tier') AS temperature, score, confidence, retrieval_backend, query_excerpt " +
		"FROM memory_events WHERE event_kind = 'retrieval' AND selected = true " +
		"AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) " +
		"AND json_extract_string(payload_json, '$.temperature_tier') IN ('t2', 't3') " +
		"ORDER BY score DESC NULLS LAST, timestamp_ms DESC LIMIT 25";

	const lowScoreSelectedSql =
		"SELECT timestamp_ms, agent_id, scope, tier_name, item_key, score, confidence, retrieval_backend, query_excerpt " +
		"FROM memory_events WHERE event_kind = 'retrieval' AND selected = true AND score IS NOT NULL AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) " +
		"ORDER BY score ASC, timestamp_ms DESC LIMIT 25";

	const utilityJudgementByLabelSql =
		"SELECT COALESCE(target, status, 'unknown') AS label, COUNT(*) AS judgements, AVG(confidence) AS avg_confidence " +
		"FROM memory_events WHERE event_kind = 'memory_temperature_utility_judgement' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) " +
		"GROUP BY label ORDER BY judgements DESC";

	const utilityJudgementByLaneSql =
		"SELECT COALESCE(scope, 'unknown') AS lane, COALESCE(target, status, 'unknown') AS label, COUNT(*) AS judgements, AVG(confidence) AS avg_confidence " +
		"FROM memory_events WHERE event_kind = 'memory_temperature_utility_judgement' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 7 DAYS) " +
		"GROUP BY lane, label ORDER BY judgements DESC, lane LIMIT 32";

	const latestUtilityReviewsSql =
		"SELECT timestamp_ms, agent_id, scope, selected_count AS reviewed, input_count AS promoted, skipped_count AS demoted, output_count AS hot_projections, status, " +
		"json_extract_string(payload_json, '$.run_id') AS run_id, json_extract_string(payload_json, '$.outcome') AS outcome " +
		"FROM memory_events WHERE event_kind = 'memory_temperature_utility_review' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) ORDER BY timestamp_ms DESC LIMIT 25";

	const latestHotProjectionsSql =
		"SELECT timestamp_ms, agent_id, scope AS lane, tier_name AS review_label, item_key, confidence, output_chars, " +
		"json_extract_string(payload_json, '$.source_tier_name') AS source_tier, json_extract_string(payload_json, '$.source_item_key') AS source_item, " +
		"json_extract(payload_json, '$.projection_policy_version') AS policy_version, json_extract_string(payload_json, '$.run_id') AS run_id, " +
		"json_extract_string(payload_json, '$.promotion_reason') AS reason " +
		"FROM memory_events WHERE event_kind = 'memory_hot_projection_upserted' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) ORDER BY timestamp_ms DESC LIMIT 25";

	const recentConsolidationsSql =
		"SELECT timestamp_ms, agent_id, rule_name, target, source_kind, input_count, output_count, skipped_count, status " +
		"FROM memory_events WHERE event_kind = 'consolidation_transform' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) " +
		"ORDER BY timestamp_ms DESC LIMIT 25";

	const evalBySuiteSql =
		"SELECT eval_suite, COUNT(*) AS cases, SUM(CASE WHEN eval_pass THEN 1 ELSE 0 END) AS passed, " +
		"SUM(CASE WHEN NOT eval_pass THEN 1 ELSE 0 END) AS failed, AVG(best_rank) AS avg_best_rank " +
		"FROM memory_events WHERE event_kind = 'eval_case' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 14 DAYS) " +
		"GROUP BY eval_suite ORDER BY failed DESC, cases DESC LIMIT 20";

	const evalByBackendSql =
		"SELECT " + evalBackendExpr + " AS backend, COUNT(*) AS cases, " +
		"CAST(AVG(CASE WHEN eval_pass THEN 1.0 ELSE 0.0 END) AS DOUBLE) AS pass_rate, " +
		"SUM(CASE WHEN eval_pass THEN 0 ELSE 1 END) AS failures, AVG(best_rank) AS avg_best_rank " +
		"FROM memory_events WHERE event_kind = 'eval_case' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 14 DAYS) " +
		"GROUP BY backend ORDER BY failures DESC, cases DESC";

	const evalBackendBySuiteSql =
		"SELECT eval_suite, " + evalBackendExpr + " AS backend, COUNT(*) AS cases, " +
		"CAST(AVG(CASE WHEN eval_pass THEN 1.0 ELSE 0.0 END) AS DOUBLE) AS pass_rate, AVG(best_rank) AS avg_best_rank " +
		"FROM memory_events WHERE event_kind = 'eval_case' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 14 DAYS) " +
		"GROUP BY eval_suite, backend ORDER BY eval_suite, backend";

	const recentEvalFailuresSql =
		"SELECT timestamp_ms, eval_suite, eval_case_id, agent_id, scope, eval_query, expected_count, matched_count, best_rank, status " +
		"FROM memory_events WHERE event_kind = 'eval_case' AND eval_pass = false AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) " +
		"ORDER BY timestamp_ms DESC LIMIT 25";

	const evalTimelineSql =
		"SELECT (timestamp_ms / 3600000)::BIGINT * 3600000 AS bucket_ms, " +
		"CASE WHEN eval_pass THEN 'passed' ELSE 'failed' END AS result, COUNT(*) AS cases " +
		"FROM memory_events WHERE event_kind = 'eval_case' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 14 DAYS) " +
		"GROUP BY 1, 2 ORDER BY 1, 2";

	const evalPassRateByAgentSql =
		"SELECT COALESCE(agent_id, 'unknown') AS agent_id, COUNT(*) AS cases, " +
		"CAST(AVG(CASE WHEN eval_pass THEN 1.0 ELSE 0.0 END) AS DOUBLE) AS pass_rate, " +
		"SUM(CASE WHEN eval_pass THEN 0 ELSE 1 END) AS failures, AVG(best_rank) AS avg_best_rank " +
		"FROM memory_events WHERE event_kind = 'eval_case' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 14 DAYS) " +
		"GROUP BY agent_id ORDER BY failures DESC, cases DESC";

	const evalStatusBreakdownSql =
		"SELECT status, COUNT(*) AS cases FROM memory_events " +
		"WHERE event_kind = 'eval_case' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 14 DAYS) " +
		"GROUP BY status ORDER BY cases DESC";

	const evalWorstCasesSql =
		"SELECT eval_suite, eval_case_id, agent_id, COUNT(*) AS runs, " +
		"SUM(CASE WHEN eval_pass THEN 0 ELSE 1 END) AS failures, " +
		"CAST(AVG(CASE WHEN eval_pass THEN 1.0 ELSE 0.0 END) AS DOUBLE) AS pass_rate, AVG(best_rank) AS avg_best_rank " +
		"FROM memory_events WHERE event_kind = 'eval_case' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) " +
		"GROUP BY eval_suite, eval_case_id, agent_id ORDER BY failures DESC, pass_rate ASC, runs DESC LIMIT 25";

	const evalRankDistributionSql =
		"SELECT CASE " +
		"WHEN best_rank IS NULL THEN 'missing' WHEN best_rank <= 1 THEN 'rank 1' WHEN best_rank <= 3 THEN 'rank 2-3' " +
		"WHEN best_rank <= 5 THEN 'rank 4-5' ELSE 'rank 6+' END AS rank_bucket, COUNT(*) AS cases " +
		"FROM memory_events WHERE event_kind = 'eval_case' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 14 DAYS) " +
		"GROUP BY rank_bucket ORDER BY MIN(COALESCE(best_rank, 9999))";

	const latestEvalRunsSql =
		"SELECT timestamp_ms, eval_suite, eval_case_id, agent_id, scope, " + evalBackendExpr + " AS backend, eval_pass, matched_count, expected_count, selected_count, best_rank, status " +
		"FROM memory_events WHERE event_kind = 'eval_case' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) ORDER BY timestamp_ms DESC LIMIT 50";

	const evalFailureDrilldownSql =
		"SELECT timestamp_ms, eval_suite, eval_case_id, agent_id, scope, eval_query, expected_count, matched_count, best_rank, status, " +
		"substr(COALESCE(payload_json, ''), 1, 1200) AS payload_excerpt " +
		"FROM memory_events WHERE event_kind = 'eval_case' AND eval_pass = false AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) " +
		"ORDER BY timestamp_ms DESC LIMIT 20";

	const regressionStatusHistorySql =
		"SELECT timestamp_ms, status, candidate_count AS cases, selected_count AS passed, dropped_count AS failed, " +
		"json_extract_string(payload_json, '$.status_reason') AS reason, " +
		"json_extract_string(payload_json, '$.direct_fallback_count') AS direct_fallback_count " +
		"FROM memory_events WHERE event_kind = 'memory_regression_status' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) ORDER BY timestamp_ms DESC LIMIT 20";

	const consolidationTrendSql =
		"SELECT (timestamp_ms / 3600000)::BIGINT * 3600000 AS bucket_ms, status, COUNT(*) AS transforms " +
		"FROM memory_events WHERE event_kind = 'consolidation_transform' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 14 DAYS) " +
		"GROUP BY 1, 2 ORDER BY 1, 2";

	const consolidationStatusSql =
		"SELECT status, COUNT(*) AS transforms FROM memory_events " +
		"WHERE event_kind = 'consolidation_transform' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 14 DAYS) " +
		"GROUP BY status ORDER BY transforms DESC";

	const consolidationTargetQualitySql =
		"SELECT COALESCE(target, 'unknown') AS target, COUNT(*) AS transforms, SUM(input_count) AS inputs, SUM(output_count) AS emitted, " +
		"SUM(skipped_count) AS skipped, AVG(CASE WHEN input_count > 0 THEN output_count * 1.0 / input_count ELSE NULL END) AS output_per_input " +
		"FROM memory_events WHERE event_kind = 'consolidation_transform' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 14 DAYS) " +
		"GROUP BY target ORDER BY skipped DESC NULLS LAST, emitted DESC NULLS LAST LIMIT 25";

	const consolidationSkippedByRuleSql =
		"SELECT COALESCE(rule_name, 'unknown') AS rule_name, SUM(skipped_count) AS skipped, SUM(output_count) AS emitted, COUNT(*) AS transforms " +
		"FROM memory_events WHERE event_kind = 'consolidation_transform' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 14 DAYS) " +
		"GROUP BY rule_name ORDER BY skipped DESC NULLS LAST LIMIT 20";

	const consolidationSourceKindSql =
		"SELECT COALESCE(source_kind, 'unknown') AS source_kind, COUNT(*) AS transforms, SUM(input_count) AS inputs, SUM(output_count) AS emitted " +
		"FROM memory_events WHERE event_kind = 'consolidation_transform' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 14 DAYS) " +
		"GROUP BY source_kind ORDER BY transforms DESC LIMIT 20";

	const recentConsolidationIssuesSql =
		"SELECT timestamp_ms, agent_id, rule_name, target, source_kind, input_count, output_count, skipped_count, status, substr(COALESCE(payload_json, ''), 1, 600) AS payload_excerpt " +
		"FROM memory_events WHERE event_kind = 'consolidation_transform' AND timestamp_ms > epoch_ms(CAST(now() AS TIMESTAMP) - INTERVAL 30 DAYS) AND (status <> 'ok' OR COALESCE(skipped_count, 0) > 0 OR COALESCE(output_count, 0) = 0) " +
		"ORDER BY timestamp_ms DESC LIMIT 25";
</script>

<section class="memory-observability">
	<header class="memory-observability-header">
		<div>
			<h2>Memory observability</h2>
			<p>Prompt retrieval, drops, consolidation output, and quality signals from scoped Parquet.</p>
		</div>
		<div class="memory-observability-actions">
			<button type="button" on:click={runMemoryEvalsNow} disabled={evalRunInFlight}>
				{evalRunInFlight ? 'Running evals...' : 'Run evals now'}
			</button>
			<button type="button" on:click={refresh} title={new Date(lastRefreshed).toLocaleString()}>
				Refresh
			</button>
		</div>
	</header>
	{#if evalRunStatus}
		<p class="memory-run-status">{evalRunStatus}</p>
	{/if}
	{#if evalRunError}
		<p class="memory-run-status memory-run-status-error">{evalRunError}</p>
	{/if}

	<section class="memory-widget memory-temperature-state">
		<div class="memory-temperature-state-head">
			<div>
				<h3>Temperature current state</h3>
				<p>Current overlay inventory and active hot projections from scoped memory storage.</p>
			</div>
			<div class="memory-temperature-state-meta">
				{#if temperatureStateLoading}
					<span>Loading</span>
				{:else if temperatureState}
					<span>Overlay {formatTimestamp(temperatureState.overlay_updated_at_ms)}</span>
					<span>Projections {formatTimestamp(temperatureState.projection_updated_at_ms)}</span>
				{/if}
			</div>
		</div>
		{#if temperatureStateError}
			<p class="memory-run-status memory-run-status-error">{temperatureStateError}</p>
		{:else if temperatureState}
			<div class="memory-temperature-summary">
				<div>
					<strong>{temperatureState.overlay_entry_count}</strong>
					<span>Overlay entries</span>
				</div>
				<div>
					<strong>{temperatureState.current_entry_count}</strong>
					<span>Current entries</span>
				</div>
				<div>
					<strong>{temperatureState.superseded_entry_count}</strong>
					<span>Superseded entries</span>
				</div>
				<div>
					<strong>{temperatureState.projection_count}</strong>
					<span>Projection records</span>
				</div>
				<div>
					<strong>{temperatureState.active_projection_count}</strong>
					<span>Active projections</span>
				</div>
				<div>
					<strong>{temperatureState.inactive_projection_count}</strong>
					<span>Inactive projections</span>
				</div>
				<div>
					<strong>{temperatureState.utility_label_counts.reduce((sum, item) => sum + item.count, 0)}</strong>
					<span>Reviewed entries</span>
				</div>
			</div>

			<div class="memory-temperature-grid">
				<div class="memory-temperature-subpanel">
					<h4>Utility maintenance queue</h4>
					<div class="memory-pill-list">
						<span class="memory-pill"><b>active</b>{temperatureState.utility_queue.active}</span>
						<span class="memory-pill"><b>due</b>{temperatureState.utility_queue.eligible}</span>
						<span class="memory-pill"><b>retrying</b>{temperatureState.utility_queue.retrying}</span>
						<span class="memory-pill"><b>dead</b>{temperatureState.utility_queue.dead}</span>
						<span class="memory-pill"><b>oldest</b>{temperatureState.utility_queue.oldest_pending_age_secs == null ? '-' : `${temperatureState.utility_queue.oldest_pending_age_secs}s`}</span>
					</div>
				</div>
				<div class="memory-temperature-subpanel">
					<h4>Lane inventory</h4>
					<div class="memory-pill-list">
						{#each temperatureState.lane_counts as item}
							<span class="memory-pill"><b>{item.lane}</b>{item.count}</span>
						{/each}
					</div>
				</div>
				<div class="memory-temperature-subpanel">
					<h4>Temperature by lane</h4>
					<div class="memory-pill-list">
						{#each temperatureState.tier_counts as item}
							<span class="memory-pill"><b>{item.lane}:{item.temperature_tier}</b>{item.count}</span>
						{/each}
					</div>
				</div>
			</div>

			<div class="memory-temperature-tables">
				<div class="memory-temperature-subpanel memory-temperature-subpanel-full">
					<h4>Top temperature entries</h4>
					<div class="memory-state-table-wrap">
						<table class="memory-state-table">
							<thead>
								<tr>
									<th>Memory</th>
									<th>Lane</th>
									<th>Tier</th>
									<th>Score</th>
									<th>Use</th>
									<th>Review</th>
								</tr>
							</thead>
							<tbody>
								{#each temperatureState.top_entries.slice(0, 12) as entry}
									<tr>
										<td title={entry.memory_candidate_key}>{shortKey(entry.memory_candidate_key)}</td>
										<td>{entry.lane}</td>
										<td>{entry.temperature_tier}</td>
										<td>{formatNumber(entry.temperature_score, 1)}</td>
										<td>{entry.successful_use_count}/{entry.failed_use_count}</td>
										<td title={entry.last_utility_review_reason || ''}>
											{entry.last_utility_review_label || '-'}
										</td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
				</div>
				<div class="memory-temperature-subpanel memory-temperature-subpanel-full">
					<h4>Active hot projections</h4>
					<div class="memory-state-table-wrap">
						<table class="memory-state-table">
							<thead>
								<tr>
									<th>Source</th>
									<th>Lane</th>
									<th>Tier</th>
									<th>Source item</th>
									<th>Policy</th>
									<th>Injected</th>
									<th>Preview</th>
								</tr>
							</thead>
							<tbody>
								{#each temperatureState.active_projections.slice(0, 12) as projection}
									<tr>
										<td title={projection.source_memory_candidate_key}>{shortKey(projection.source_memory_candidate_key)}</td>
										<td>{projection.lane}</td>
										<td>{projection.temperature_tier}</td>
										<td title={projectionSourceLabel(projection)}>{shortKey(projectionSourceLabel(projection))}</td>
										<td>v{projection.projection_policy_version}</td>
										<td>{projection.injected_count}</td>
										<td title={projection.promotion_reason || ''}>{projection.compact_text_preview}</td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
				</div>
			</div>
			{#if temperatureState.superseded_entries.length > 0}
				<div class="memory-temperature-subpanel">
					<h4>Superseded memory</h4>
					<div class="memory-state-table-wrap">
						<table class="memory-state-table">
							<thead>
								<tr>
									<th>Memory</th>
									<th>Lane</th>
									<th>By</th>
									<th>Reason</th>
									<th>When</th>
								</tr>
							</thead>
							<tbody>
								{#each temperatureState.superseded_entries.slice(0, 12) as entry}
									<tr>
										<td title={entry.memory_candidate_key}>{shortKey(entry.memory_candidate_key)}</td>
										<td>{entry.lane}</td>
										<td title={entry.superseded_by || ''}>{entry.superseded_by ? shortKey(entry.superseded_by) : '-'}</td>
										<td title={entry.supersession_reason || ''}>{entry.supersession_reason || '-'}</td>
										<td>{formatTimestamp(entry.superseded_at_ms)}</td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
				</div>
			{/if}
			{#if temperatureState.supersession_chains.length > 0}
				<div class="memory-temperature-subpanel">
					<h4>Supersession chains</h4>
					<div class="memory-state-table-wrap">
						<table class="memory-state-table">
							<thead>
								<tr>
									<th>Root</th>
									<th>Chain</th>
									<th>Reason</th>
								</tr>
							</thead>
							<tbody>
								{#each temperatureState.supersession_chains.slice(0, 8) as chain}
									<tr>
										<td title={chain.root_memory_candidate_key}>{shortKey(chain.root_memory_candidate_key)}</td>
										<td title={chain.chain.map((node) => node.memory_candidate_key).join(' -> ')}>
											{chain.chain.map((node) => shortKey(node.memory_candidate_key)).join(' -> ')}
										</td>
										<td title={chain.chain[0]?.supersession_reason || ''}>
											{chain.chain[0]?.supersession_reason || '-'}
										</td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
				</div>
			{/if}
			{#if temperatureState.inactive_projections.length > 0}
				<div class="memory-temperature-subpanel">
					<h4>Projection lifecycle</h4>
					<div class="memory-state-table-wrap">
						<table class="memory-state-table">
							<thead>
								<tr>
									<th>Source</th>
									<th>Lane</th>
									<th>Tier</th>
									<th>Source item</th>
									<th>Policy</th>
									<th>Reason</th>
									<th>Deactivated</th>
									<th>Last check</th>
								</tr>
							</thead>
							<tbody>
								{#each temperatureState.inactive_projections.slice(0, 12) as projection}
									<tr>
										<td title={projection.source_memory_candidate_key}>{shortKey(projection.source_memory_candidate_key)}</td>
										<td>{projection.lane}</td>
										<td>{projection.temperature_tier}</td>
										<td title={projectionSourceLabel(projection)}>{shortKey(projectionSourceLabel(projection))}</td>
										<td>v{projection.projection_policy_version}</td>
										<td>{projection.deactivation_reason || '-'}</td>
										<td>{formatTimestamp(projection.deactivated_at_ms)}</td>
										<td>{formatTimestamp(projection.last_lifecycle_check_at_ms)}</td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
				</div>
			{/if}
		{:else}
			<p class="memory-run-status">No temperature state loaded yet.</p>
		{/if}
	</section>

	{#if visibleAnalyticsLevel >= 1}
	<section class="memory-kpis">
		<MetricCard label="Injected items (24h)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: retrievals24hSql }} />
		<MetricCard label="Injected items (7d)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: injectedItems7dSql }} />
		<MetricCard label="Consolidations (7d)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: consolidation7dSql }} />
		<MetricCard label="Avg candidates (24h)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: avgCandidates24hSql }} />
		<MetricCard label="Hybrid fallbacks (24h)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: hybridFallbacks24hSql }} />
		<MetricCard label="Index rebuild failures (24h)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: indexRebuildFailures24hSql }} />
		<MetricCard label="Index row changes (7d)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: indexRowChanges7dSql }} />
		<MetricCard label="Optimize failures (7d)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: indexOptimizeFailures7dSql }} />
		<MetricCard label="Avg selected relevance (24h)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: avgSelectedRelevance24hSql }} />
		<MetricCard label="Selection lift (24h)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: selectionLift24hSql }} />
		<MetricCard label="Utility reviews (24h)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: utilityReviews24hSql }} />
		<MetricCard label="Load-bearing labels (7d)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: loadBearing7dSql }} />
		<MetricCard label="Temp promotions (7d)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: temperaturePromotions7dSql }} />
		<MetricCard label="Temp demotions (7d)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: temperatureDemotions7dSql }} />
		<MetricCard label="Hot projections (7d)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: hotProjectionUpserts7dSql }} />
		<MetricCard label="Eval pass rate (7d)" formatAs="percent" dataSource={{ kind: 'memory_events_sql', sql: evalPassRate7dSql }} />
		<MetricCard label="Eval cases (7d)" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: evalCases7dSql }} />
		<MetricCard label="Regression status" formatAs="string" dataSource={{ kind: 'memory_events_sql', sql: regressionStatusSql }} />
		<MetricCard label="Regression failures" formatAs="number" dataSource={{ kind: 'memory_events_sql', sql: regressionFailuresSql }} />
	</section>
	{:else}
		<p class="memory-run-status">Loading observability metrics...</p>
	{/if}

	{#if visibleAnalyticsLevel >= 2}
	<section class="memory-widget">
		<h3>Index maintenance</h3>
		<Table dataSource={{ kind: 'memory_events_sql', sql: indexMaintenanceSql }} />
	</section>

	<section class="memory-widget">
		<h3>Memory events over time</h3>
		<LineChart
			dataSource={{ kind: 'memory_events_sql', sql: eventTimelineSql }}
			xField="bucket_ms"
			yField="events"
			seriesField="event_kind"
		/>
	</section>

	<section class="memory-pair">
		<div class="memory-widget">
			<h3>Top injected tiers</h3>
			<BarChart
				horizontal={true}
				dataSource={{ kind: 'memory_events_sql', sql: topInjectedTiersSql }}
				xField="tier_name"
				yField="injected"
			/>
		</div>
		<div class="memory-widget">
			<h3>Dropped candidates by scope</h3>
			<BarChart
				horizontal={true}
				dataSource={{ kind: 'memory_events_sql', sql: droppedByScopeSql }}
				xField="scope"
				yField="dropped"
			/>
		</div>
	</section>
	{/if}

	{#if visibleAnalyticsLevel >= 3}
	<section class="memory-widget">
		<h3>Search relevance over time</h3>
		<LineChart
			dataSource={{ kind: 'memory_events_sql', sql: relevanceTimelineSql }}
			xField="bucket_ms"
			yField="avg_score"
			seriesField="selection"
		/>
	</section>

	<section class="memory-pair">
		<div class="memory-widget">
			<h3>Relevance score distribution</h3>
			<BarChart
				horizontal={true}
				dataSource={{ kind: 'memory_events_sql', sql: relevanceDistributionSql }}
				xField="bucket_selection"
				yField="candidates"
			/>
		</div>
		<div class="memory-widget">
			<h3>Relevance by backend</h3>
			<Table dataSource={{ kind: 'memory_events_sql', sql: relevanceByBackendSql }} />
		</div>
	</section>
	{/if}

	{#if visibleAnalyticsLevel >= 4}
	<section class="memory-widget">
		<h3>Relevance by tier</h3>
		<Table dataSource={{ kind: 'memory_events_sql', sql: relevanceByTierSql }} />
	</section>

	<section class="memory-widget">
		<h3>Low-score selected memory</h3>
		<Table
			dataSource={{ kind: 'memory_events_sql', sql: lowScoreSelectedSql }}
			stackedFields={[
				{ key: 'item_key', label: 'Item key' },
				{ key: 'query_excerpt', label: 'Query excerpt' }
			]}
		/>
	</section>

	<section class="memory-widget">
		<h3>Temperature selection by lane</h3>
		<Table dataSource={{ kind: 'memory_events_sql', sql: relevanceByTemperatureSql }} />
	</section>

	<section class="memory-widget">
		<h3>Cold-memory retrieval wins</h3>
		<Table dataSource={{ kind: 'memory_events_sql', sql: coldMemoryWinsSql }} />
	</section>
	{/if}

	{#if visibleAnalyticsLevel >= 5}
	<section class="memory-pair">
		<div class="memory-widget">
			<h3>Utility review labels</h3>
			<BarChart
				horizontal={true}
				dataSource={{ kind: 'memory_events_sql', sql: utilityJudgementByLabelSql }}
				xField="label"
				yField="judgements"
			/>
		</div>
		<div class="memory-widget">
			<h3>Utility labels by lane</h3>
			<Table dataSource={{ kind: 'memory_events_sql', sql: utilityJudgementByLaneSql }} />
		</div>
	</section>

	<section class="memory-widget">
		<h3>Latest utility reviews</h3>
		<Table dataSource={{ kind: 'memory_events_sql', sql: latestUtilityReviewsSql }} />
	</section>

	<section class="memory-widget">
		<h3>Latest hot projections</h3>
		<Table dataSource={{ kind: 'memory_events_sql', sql: latestHotProjectionsSql }} />
	</section>

	<section class="memory-pair">
		<div class="memory-widget">
			<h3>Consolidation output by rule</h3>
			<BarChart
				horizontal={true}
				dataSource={{ kind: 'memory_events_sql', sql: consolidationByRuleSql }}
				xField="rule_name"
				yField="emitted"
			/>
		</div>
		<div class="memory-widget">
			<h3>Retrieval quality by agent/scope</h3>
			<Table dataSource={{ kind: 'memory_events_sql', sql: retrievalQualitySql }} />
		</div>
	</section>

	<section class="memory-widget">
		<h3>Recent consolidations</h3>
		<Table dataSource={{ kind: 'memory_events_sql', sql: recentConsolidationsSql }} />
	</section>

	<section class="memory-widget">
		<h3>Consolidation quality over time</h3>
		<LineChart
			dataSource={{ kind: 'memory_events_sql', sql: consolidationTrendSql }}
			xField="bucket_ms"
			yField="transforms"
			seriesField="status"
		/>
	</section>
	{/if}

	{#if visibleAnalyticsLevel >= 6}
	<section class="memory-pair">
		<div class="memory-widget">
			<h3>Consolidation status breakdown</h3>
			<BarChart
				horizontal={true}
				dataSource={{ kind: 'memory_events_sql', sql: consolidationStatusSql }}
				xField="status"
				yField="transforms"
			/>
		</div>
		<div class="memory-widget">
			<h3>Skipped outputs by rule</h3>
			<BarChart
				horizontal={true}
				dataSource={{ kind: 'memory_events_sql', sql: consolidationSkippedByRuleSql }}
				xField="rule_name"
				yField="skipped"
			/>
		</div>
	</section>

	<section class="memory-pair">
		<div class="memory-widget">
			<h3>Consolidation target quality</h3>
			<Table dataSource={{ kind: 'memory_events_sql', sql: consolidationTargetQualitySql }} />
		</div>
		<div class="memory-widget">
			<h3>Consolidation sources</h3>
			<Table dataSource={{ kind: 'memory_events_sql', sql: consolidationSourceKindSql }} />
		</div>
	</section>

	<section class="memory-widget">
		<h3>Recent consolidation issues</h3>
		<Table dataSource={{ kind: 'memory_events_sql', sql: recentConsolidationIssuesSql }} />
	</section>

	<section class="memory-widget">
		<h3>Eval results by suite</h3>
		<Table dataSource={{ kind: 'memory_events_sql', sql: evalBySuiteSql }} />
	</section>

	<section class="memory-widget">
		<h3>Recent eval failures</h3>
		<Table dataSource={{ kind: 'memory_events_sql', sql: recentEvalFailuresSql }} />
	</section>

	<section class="memory-pair">
		<div class="memory-widget">
			<h3>Eval backend comparison</h3>
			<Table dataSource={{ kind: 'memory_events_sql', sql: evalByBackendSql }} />
		</div>
		<div class="memory-widget">
			<h3>Eval backend by suite</h3>
			<Table dataSource={{ kind: 'memory_events_sql', sql: evalBackendBySuiteSql }} />
		</div>
	</section>

	<section class="memory-widget">
		<h3>Eval cases over time</h3>
		<LineChart
			dataSource={{ kind: 'memory_events_sql', sql: evalTimelineSql }}
			xField="bucket_ms"
			yField="cases"
			seriesField="result"
		/>
	</section>

	<section class="memory-pair">
		<div class="memory-widget">
			<h3>Eval pass rate by agent</h3>
			<Table dataSource={{ kind: 'memory_events_sql', sql: evalPassRateByAgentSql }} />
		</div>
		<div class="memory-widget">
			<h3>Eval status breakdown</h3>
			<BarChart
				horizontal={true}
				dataSource={{ kind: 'memory_events_sql', sql: evalStatusBreakdownSql }}
				xField="status"
				yField="cases"
			/>
		</div>
	</section>

	<section class="memory-pair">
		<div class="memory-widget">
			<h3>Worst eval cases</h3>
			<Table dataSource={{ kind: 'memory_events_sql', sql: evalWorstCasesSql }} />
		</div>
		<div class="memory-widget">
			<h3>Best-rank distribution</h3>
			<BarChart
				horizontal={true}
				dataSource={{ kind: 'memory_events_sql', sql: evalRankDistributionSql }}
				xField="rank_bucket"
				yField="cases"
			/>
		</div>
	</section>

	<section class="memory-widget">
		<h3>Latest eval runs</h3>
		<Table dataSource={{ kind: 'memory_events_sql', sql: latestEvalRunsSql }} />
	</section>

	<section class="memory-widget">
		<h3>Eval failure drilldown</h3>
		<Table dataSource={{ kind: 'memory_events_sql', sql: evalFailureDrilldownSql }} />
	</section>

	<section class="memory-widget">
		<h3>Memory regression status history</h3>
		<Table dataSource={{ kind: 'memory_events_sql', sql: regressionStatusHistorySql }} />
	</section>
	{/if}

	{#if visibleAnalyticsLevel < 6}
		<p class="memory-run-status">Loading additional observability panels...</p>
	{/if}
</section>

<style>
	.memory-observability {
		display: flex;
		flex-direction: column;
		gap: 18px;
		/* Inherit the parent page's content width (1320px on .presto-gaui-page,
		   1280px on .skills-page) instead of pinning to 1180px which would
		   render narrower than every other section on the surrounding page. */
		width: 100%;
		margin: 0 auto 24px;
		padding: 8px 0 24px;
		background: transparent;
		color: var(--theme-color-foreground, var(--color-text, #111827));
		box-sizing: border-box;
	}

	.memory-observability-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 16px;
	}

	.memory-observability-actions {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 8px;
		flex-wrap: wrap;
	}

	h2,
	h3,
	h4,
	p {
		margin: 0;
	}

	h2 {
		font-size: 1.15rem;
		font-weight: 650;
		letter-spacing: 0;
	}

	h3 {
		font-size: 0.9rem;
		font-weight: 650;
		letter-spacing: 0;
		color: var(--theme-color-foreground-muted, #64748b);
	}

	h4 {
		font-size: 0.82rem;
		font-weight: 650;
		letter-spacing: 0;
		color: var(--theme-color-foreground, #111827);
	}

	p {
		margin-top: 4px;
		color: var(--theme-color-foreground-muted, #64748b);
		font-size: 0.9rem;
	}

	button {
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.14));
		background: color-mix(in srgb, var(--theme-color-surface, #fff) 92%, transparent);
		color: inherit;
		padding: 8px 12px;
		border-radius: 8px;
		font: inherit;
		cursor: pointer;
		box-shadow: 0 1px 2px rgba(15, 23, 42, 0.04);
	}

	button:disabled {
		cursor: wait;
		opacity: 0.62;
	}

	.memory-run-status {
		margin: -6px 0 0;
		color: var(--theme-color-foreground-muted, #64748b);
		font-size: 0.86rem;
	}

	.memory-run-status-error {
		color: var(--theme-color-danger, #b91c1c);
	}

	.memory-kpis,
	.memory-pair {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
		gap: 14px;
	}

	.memory-pair {
		grid-template-columns: repeat(auto-fit, minmax(360px, 1fr));
	}

	.memory-widget {
		display: flex;
		flex-direction: column;
		gap: 12px;
		padding: 16px;
		background: color-mix(in srgb, var(--theme-color-surface, #fff) 94%, transparent);
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.08));
		border-radius: 8px;
		box-shadow: 0 1px 2px rgba(15, 23, 42, 0.04);
		min-width: 0;
	}

	.memory-temperature-state-head,
	.memory-temperature-summary,
	.memory-temperature-grid,
	.memory-temperature-tables {
		display: grid;
		gap: 12px;
	}

	.memory-temperature-state-head {
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: start;
	}

	.memory-temperature-state-meta {
		display: flex;
		flex-direction: column;
		gap: 4px;
		align-items: flex-end;
		color: var(--theme-color-foreground-muted, #64748b);
		font-size: 0.78rem;
		white-space: nowrap;
	}

	.memory-temperature-summary {
		grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
	}

	.memory-temperature-summary > div,
	.memory-temperature-subpanel {
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.08));
		border-radius: 8px;
		background: color-mix(in srgb, var(--theme-color-surface-raised, #fff) 88%, transparent);
	}

	.memory-temperature-summary > div {
		padding: 12px;
	}

	.memory-temperature-summary strong {
		display: block;
		font-size: 1.05rem;
		line-height: 1.2;
	}

	.memory-temperature-summary span {
		color: var(--theme-color-foreground-muted, #64748b);
		font-size: 0.78rem;
	}

	.memory-temperature-grid,
	.memory-temperature-tables {
		grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
	}

	.memory-temperature-subpanel {
		min-width: 0;
		padding: 12px;
	}

	.memory-temperature-subpanel-full {
		grid-column: 1 / -1;
	}

	.memory-pill-list {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
		margin-top: 10px;
	}

	.memory-pill {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		padding: 5px 8px;
		border-radius: 999px;
		border: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.1));
		background: color-mix(in srgb, var(--theme-color-surface, #fff) 86%, transparent);
		font-size: 0.76rem;
		color: var(--theme-color-foreground-muted, #64748b);
	}

	.memory-pill b {
		color: var(--theme-color-foreground, #111827);
		font-weight: 650;
	}

	.memory-state-table-wrap {
		overflow: auto;
		margin-top: 10px;
	}

	.memory-state-table {
		width: 100%;
		min-width: 620px;
		border-collapse: collapse;
		font-size: 0.78rem;
	}

	.memory-state-table th,
	.memory-state-table td {
		border-bottom: 1px solid var(--theme-color-border, rgba(15, 23, 42, 0.08));
		padding: 7px 8px;
		text-align: left;
		vertical-align: top;
	}

	.memory-state-table th {
		color: var(--theme-color-foreground-muted, #64748b);
		font-weight: 650;
	}

	.memory-state-table td {
		color: var(--theme-color-foreground, #111827);
	}

	@media (max-width: 720px) {
		.memory-observability {
			width: calc(100vw - 16px);
			padding: 16px;
		}

		.memory-observability-header {
			flex-direction: column;
		}

		.memory-pair {
			grid-template-columns: 1fr;
		}

		.memory-temperature-state-head {
			grid-template-columns: 1fr;
		}

		.memory-temperature-state-meta {
			align-items: flex-start;
			white-space: normal;
		}
	}
</style>
