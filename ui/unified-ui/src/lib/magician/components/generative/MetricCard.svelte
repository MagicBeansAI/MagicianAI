<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import {
		createLiveRewirer,
		setupLiveDataSource,
		type LiveDataSource
	} from '$lib/magician/dashboard/useLiveDataSource';
	import LiveDataRefreshButton from './LiveDataRefreshButton.svelte';
	import { parseNumericDuckDbValue, cleanDuckDbValue } from './chartUtil';

	export let value: string = '';
	export let label: string = '';
	export let trend: 'up' | 'down' | 'flat' | undefined = undefined;
	export let trendLabel: string = '';

	/**
	 * Live data binding — populates `value` from the first row's first
	 * column (or named `valueField`) of a SQL fetch. If two rows are
	 * returned, computes trend by comparing row 0 (latest) vs row 1
	 * (prior). Numbers are Intl-formatted; non-numbers cast to string.
	 */
	export let dataSource: LiveDataSource | null = null;
	export let valueField: string | null = null;
	export let formatAs: 'number' | 'currency' | 'percent' | 'string' = 'number';

	let liveError: string | null = null;
	let liveRefreshing: boolean = false;

	$: compactLiveError =
		liveError && liveError.length > 160 ? `${liveError.slice(0, 157)}...` : liveError;

	function format(v: unknown): string {
		if (v === null || v === undefined) return '—';
		
		const num = parseNumericDuckDbValue(v);
		if (num !== undefined) {
			if (formatAs === 'currency') {
				return new Intl.NumberFormat(undefined, {
					style: 'currency',
					currency: 'USD',
					maximumFractionDigits: 2
				}).format(num);
			}
			if (formatAs === 'percent') {
				return new Intl.NumberFormat(undefined, {
					style: 'percent',
					maximumFractionDigits: 1
				}).format(num);
			}
			return new Intl.NumberFormat(undefined, { maximumFractionDigits: 3 }).format(num);
		}
		
		return String(cleanDuckDbValue(v));
	}

	let liveMounted = false;

	const liveRewirer = createLiveRewirer((source) =>
		setupLiveDataSource({
			dataSource: source,
			onStart: () => {
				liveRefreshing = true;
			},
			onRows: ({ columns, rows }) => {
				const col = valueField
					? columns.indexOf(valueField)
					: 0;
				const idx = col < 0 ? 0 : col;
				const latest = rows[0]?.[idx];
				value = format(latest);
				if (rows.length >= 2) {
					const prior = rows[1]?.[idx];
					const latestNum = parseNumericDuckDbValue(latest);
					const priorNum = parseNumericDuckDbValue(prior);
					
					if (latestNum !== undefined && priorNum !== undefined && priorNum !== 0) {
						const delta = (latestNum - priorNum) / Math.abs(priorNum);
						trend = delta > 0.005 ? 'up' : delta < -0.005 ? 'down' : 'flat';
						trendLabel = `${(delta * 100).toFixed(1)}%`;
					}
				}
				liveError = null;
			},
			onError: (err) => {
				liveError = err.message;
				value = '⚠';
			},
			onSettled: () => {
				liveRefreshing = false;
			}
		})
	);

	onMount(() => {
		liveMounted = true;
	});

	// Initial wiring AND rewiring when the bound query changes: filter-driven
	// pages rebuild `dataSource.sql` reactively, and a controller frozen at
	// mount would keep re-fetching the original query on every refresh
	// event. Static dataSource props never retrigger this (key comparison
	// inside the rewirer).
	$: if (liveMounted) liveRewirer.sync(dataSource);

	onDestroy(() => {
		liveRewirer.destroy();
	});

	function refreshLiveData(): void {
		if (!liveRewirer.controller || liveRefreshing) return;
		void liveRewirer.controller.refresh();
	}

	$: safeTrend = trend === 'up' || trend === 'down' || trend === 'flat' ? trend : undefined;
	// R324: Compute semantic trend description for screen readers
	$: trendAriaLabel = safeTrend
		? (safeTrend === 'up' ? 'Trending up' : safeTrend === 'down' ? 'Trending down' : 'Flat trend') + (trendLabel ? `: ${trendLabel}` : '')
		: '';
</script>

<div class="muij-metric-card" class:muij-metric-card-live={!!dataSource} role="group" aria-label={label || 'Metric'}>
	{#if dataSource}
		<LiveDataRefreshButton
			loading={liveRefreshing}
			label={`Refetch ${label || 'metric'} data`}
			on:refresh={refreshLiveData}
		/>
	{/if}
	<!-- R678: Removed aria-live from individual metric values — in multi-metric
	     dashboards, simultaneous updates would flood screen reader announcements.
	     The parent container's role="group" provides sufficient context. -->
	<div class="muij-metric-value">{value}</div>
	{#if label}
		<div class="muij-metric-label">{label}</div>
	{/if}
	{#if compactLiveError}
		<div class="muij-metric-error" title={liveError || ''}>{compactLiveError}</div>
	{/if}
	{#if safeTrend}
		<div
			class="muij-metric-trend"
			class:muij-metric-trend-up={safeTrend === 'up'}
			class:muij-metric-trend-down={safeTrend === 'down'}
			class:muij-metric-trend-flat={safeTrend === 'flat'}
			aria-label={trendAriaLabel}
		>
			<span aria-hidden="true">{#if safeTrend === 'up'}▲{:else if safeTrend === 'down'}▼{:else}—{/if}</span>
			{#if trendLabel}
				<span class="muij-metric-trend-label">{trendLabel}</span>
			{/if}
		</div>
	{/if}
</div>

<style>
	.muij-metric-card {
		position: relative;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md);
		padding: var(--space-md);
		box-shadow: var(--shadow-sm);
	}

	.muij-metric-card-live {
		padding-right: 42px;
	}

	.muij-metric-value {
		font-family: var(--font-mono);
		font-size: 1.5rem;
		font-weight: 700;
		color: var(--text-primary);
		line-height: 1.2;
		overflow-wrap: anywhere;
	}

	.muij-metric-label {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-muted);
		margin-top: 4px;
		overflow-wrap: anywhere;
	}

	.muij-metric-error {
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		color: var(--color-error, #b91c1c);
		margin-top: 6px;
		line-height: 1.3;
		overflow-wrap: anywhere;
	}

	.muij-metric-trend {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		margin-top: 6px;
		display: flex;
		align-items: center;
		gap: 4px;
	}

	.muij-metric-trend-up {
		color: var(--color-success);
	}

	.muij-metric-trend-down {
		color: var(--color-error);
	}

	.muij-metric-trend-flat {
		color: var(--text-muted);
	}

	.muij-metric-trend-label {
		overflow-wrap: anywhere;
	}
</style>
