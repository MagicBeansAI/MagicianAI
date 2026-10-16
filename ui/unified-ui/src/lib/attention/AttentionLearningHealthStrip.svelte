<script lang="ts">
	import { browser } from '$app/environment';
	import { onDestroy } from 'svelte';

	import Icon from '$lib/shared/icons/Icon.svelte';
	import {
		fetchAttentionRankRecomputeHealth,
		type AttentionRankRecomputeHealth
	} from '$lib/attention/attentionRankRecompute';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import type {
		AttentionActionabilityPage,
		AttentionActionabilityTrainingStatus,
		ChannelFollowUpLearningHealth
	} from '$lib/channel/channelFollowUpLearning';
	import {
		groupingMayCollapse,
		groupingTotalsReconcile,
		type AttentionGroupingPage
	} from '$lib/attention/attentionGrouping';
	import type { AttentionRoutingPage } from '$lib/attention/attentionRouting';
	import type { AttentionBanditHealth } from '$lib/attention/attentionBandit';
	import {
		semanticExtractionCountsReconcile,
		semanticExtractionCoverageReconciles,
		semanticExtractionSurfacesReconcile,
		type AttentionSemanticExtractionCounts,
		type AttentionSemanticExtractionHealth
	} from '$lib/attention/attentionSemanticExtraction';

	export let health: ChannelFollowUpLearningHealth | null = null;
	export let actionability: AttentionActionabilityPage | null = null;
	export let actionabilityTraining: AttentionActionabilityTrainingStatus | null = null;
	export let grouping: AttentionGroupingPage | null = null;
	export let routing: AttentionRoutingPage | null = null;
	export let bandit: AttentionBanditHealth | null = null;
	export let semanticExtraction: AttentionSemanticExtractionHealth | null = null;
	export let surfaceLabel = 'Attention';
	export let semanticRankingEnabled: boolean | null = null;
	/** Diagnostics stay out of the way until asked for; nothing here is routine reading. */
	export let expanded = false;
	/**
	 * The health payload has not arrived yet. Without this the strip renders
	 * nothing and then appears, shifting whatever sits under it; a placeholder
	 * holds the row instead.
	 */
	export let loading = false;
	/** Shared rank-recompute queue. Tests pass it directly; the page strip loads it. */
	export let rankRecompute: AttentionRankRecomputeHealth | null = null;

	const RANK_QUEUE_REFRESH_MS = 15_000;
	let fetchedRankRecompute: AttentionRankRecomputeHealth | null = null;
	let rankFetchGeneration = 0;
	let rankRefreshTimer: ReturnType<typeof setInterval> | null = null;

	function stopRankRefresh(): void {
		if (rankRefreshTimer !== null) {
			clearInterval(rankRefreshTimer);
			rankRefreshTimer = null;
		}
	}

	async function loadRankQueue(scope: { principal: string; workspace: string }): Promise<void> {
		if (!browser || rankRecompute) return;
		const generation = ++rankFetchGeneration;
		const health = await fetchAttentionRankRecomputeHealth(scope);
		if (generation !== rankFetchGeneration || rankRecompute) return;
		fetchedRankRecompute = health;
	}

	$: shownRankRecompute = rankRecompute ?? fetchedRankRecompute;
	// Collapsed strips stay quiet. The status request starts when the reader opens
	// the diagnostics, and repeats only while that disclosure stays open.
	$: if (browser && expanded && !rankRecompute) {
		stopRankRefresh();
		const scope = {
			principal: $scopeIdentityStore.principal,
			workspace: $scopeIdentityStore.workspace
		};
		void loadRankQueue(scope);
		rankRefreshTimer = setInterval(() => void loadRankQueue(scope), RANK_QUEUE_REFRESH_MS);
	} else {
		stopRankRefresh();
	}

	onDestroy(() => {
		rankFetchGeneration += 1;
		stopRankRefresh();
	});

	$: sourceFamilies = health
		? Object.entries(health.source_family_counts).sort((left, right) => right[1] - left[1])
		: [];
	$: embeddingPercent = health ? Math.round(health.embedding_coverage * 100) : 0;
	$: semanticExtractionPercent = semanticExtraction
		? Math.round(semanticExtraction.totals.coverage * 100)
		: actionability
			? Math.round(actionability.semantic_extraction_coverage * 100)
			: 0;
	$: pairBudgetExceeded = grouping?.health.budget_exceeded === true;
	$: pairBudgetDiagnostic =
		pairBudgetExceeded &&
		typeof grouping?.health.required_pair_evaluations === 'number' &&
		typeof grouping.health.pair_evaluation_budget === 'number'
			? {
					required: grouping.health.required_pair_evaluations,
					budget: grouping.health.pair_evaluation_budget
				}
			: null;
	$: actionabilityModeLabel =
		actionability?.mode === 'enforced'
			? 'Actionability ordering active'
			: actionability?.mode === 'shadow'
				? 'Actionability preview'
				: null;
	$: groupingModeLabel = pairBudgetExceeded
		? 'Grouping inactive · pair budget exceeded'
		: grouping?.mode === 'enforced' && groupingMayCollapse(grouping)
			? 'Grouping active'
			: grouping?.mode === 'shadow'
				? 'Grouping preview'
				: grouping?.mode === 'enforced'
					? 'Grouping fallback'
					: null;
	$: groupingReconciles = groupingTotalsReconcile(grouping);
	$: decisionCoveragePercent = routing
		? Math.round(routing.health.decision_item_coverage * 100)
		: 0;
	$: impressionCoveragePercent = routing?.health.verified_impression_coverage === undefined
		? null
		: Math.round(routing.health.verified_impression_coverage * 100);
	$: routingModeLabel =
		routing?.decision.routing_mode === 'shadow'
			? 'Lane learning preview'
			: routing?.decision.routing_mode === 'canary' && routing.health.applied_route_count > 0
				? 'Lane learning active'
				: routing?.decision.routing_mode === 'canary'
					? 'Lane learning · baseline retained'
					: routing
						? 'Lane learning on'
						: null;
	$: propensityCoveragePercent = bandit ? Math.round(bandit.propensity_coverage * 100) : 0;
	$: explorationRatePercent = bandit ? Math.round(bandit.exploration_rate * 100) : 0;
	$: banditModeLabel =
		!bandit?.policy_snapshot_id
			? null
			: bandit.mode === 'shadow'
				? 'Personal ranking preview'
				: bandit.mode === 'canary' && bandit.support_ok && !bandit.degradation_reason
					? 'Personal ranking on'
					: bandit.mode === 'canary'
						? 'Personal ranking degraded'
						: null;
	$: semanticExtractionDisabled =
		semanticExtraction !== null &&
		(!semanticExtraction.enabled || semanticExtraction.pause_reason === 'disabled');
	$: semanticExtractionRecovering =
		(semanticExtraction?.queue.expired_in_flight ?? 0) > 0;
	$: semanticExtractionModeLabel = semanticExtractionDisabled
		? 'Semantic backfill disabled'
		: semanticExtraction?.paused
			? 'Semantic backfill paused'
			: semanticExtractionRecovering
				? 'Semantic backfill recovering'
			: semanticExtraction?.degradation_reason
				? 'Semantic extraction degraded'
				: semanticExtraction?.enabled
					? 'Semantic backfill active'
					: semanticExtraction
						? 'Semantic backfill disabled'
						: null;
	$: semanticExtractionReconciles = semanticExtraction
		? semanticExtractionCountsReconcile(semanticExtraction.totals) &&
			semanticExtractionCountsReconcile(semanticExtraction.surfaces.follow_up) &&
			semanticExtractionCountsReconcile(semanticExtraction.surfaces.worth_a_look) &&
			semanticExtractionCoverageReconciles(semanticExtraction.totals) &&
			semanticExtractionSurfacesReconcile(semanticExtraction)
		: false;

	function semanticStateSummary(counts: AttentionSemanticExtractionCounts): string {
		return `Succeeded ${counts.succeeded.toLocaleString()} · missing ${counts.missing.toLocaleString()} · invalid ${counts.invalid.toLocaleString()} · pending ${counts.pending.toLocaleString()} · in-flight ${counts.in_flight.toLocaleString()} · retry ${counts.retry.toLocaleString()} · dead ${counts.dead.toLocaleString()}${counts.not_applicable ? ` · not applicable ${counts.not_applicable.toLocaleString()}` : ''}`;
	}

	function familyLabel(value: string): string {
		return value.replace(/_/g, ' ').replace(/\b\w/g, (letter) => letter.toUpperCase());
	}

	// Each stage below feeds the next, so the list is ordered by dependency
	// rather than importance: a stage that is off or blocked explains why every
	// stage under it cannot advance.
	type AttentionPipelineState = 'ok' | 'warn' | 'blocked' | 'off';

	interface AttentionPipelineStage {
		key: string;
		label: string;
		state: AttentionPipelineState;
		detail: string;
	}

	const PIPELINE_STATE_LABEL: Record<AttentionPipelineState, string> = {
		ok: 'Working',
		warn: 'Degraded',
		blocked: 'Blocked',
		off: 'Not enabled'
	};

	function stageFromMode(
		key: string,
		label: string,
		mode: string | null | undefined,
		offDetail: string,
		enforcedDetail: string
	): AttentionPipelineStage {
		if (mode === 'enforced') return { key, label, state: 'ok', detail: enforcedDetail };
		if (mode === 'canary')
			return { key, label, state: 'warn', detail: 'Applied to a canary fraction only' };
		if (mode === 'shadow')
			return { key, label, state: 'warn', detail: 'Preview only — scored but not applied' };
		return { key, label, state: 'off', detail: offDetail };
	}

	$: pipeline = ((): AttentionPipelineStage[] => {
		const stages: AttentionPipelineStage[] = [];

		stages.push({
			key: 'ranking',
			label: 'Ranked order',
			state: semanticRankingEnabled ? 'ok' : 'warn',
			detail: semanticRankingEnabled
				? 'Learned order is applied to this lane'
				: semanticRankingEnabled === false
					? 'Learned order is not applied to this lane'
					: 'Waiting for a ranked projection'
		});

		if (health) {
			stages.push({
				key: 'embeddings',
				label: 'Embeddings',
				state: health.embedding_coverage >= 0.9 ? 'ok' : 'warn',
				detail: `${embeddingPercent}% of candidates embedded`
			});
		}

		if (semanticExtraction) {
			const dead = semanticExtraction.totals.dead;
			const invalid = semanticExtraction.totals.invalid;
			const missing = semanticExtraction.totals.missing;
			stages.push({
				key: 'semantic',
				label: 'Semantic features',
				state: semanticExtractionDisabled
					? 'off'
					: semanticExtraction.paused || semanticExtraction.degradation_reason || dead > 0 || invalid > 0 || missing > 0
						? 'warn'
						: 'ok',
				detail: [
					`${semanticExtractionPercent}% covered`,
					semanticExtraction.totals.not_applicable ? `${semanticExtraction.totals.not_applicable.toLocaleString()} not applicable` : null,
					dead > 0 ? `${dead.toLocaleString()} active dead-letter rows` : null,
					invalid > 0 ? `${invalid.toLocaleString()} invalid` : null,
					missing > 0 ? `${missing.toLocaleString()} missing source features` : null
				].filter(Boolean).join(' · ')
			});
		}

		if (routing) {
			// Scope-level, not the per-decision coverage: a fresh decision is
			// recorded on nearly every projection, so its coverage is ~0 by
			// construction and would report a working signal as blocked.
			const eligible = routing.health.impression_eligible_count;
			const recorded = routing.health.verified_impression_total ?? 0;
			stages.push({
				key: 'impressions',
				label: 'Verified impressions',
				state: recorded > 0 ? 'ok' : eligible > 0 ? 'blocked' : 'off',
				detail:
					recorded > 0
						? `${recorded.toLocaleString()} recorded · ${eligible.toLocaleString()} eligible in the current decision`
						: `None recorded from ${eligible.toLocaleString()} eligible — no reward signal reaches the model`
			});
		}

		if (actionabilityTraining?.last_status === 'written') {
			stages.push({
				key: 'actionability-training',
				label: 'Actionability trainer',
				state: 'ok',
				detail: `Wrote ${actionabilityTraining.last_snapshot_id ?? 'a snapshot'}`
			});
		}

		const appliedRoutes = routing?.health.applied_route_count ?? 0;
		const learnedRoutes = routing?.health.learned_route_count ?? 0;
		stages.push({
			key: 'lanes',
			label: 'Lane learning',
			state: appliedRoutes > 0 ? 'ok' : learnedRoutes > 0 ? 'warn' : 'ok',
			detail: appliedRoutes > 0
				? 'Similar future cards move between For you and Worth a look'
				: learnedRoutes > 0
					? 'Lane suggestions recorded — not yet moving cards'
					: 'Shouldn’t-have-been-flagged and This-needs-me teach future lanes'
		});

		if (actionability && actionability.mode !== 'disabled')
			stages.push(
				stageFromMode(
					'actionability',
					'Actionability model',
					actionability.mode,
					'No reviewed snapshot installed',
					'Ordering by predicted actionability'
				)
			);

		if (pairBudgetExceeded || (grouping && grouping.mode !== 'disabled'))
			stages.push(
				stageFromMode(
					'grouping',
					'Duplicate grouping',
					pairBudgetExceeded ? null : grouping?.mode,
					pairBudgetExceeded
						? 'Inactive — pair evaluation budget exceeded'
						: 'No reviewed snapshot installed',
					'Collapsing duplicates across lanes'
				)
			);

		if (bandit?.policy_snapshot_id) {
			let banditState: AttentionPipelineState = 'ok';
			let banditDetail = `${bandit.posterior_update_count.toLocaleString()} posterior updates`;
			if (bandit.degradation_reason) {
				banditState = 'warn';
				banditDetail = bandit.degradation_reason.replace(/_/g, ' ');
			} else if (!bandit.support_ok) {
				banditState = 'warn';
				banditDetail = 'Insufficient support for the served slate';
			}
			stages.push({
				key: 'bandit',
				label: 'Personal bandit',
				state: banditState,
				detail: banditDetail
			});
		}

		if (shownRankRecompute) {
			const queue = shownRankRecompute.queue;
			const waiting = queue.retry > 0 || queue.dead > 0;
			stages.push({
				key: 'rank-recompute',
				label: 'Rank recompute',
				state: shownRankRecompute.enabled ? (waiting ? 'warn' : 'ok') : 'off',
				detail: shownRankRecompute.enabled
					? waiting
						? `${queue.retry.toLocaleString()} waiting to retry · ${queue.dead.toLocaleString()} stopped`
						: queue.pending + queue.in_flight > 0
							? `${queue.pending.toLocaleString()} pending · ${queue.in_flight.toLocaleString()} active`
							: 'Queue idle'
					: 'Disabled by configuration'
			});
		}

		return stages;
	})();

	$: hasHealthData = Boolean(
		health || actionability || actionabilityTraining || grouping || routing || bandit || semanticExtraction
	);
	$: pipelineBlocked = pipeline.filter((stage) => stage.state === 'blocked');
	// Collapsing hides the per-stage verdicts, so anything that is actually
	// broken has to survive into the header. "Not enabled" is deliberately not
	// counted: a dormant stage is by design, and reporting it as a problem is
	// what makes a health indicator get ignored.
	$: attentionNeeded = pipeline.filter(
		(stage) => stage.state === 'blocked' || stage.state === 'warn'
	);
	$: attentionSummary =
		attentionNeeded.length > 0
			? `${attentionNeeded.length} need${attentionNeeded.length === 1 ? 's' : ''} attention`
			: '';
