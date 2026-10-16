<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { modelCost } from '$lib/llm/decisionModels';
	import {
		createLiveRewirer,
		setupLiveDataSource
	} from '$lib/magician/dashboard/useLiveDataSource';
	import {
		buildLlmUsageOverviewSql,
		llmUsageWindowWhere,
		parseLlmUsageOverview,
		type LlmUsageOverviewData
	} from './overview';

	export let where = llmUsageWindowWhere(7);
	export let rangeLabel = '7d';
	export let compact = false;
	export let className = '';
	export let ariaLabel = 'LLM usage overview';

	let mounted = false;
	let loading = false;
	let error: string | null = null;
	let data: LlmUsageOverviewData | null = null;

	$: sql = buildLlmUsageOverviewSql(where);
	$: source = { kind: 'llm_calls_sql' as const, sql };

	const liveRewirer = createLiveRewirer((dataSource) =>
		setupLiveDataSource({
			dataSource,
			onStart: () => {
				loading = true;
			},
			onRows: ({ records }) => {
				data = parseLlmUsageOverview(records[0]);
				error = null;
			},
			onError: (nextError) => {
				error = nextError.message;
			},
			onSettled: () => {
				loading = false;
			}
		})
	);

	onMount(() => {
		mounted = true;
	});

	$: if (mounted) liveRewirer.sync(source);

	onDestroy(() => {
		liveRewirer.destroy();
	});

	function currency(value: number | null | undefined): string {
		if (value == null) return unavailableValue();
		if (value > 0 && value < 0.01) return modelCost(value);
		return new Intl.NumberFormat(undefined, {
			style: 'currency',
			currency: 'USD',
			maximumFractionDigits: 2
		}).format(value);
	}

	function number(value: number | null | undefined): string {
		if (value == null) return unavailableValue();
		return new Intl.NumberFormat(undefined, { maximumFractionDigits: 3 }).format(value);
	}

	function percent(value: number | null | undefined): string {
		if (value == null) return unavailableValue();
		return new Intl.NumberFormat(undefined, {
			style: 'percent',
			maximumFractionDigits: 1
		}).format(value);
	}

	function unavailableValue(): string {
		return loading && !data ? 'Loading' : 'Unavailable';
	}
</script>

<section
	class={`llm-usage-overview ${className}`.trim()}
	class:llm-usage-overview--compact={compact}
	aria-label={ariaLabel}
	aria-busy={loading}
>
	<div class="llm-usage-overview__grid">
		<dl class="llm-usage-overview__metric">
			<dt>Spend ({rangeLabel})</dt>
			<dd>{currency(data?.spendUsd)}</dd>
			<dd class="llm-usage-overview__detail">Recorded model cost</dd>
		</dl>
		<dl class="llm-usage-overview__metric">
			<dt>Calls ({rangeLabel})</dt>
			<dd>{number(data?.calls)}</dd>
			<dd class="llm-usage-overview__detail">Recorded model requests</dd>
		</dl>
		<dl class="llm-usage-overview__metric">
			<dt>Retry rate ({rangeLabel})</dt>
			<dd>{percent(data?.retryRate)}</dd>
			<dd class="llm-usage-overview__detail">Calls after the first attempt</dd>
		</dl>
		<dl class="llm-usage-overview__metric">
			<dt>Avg latency ({rangeLabel})</dt>
			<dd>{number(data?.avgLatencyMs)}</dd>
			<dd class="llm-usage-overview__detail">Milliseconds per recorded call</dd>
		</dl>
	</div>

	{#if error}
		<p class="llm-usage-overview__status" role="status">
			{data ? 'Showing the last available LLM summary. ' : ''}{error}
		</p>
	{/if}
</section>

<style>
	.llm-usage-overview {
		min-width: 0;
		color: var(--overview-text, var(--text-primary, #1f2937));
	}

	.llm-usage-overview__grid {
		display: grid;
		grid-template-columns: repeat(4, minmax(0, 1fr));
		gap: var(--overview-gap, 1rem);
	}

	.llm-usage-overview__metric {
		min-width: 0;
		margin: 0;
		padding: var(--overview-padding, 1rem);
		border: 1px solid var(--overview-border, var(--border-soft, #d8dee9));
		border-radius: var(--overview-radius, var(--radius-md, 0.5rem));
		background: var(--overview-surface, var(--bg-card, #ffffff));
		box-shadow: var(--overview-shadow, var(--shadow-sm, none));
	}

	.llm-usage-overview__metric dt {
		color: var(--overview-muted, var(--text-muted, #667085));
		font-size: 0.75rem;
		font-weight: 700;
	}

	.llm-usage-overview__metric > dd:not(.llm-usage-overview__detail) {
		margin: 0.35rem 0 0;
		color: var(--overview-text, var(--text-primary, #1f2937));
		font-family: var(--font-mono, ui-monospace, monospace);
		font-size: 1.45rem;
		font-weight: 750;
		line-height: 1.15;
		overflow-wrap: anywhere;
	}

	.llm-usage-overview__detail {
		margin: 0.35rem 0 0;
		color: var(--overview-muted, var(--text-muted, #667085));
		font-size: 0.72rem;
		line-height: 1.35;
		overflow-wrap: anywhere;
	}

	.llm-usage-overview__status {
		margin: 0.65rem 0 0;
		color: var(--overview-critical, var(--color-error, #b91c1c));
		font-size: 0.75rem;
	}

	.llm-usage-overview--compact .llm-usage-overview__metric {
		padding: var(--overview-compact-padding, 0.8rem);
	}

	@media (max-width: 900px) {
		.llm-usage-overview__grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
	}
</style>