</script>

{#if hasHealthData}
	<section class="attention-learning-health" aria-label={`${surfaceLabel} learning health`}>
		<button
			type="button"
			class="attention-learning-health__intro"
			aria-expanded={expanded}
			on:click={() => (expanded = !expanded)}
		>
			<Icon name={expanded ? 'chevron-down' : 'chevron-right'} size={12} />
			<strong>System health</strong>
			{#if !expanded && attentionSummary}
				<!-- The chips below are mode labels, not verdicts; without this a
				     blocked stage would be invisible while collapsed. -->
				<span class="attention-learning-health__mode--warning">{attentionSummary}</span>
			{/if}
			<span class:attention-learning-health__mode--active={semanticRankingEnabled}>
				{semanticRankingEnabled ? 'Learned order active' : semanticRankingEnabled === false ? 'Learned order not applied' : 'Waiting for ranking status'}
			</span>
			{#if actionabilityModeLabel}
				<span
					class:attention-learning-health__mode--active={actionability?.mode === 'enforced'}
					class:attention-learning-health__mode--preview={actionability?.mode === 'shadow'}
				>
					{actionabilityModeLabel}
				</span>
			{/if}
			{#if groupingModeLabel}
				<span
					class:attention-learning-health__mode--active={groupingMayCollapse(grouping)}
					class:attention-learning-health__mode--preview={grouping?.mode === 'shadow'}
					class:attention-learning-health__mode--warning={pairBudgetExceeded ||
						(grouping?.mode === 'enforced' && !groupingMayCollapse(grouping))}
				>
					{groupingModeLabel}
				</span>
			{/if}
			{#if routingModeLabel}
				<span
					class:attention-learning-health__mode--active={routing?.decision.routing_mode ===
						'canary' && routing.health.applied_route_count > 0}
					class:attention-learning-health__mode--preview={routing?.decision.routing_mode ===
						'shadow'}
					class:attention-learning-health__mode--warning={routing?.decision.degradation_reason !==
						null}
				>
					{routingModeLabel}
				</span>
			{/if}
			{#if banditModeLabel}
				<span
					class:attention-learning-health__mode--preview={bandit?.mode === 'shadow' ||
						(bandit?.mode === 'canary' && bandit.support_ok)}
					class:attention-learning-health__mode--warning={!bandit?.support_ok ||
						bandit?.degradation_reason !== null}
				>
					{banditModeLabel}
				</span>
			{/if}
			{#if semanticExtractionModeLabel}
				<span
					class:attention-learning-health__mode--active={semanticExtraction?.enabled &&
						!semanticExtraction.paused && !semanticExtraction.degradation_reason &&
						!semanticExtractionRecovering}
					class:attention-learning-health__mode--warning={!semanticExtractionDisabled &&
						(semanticExtraction?.paused || semanticExtraction?.degradation_reason !== null ||
							semanticExtractionRecovering)}
				>
					{semanticExtractionModeLabel}
				</span>
			{/if}
		</button>
		{#if expanded}
		{#if pipeline.length > 0}
			<ul class="attention-learning-health__pipeline" aria-label={`${surfaceLabel} learning pipeline`}>
				{#each pipeline as stage (stage.key)}
					<li class={`attention-learning-health__stage attention-learning-health__stage--${stage.state}`}>
						<span class="attention-learning-health__stage-name">{stage.label}</span>
						<span class="attention-learning-health__stage-state">
							{PIPELINE_STATE_LABEL[stage.state]}
						</span>
						<span class="attention-learning-health__stage-detail">{stage.detail}</span>
					</li>
				{/each}
			</ul>
			{#if pipelineBlocked.length > 0}
				<p class="attention-learning-health__blocker">
					{pipelineBlocked.map((stage) => stage.label).join(', ')} blocked — stages below cannot
					advance until this is resolved.
				</p>
			{/if}
		{/if}
		<dl class="attention-learning-health__metrics">
			{#if health}
				<div>
					<dt>Active candidates</dt>
					<dd>{health.total_active.toLocaleString()}</dd>
				</div>
				<div>
					<dt>Embedding coverage</dt>
					<dd>{health.embedded_candidates.toLocaleString()} · {embeddingPercent}%</dd>
				</div>
				<div>
					<dt>Rank changes</dt>
					<dd>{health.learned_rank_changes.toLocaleString()}</dd>
				</div>
			{/if}
			{#if grouping}
				<div>
					<dt>All candidates / members</dt>
					<dd>
						{grouping.health.candidate_total.toLocaleString()} / {grouping.health.member_total.toLocaleString()}
					</dd>
				</div>
				<div>
					<dt>Clusters / representatives</dt>
					<dd>
						{grouping.health.cluster_total.toLocaleString()} / {grouping.health.representative_total.toLocaleString()}
					</dd>
				</div>
				<div>
					<dt>{groupingMayCollapse(grouping) ? 'Related updates represented' : 'Potential related updates'}</dt>
					<dd>{grouping.health.collapsed_member_total.toLocaleString()}</dd>
				</div>
				<div>
					<dt>Pair evidence / cannot-links</dt>
					<dd>
						{grouping.health.scored_pair_total.toLocaleString()} / {grouping.health.cannot_link_total.toLocaleString()}
					</dd>
				</div>
				{#if pairBudgetDiagnostic}
					<div>
						<dt>Pair evaluations required / budget</dt>
						<dd>
							{pairBudgetDiagnostic.required.toLocaleString()} / {pairBudgetDiagnostic.budget.toLocaleString()}
						</dd>
					</div>
				{/if}
				<div>
					<dt>Ungrouped fallback</dt>
					<dd>{grouping.health.fallback_ungrouped_total.toLocaleString()}</dd>
				</div>
				<div>
					<dt>Reconciliation</dt>
					<dd class:attention-learning-health__reconcile--bad={!groupingReconciles}>
						{groupingReconciles ? 'Exact' : 'Mismatch · grouping not trusted'}
					</dd>
				</div>
			{/if}
			{#if actionability && actionability.mode !== 'disabled'}
				<div>
					<dt>Semantic extraction</dt>
					<dd>{Math.round(actionability.semantic_extraction_coverage * 100)}%</dd>
				</div>
				<div>
					<dt>Actionability scored</dt>
					<dd>{actionability.scored_count.toLocaleString()}</dd>
				</div>
				<div>
					<dt>Slice 1 fallback</dt>
					<dd>{actionability.fallback_count.toLocaleString()}</dd>
				</div>
				{#if actionability.snapshot_id}
					<div>
						<dt>Snapshot</dt>
						<dd title={actionability.snapshot_id}>{actionability.snapshot_id}</dd>
					</div>
				{/if}
			{/if}
			{#if routing}
				<div>
					<dt>Evaluated / selected / returned</dt>
					<dd>
						{routing.health.evaluated_count.toLocaleString()} / {routing.decision.selected_item_count.toLocaleString()} / {routing.decision.returned_item_count.toLocaleString()}
					</dd>
				</div>
				<div>
					<dt>Learned routes / applied</dt>
					<dd>
						{routing.health.learned_route_count.toLocaleString()} / {routing.health.applied_route_count.toLocaleString()}
					</dd>
				</div>
				<div>
					<dt>Baseline retained</dt>
					<dd>{routing.health.baseline_retained_count.toLocaleString()}</dd>
				</div>
				<div>
					<dt>Decision-item coverage</dt>
					<dd>{decisionCoveragePercent}%</dd>
				</div>
				<div>
					<dt>Impression eligible</dt>
					<dd>{routing.health.impression_eligible_count.toLocaleString()}</dd>
				</div>
				{#if impressionCoveragePercent !== null}
					<div>
						<dt>Verified impression coverage</dt>
						<dd>{impressionCoveragePercent}%</dd>
					</div>
				{/if}
				{#if routing.health.impression_dedupe_count !== undefined}
					<div>
						<dt>Impressions deduplicated</dt>
						<dd>{routing.health.impression_dedupe_count.toLocaleString()}</dd>
					</div>
				{/if}
				<div>
					<dt>Decision universe</dt>
					<dd>{routing.decision.complete_universe_recorded ? 'Complete' : 'Degraded'}</dd>
				</div>
				<div>
					<dt>Diagnostic</dt>
					<dd>
						<a class="attention-learning-health__all-candidates" href={routing.health.all_candidates_path}>
							All candidates
						</a>
					</dd>
				</div>
			{/if}
			{#if bandit}
				<div>
					<dt>Posterior version / updates</dt>
					<dd>{bandit.posterior_version.toLocaleString()} / {bandit.posterior_update_count.toLocaleString()}</dd>
				</div>
				<div>
					<dt>Propensity coverage</dt>
					<dd>{propensityCoveragePercent}%</dd>
				</div>
				<div>
					<dt>Exploration rate</dt>
					<dd>{explorationRatePercent}%</dd>
				</div>
				<div>
					<dt>Personalization scope</dt>
					<dd>{bandit.first_page_bounded ? 'First page only' : 'Unsupported scope'}</dd>
				</div>
				<div>
					<dt>Bandit support</dt>
					<dd class:attention-learning-health__reconcile--bad={!bandit.support_ok}>
						{bandit.support_ok ? 'Supported' : 'Degraded'}
					</dd>
				</div>
				{#if bandit.policy_snapshot_id}
					<div>
						<dt>Policy snapshot</dt>
						<dd title={bandit.policy_snapshot_id}>{bandit.policy_snapshot_id}</dd>
					</div>
				{/if}
				{#if bandit.degradation_reason}
					<div>
						<dt>Bandit degradation</dt>
						<dd class="attention-learning-health__reconcile--bad">
							{familyLabel(bandit.degradation_reason)}
						</dd>
					</div>
				{/if}
			{/if}
			{#if shownRankRecompute}
				<div>
					<dt>Rank queue pending / active / retry</dt>
					<dd>
						{shownRankRecompute.queue.pending.toLocaleString()} / {shownRankRecompute.queue.in_flight.toLocaleString()} / {shownRankRecompute.queue.retry.toLocaleString()}
					</dd>
				</div>
				<div>
					<dt>Rank outcomes succeeded / stale / dead</dt>
					<dd>
						{shownRankRecompute.queue.succeeded.toLocaleString()} / {shownRankRecompute.queue.stale.toLocaleString()} / {shownRankRecompute.queue.dead.toLocaleString()}
					</dd>
				</div>
				{#if !shownRankRecompute.enabled}
					<div>
						<dt>Rank worker</dt>
						<dd>Disabled by configuration</dd>
					</div>
				{/if}
			{/if}
			{#if semanticExtraction}
				<div>
					<dt>Semantic active / compatible</dt>
					<dd>
						{semanticExtraction.totals.active_total.toLocaleString()} / {semanticExtraction.totals.compatible_revision.toLocaleString()} · {Math.round(semanticExtraction.totals.coverage * 100)}%
					</dd>
				</div>
				<div class="attention-learning-health__sources">
					<dt>Follow-up extraction states</dt>
					<dd>{semanticStateSummary(semanticExtraction.surfaces.follow_up)}</dd>
				</div>
				<div class="attention-learning-health__sources">
					<dt>Worth extraction states</dt>
					<dd>{semanticStateSummary(semanticExtraction.surfaces.worth_a_look)}</dd>
				</div>
				<div>
					<dt>Backfill queue P / active / expired / R / dead</dt>
					<dd>
						{semanticExtraction.queue.pending.toLocaleString()} / {semanticExtraction.queue.active_in_flight.toLocaleString()} / {semanticExtraction.queue.expired_in_flight.toLocaleString()} / {semanticExtraction.queue.retry.toLocaleString()} / {semanticExtraction.queue.dead.toLocaleString()}
					</dd>
				</div>
				<div>
					<dt>Extraction reconciliation</dt>
					<dd class:attention-learning-health__reconcile--bad={!semanticExtractionReconciles}>
						{semanticExtractionReconciles ? 'Exact' : 'Mismatch · diagnostics not trusted'}
					</dd>
				</div>
				<div>
					<dt>Extractor contract</dt>
					<dd title={semanticExtraction.contract.extractor_contract}>
						Schema {semanticExtraction.contract.semantic_schema_version} · {semanticExtraction.contract.extractor_contract} · prompt {semanticExtraction.contract.prompt_version}
					</dd>
				</div>
				<div>
					<dt>Extractor model / profile</dt>
					<dd>
						{semanticExtraction.contract.model ?? 'Unavailable'} / {semanticExtraction.contract.profile ?? 'Unavailable'}
					</dd>
				</div>
				{#if semanticExtraction.checkpoint.cursor || semanticExtraction.checkpoint.updated_at !== null}
					<div>
						<dt>Backfill checkpoint</dt>
						<dd title={semanticExtraction.checkpoint.cursor ?? undefined}>
							{semanticExtraction.checkpoint.cursor ?? 'No cursor'}{semanticExtraction.checkpoint.updated_at === null ? '' : ` · ${semanticExtraction.checkpoint.updated_at}`}
						</dd>
					</div>
				{/if}
				{#if semanticExtraction.pause_reason}
					<div>
						<dt>Pause reason</dt>
						<dd>{familyLabel(semanticExtraction.pause_reason)}</dd>
					</div>
				{/if}
				{#if semanticExtraction.degradation_reason}
					<div>
						<dt>Extraction degradation</dt>
						<dd class="attention-learning-health__reconcile--bad">
							{familyLabel(semanticExtraction.degradation_reason)}
						</dd>
					</div>
				{/if}
			{/if}
			{#if sourceFamilies.length > 0}
				<div class="attention-learning-health__sources">
					<dt>Source families</dt>
					<dd>
						{#each sourceFamilies.slice(0, 3) as [family, count], index (family)}
							{#if index > 0}<span aria-hidden="true"> · </span>{/if}{familyLabel(family)} {count.toLocaleString()}
						{/each}
						{#if sourceFamilies.length > 3}
							<span aria-label={`${sourceFamilies.length - 3} additional source families`}> · +{sourceFamilies.length - 3}</span>
						{/if}
					</dd>
				</div>
			{/if}
		</dl>
		<p>
			Complete candidate totals remain visible; learned ordering does not remove candidates.{actionability?.mode ===
			'shadow'
				? ' Actionability is preview-only and does not change ordering.'
				: actionability?.mode === 'enforced'
					? ' Actionability ordering is active; unscored candidates retain Slice 1 fallback ordering.'
				: ''}{health?.coverage_scope
				? ` Embedding coverage scope: ${familyLabel(health.coverage_scope)}.`
				: ''}{pairBudgetDiagnostic
					? ` Pair grouping is inactive: ${pairBudgetDiagnostic.required.toLocaleString()} evaluations were required against a budget of ${pairBudgetDiagnostic.budget.toLocaleString()}; every original row remains visible as a singleton.`
					: grouping?.mode === 'shadow'
						? ' Grouping is preview-only; every current card remains separate.'
						: groupingMayCollapse(grouping)
					? ' Representatives are expandable to every member and its lifecycle.'
					: grouping?.mode === 'enforced'
						? ' Grouping reconciliation is unavailable, so Slice 2 presentation is preserved.'
						: ''}
		</p>
		{#if routing}
			<p>
				{routing.decision.routing_mode === 'shadow'
					? 'Lane changes are preview-only; baseline routes remain served.'
					: routing.decision.routing_mode === 'canary'
						? `${routing.health.applied_route_count.toLocaleString()} learned route change${routing.health.applied_route_count === 1 ? '' : 's'} server-applied; every evaluated candidate remains available through All candidates.`
						: 'Baseline routes remain served; learned alternatives are diagnostic only.'}
				Verified impressions require {routing.impression_policy.min_visible_ms.toLocaleString()} ms of continuous visibility; API return and no interaction are not impressions or negative labels.{routing.decision.degradation_reason
					? ` Routing degradation: ${routing.decision.degradation_reason.replace(/_/g, ' ')}.`
					: ''}
			</p>
		{/if}
		{#if bandit}
			<p>
				{bandit.mode === 'shadow'
					? 'Personal ranking is preview-only; the server preserves baseline order.'
					: bandit.mode === 'canary'
						? 'The server alone may sample and apply personal order on the first page; only cards marked server-applied are active.'
						: 'Personal ranking is disabled; baseline order is served.'}
				The browser never samples or reorders cards. No interaction and acknowledge remain neutral.
			</p>
		{/if}
		{#if semanticExtraction}
			<p>
				Semantic counts cover the full active scoped universe, not only this page. Coverage excludes sources marked not applicable to this extractor. Queue totals also retain historical attempts and are separate from active coverage. Extraction and backfill run server-side; the browser performs no extraction or render-time LLM work. Missing, invalid, pending, retrying, dead, or revision-incompatible candidates remain visible in Slice 1 fallback order and are never filtered by this diagnostic.
			</p>
		{/if}
		{#if shownRankRecompute}
			<p>
				Rank recompute runs on the server after feedback. Pending, active, and retry are the shared queue. Succeeded, stale, and dead totals stay in the ledger until retention. Card order changes on the next list load.
			</p>
		{/if}
		{/if}
	</section>
{:else if loading}
	<section
		class="attention-learning-health attention-learning-health--loading"
		aria-label={`${surfaceLabel} learning health`}
		aria-busy="true"
	>
		<div class="attention-learning-health__intro" role="status">
			<span class="attention-learning-health__skeleton attention-learning-health__skeleton--title"
				>&nbsp;</span
			>
			<span class="attention-learning-health__skeleton" aria-hidden="true">&nbsp;</span>
			<span class="attention-learning-health__skeleton" aria-hidden="true">&nbsp;</span>
			<span class="attention-learning-health__skeleton" aria-hidden="true">&nbsp;</span>
		</div>
	</section>
{/if}

<style>
	.attention-learning-health {
		display: grid;
		gap: 0.55rem;
		padding: 0.75rem 0.9rem;
		border: 1px solid var(--border-soft);
		border-radius: 10px;
		background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft));
		color: var(--text-secondary, var(--text-primary));
	}

	.attention-learning-health__intro,
	.attention-learning-health__metrics {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.55rem 1rem;
	}

	.attention-learning-health--loading .attention-learning-health__intro {
		cursor: default;
	}

	.attention-learning-health__skeleton {
		display: inline-block;
		width: 6.5rem;
		border-radius: 999px;
		background: var(--bg-soft);
		opacity: 0.75;
		animation: attention-health-pulse 1.4s ease-in-out infinite;
	}

	.attention-learning-health__skeleton--title { width: 7.5rem; }
	.attention-learning-health__skeleton:nth-child(3) { width: 9rem; animation-delay: 0.15s; }
	.attention-learning-health__skeleton:nth-child(4) { width: 7rem; animation-delay: 0.3s; }

	@keyframes attention-health-pulse {
		0%, 100% { opacity: 0.45; }
		50% { opacity: 0.85; }
	}

	@media (prefers-reduced-motion: reduce) {
		.attention-learning-health__skeleton { animation: none; }
	}

	/* The header is the disclosure control, so it has to shed the default button
	   chrome and stay left-aligned with the rest of the strip. */
	.attention-learning-health__intro {
		width: 100%;
		border: 0;
		padding: 0;
		background: none;
		font: inherit;
		text-align: left;
		color: inherit;
		cursor: pointer;
	}

	.attention-learning-health__intro strong {
		color: var(--text-primary);
		font-size: 0.84rem;
	}

	.attention-learning-health__intro span {
		padding: 0.18rem 0.45rem;
		border-radius: 999px;
		background: var(--bg-soft);
		font-size: 0.7rem;
		font-weight: 600;
		letter-spacing: 0.02em;
		text-transform: uppercase;
	}

	.attention-learning-health__intro .attention-learning-health__mode--active {
		background: color-mix(in srgb, var(--color-success, #2f8f5b) 12%, var(--bg-card));
		color: var(--color-success, #2f8f5b);
	}

	.attention-learning-health__intro .attention-learning-health__mode--preview {
		background: color-mix(in srgb, var(--color-info, #4d9de0) 10%, var(--bg-card));
		color: var(--color-info, #4d9de0);
	}

	.attention-learning-health__intro .attention-learning-health__mode--warning,
	.attention-learning-health .attention-learning-health__reconcile--bad {
		color: var(--color-warning, #9a6410);
	}

	.attention-learning-health__pipeline {
		list-style: none;
		margin: 0.5rem 0 0;
		padding: 0;
		display: grid;
		gap: 0.15rem;
	}

	.attention-learning-health__stage {
		display: grid;
		grid-template-columns: minmax(7rem, max-content) minmax(4.5rem, max-content) 1fr;
		align-items: baseline;
		gap: 0.5rem;
		padding: 0.2rem 0.4rem;
		border-left: 2px solid transparent;
		border-radius: 3px;
	}

	.attention-learning-health__stage:nth-child(odd) {
		background: var(--bg-soft);
	}

	.attention-learning-health__stage-name {
		color: var(--text-primary);
		font-weight: 600;
	}

	.attention-learning-health__stage-state {
		font-variant: all-small-caps;
		letter-spacing: 0.03em;
		font-weight: 700;
	}

	.attention-learning-health__stage-detail {
		color: var(--text-secondary);
		overflow-wrap: anywhere;
		min-width: 0;
	}

	.attention-learning-health__stage--ok {
		border-left-color: var(--color-success, #2f8f5b);
	}

	.attention-learning-health__stage--ok .attention-learning-health__stage-state {
		color: var(--color-success, #2f8f5b);
	}

	.attention-learning-health__stage--warn {
		border-left-color: var(--color-warning, #9a6410);
	}

	.attention-learning-health__stage--warn .attention-learning-health__stage-state {
		color: var(--color-warning, #9a6410);
	}

	.attention-learning-health__stage--blocked {
		border-left-color: var(--color-danger, #b3261e);
	}

	.attention-learning-health__stage--blocked .attention-learning-health__stage-state {
		color: var(--color-danger, #b3261e);
	}

	.attention-learning-health__stage--off {
		border-left-color: var(--border-soft);
	}

	.attention-learning-health__stage--off .attention-learning-health__stage-name,
	.attention-learning-health__stage--off .attention-learning-health__stage-state {
		color: var(--text-secondary);
	}

	.attention-learning-health__blocker {
		margin: 0.4rem 0 0;
		color: var(--color-danger, #b3261e);
	}

	@media (max-width: 40rem) {
		.attention-learning-health__stage {
			grid-template-columns: 1fr max-content;
		}

		.attention-learning-health__stage-detail {
			grid-column: 1 / -1;
		}
	}

	.attention-learning-health__metrics {
		margin: 0;
	}

	.attention-learning-health__metrics > div {
		display: flex;
		align-items: baseline;
		gap: 0.35rem;
	}

	.attention-learning-health dt {
		font-size: 0.72rem;
		color: var(--text-muted, var(--text-secondary));
	}

	.attention-learning-health dd {
		margin: 0;
		font-size: 0.78rem;
		font-weight: 650;
		font-variant-numeric: tabular-nums;
		color: var(--text-primary);
	}

	.attention-learning-health__sources {
		min-width: min(100%, 280px);
	}

	.attention-learning-health__all-candidates {
		color: var(--accent-primary);
		font-size: 0.72rem;
		font-weight: 650;
	}

	.attention-learning-health p {
		margin: 0;
		font-size: 0.72rem;
		color: var(--text-muted, var(--text-secondary));
	}
</style>
