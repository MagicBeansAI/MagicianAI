<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { sanitizeCssValue } from './cssUtil';
	import { formatCompactChartNumber, parseNumericDuckDbValue } from './chartUtil';
	import {
		createLiveRewirer,
		setupLiveDataSource,
		type LiveDataSource
	} from '$lib/magician/dashboard/useLiveDataSource';
	import LiveDataRefreshButton from './LiveDataRefreshButton.svelte';

	const DEFAULT_COLOR = 'var(--theme-color-accent, var(--theme-chart-color-0, #3b82f6))';

	interface BarDatum {
		label: string;
		value: number;
		color?: string;
	}

	export let data: BarDatum[] = [];
	export let horizontal: boolean = false;
	export let maxValue: number | undefined = undefined;

	/**
	 * Optional live data binding. When set, the chart fetches rows from
	 * the analytics SQL endpoint on mount and on every
	 * `magician:dashboard-refresh` event, mapping rows to BarDatum via
	 * `xField` (label) and `yField` (value). Falls back to the static
	 * `data` prop when `dataSource` is null/undefined.
	 */
	export let dataSource: LiveDataSource | null = null;
	export let xField: string = 'label';
	export let yField: string = 'value';

	let liveLoading: boolean = false;
	let liveError: string | null = null;
	let liveRefreshing: boolean = false;

	let liveMounted = false;

	const liveRewirer = createLiveRewirer((source) => {
		liveLoading = true;
		return setupLiveDataSource({
			dataSource: source,
			onStart: () => {
				liveLoading = true;
				liveRefreshing = true;
			},
			onRows: ({ records }) => {
				data = records.map((r) => ({
					label: String(r[xField] ?? ''),
					value: Number(r[yField] ?? 0)
				}));
				liveError = null;
			},
			onError: (err) => {
				liveError = err.message;
				liveLoading = false;
			},
			onSettled: () => {
				liveLoading = false;
				liveRefreshing = false;
			}
		});
	});

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

	interface RenderDatum {
		label: string;
		value: number;
		ratio: number;
		color: string;
	}

	function toFiniteNumber(value: unknown): number | undefined {
		return parseNumericDuckDbValue(value);
	}

	function normalizeColor(value: unknown): string | undefined {
		if (typeof value !== 'string') return undefined;
		const trimmed = sanitizeCssValue(value).trim();
		return trimmed.length > 0 ? trimmed : undefined;
	}

	$: normalized = data
		.map((entry, index) => {
			const value = toFiniteNumber(entry?.value);
			if (value === undefined) return null;
			const label = typeof entry?.label === 'string' && entry.label.trim().length > 0
				? entry.label.trim()
				: `Item ${index + 1}`;
			return {
				label,
				value: Math.max(0, value),
				color: normalizeColor(entry?.color) || DEFAULT_COLOR
			};
		})
		.filter((entry): entry is { label: string; value: number; color: string } => entry !== null);

	$: safeMaxValue = (() => {
		const explicit = toFiniteNumber(maxValue);
		if (explicit !== undefined && explicit > 0) return explicit;
		const inferred = Math.max(...normalized.map((entry) => entry.value), 0);
		return inferred > 0 ? inferred : 1;
	})();

	$: renderData = normalized.map((entry): RenderDatum => ({
		label: entry.label,
		value: entry.value,
		ratio: Math.min(1, Math.max(0, entry.value / safeMaxValue)),
		color: entry.color
	}));
</script>

<div class="muij-bar-chart" class:muij-bar-chart-live={!!dataSource}>
	{#if dataSource}
		<LiveDataRefreshButton
			loading={liveRefreshing}
			label="Refetch bar chart data"
			on:refresh={refreshLiveData}
		/>
	{/if}
	{#if liveError}
		<div class="muij-bar-error" role="alert">⚠ {liveError}</div>
	{:else if liveLoading && renderData.length === 0}
		<div class="muij-bar-empty">Loading…</div>
	{:else if renderData.length === 0}
		<div class="muij-bar-empty">No data</div>
	{:else if horizontal}
		<div class="muij-bar-rows" role="list" aria-label="Horizontal bar chart">
			{#each renderData as item, itemIndex (`${item.label}:${itemIndex}`)}
				<div class="muij-bar-row" role="listitem">
					<div class="muij-bar-label" title={item.label}>{item.label}</div>
					<div class="muij-bar-track">
						<div class="muij-bar-fill" style={`width:${item.ratio * 100}%;background:${item.color}`}></div>
					</div>
					<div class="muij-bar-value" title={item.value.toLocaleString('en-US')}>
						{formatCompactChartNumber(item.value)}
					</div>
				</div>
			{/each}
		</div>
	{:else}
		<div class="muij-bar-columns" role="list" aria-label="Bar chart">
			{#each renderData as item, itemIndex (`${item.label}:${itemIndex}`)}
				<div class="muij-bar-column" role="listitem">
					<div class="muij-bar-column-track">
						<div class="muij-bar-column-fill" style={`height:${item.ratio * 100}%;background:${item.color}`}></div>
					</div>
					<div class="muij-bar-column-label" title={item.label}>{item.label}</div>
					<div class="muij-bar-column-value" title={item.value.toLocaleString('en-US')}>
						{formatCompactChartNumber(item.value)}
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.muij-bar-chart {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		min-width: 0;
	}

	.muij-bar-chart-live {
		padding-top: 30px;
	}

	.muij-bar-error {
		font-family: var(--theme-font-mono, var(--font-primary), monospace);
		font-size: 0.85rem;
		color: var(--theme-color-accent, var(--text-secondary));
		padding: 10px 14px;
		border: 1px solid var(--theme-color-accent, var(--border-light));
		border-radius: 6px;
		background-color: var(--theme-color-surface);
	}

	.muij-bar-empty {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
		font-style: italic;
	}

	.muij-bar-rows {
		display: grid;
		gap: 8px;
	}

	.muij-bar-row {
		display: grid;
		grid-template-columns: minmax(80px, 120px) minmax(0, 1fr) minmax(42px, 64px);
		gap: 10px;
		align-items: center;
	}

	.muij-bar-label,
	.muij-bar-value,
	.muij-bar-column-label,
	.muij-bar-column-value {
		font-family: var(--font-primary);
		font-size: 0.625rem;
	}

	.muij-bar-label,
	.muij-bar-column-label {
		color: var(--text-body);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.muij-bar-value,
	.muij-bar-column-value {
		color: var(--text-secondary);
		font-variant-numeric: tabular-nums;
		line-height: 1.15;
	}

	.muij-bar-value {
		overflow: hidden;
		text-align: right;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.muij-bar-track {
		height: 10px;
		background: var(--bg-soft);
		border-radius: 999px;
		overflow: hidden;
	}

	.muij-bar-fill {
		height: 100%;
		border-radius: 999px;
		transition: width 220ms ease;
	}

	.muij-bar-columns {
		display: grid;
		grid-auto-flow: column;
		grid-auto-columns: minmax(46px, 1fr);
		gap: 10px;
		align-items: end;
	}

	.muij-bar-column {
		display: grid;
		grid-template-rows: 120px auto auto;
		gap: 6px;
		justify-items: center;
	}

	.muij-bar-column-track {
		width: 100%;
		height: 120px;
		display: flex;
		align-items: flex-end;
		justify-content: center;
		background: var(--bg-soft);
		border-radius: var(--radius-sm);
		padding: 4px;
	}

	.muij-bar-column-fill {
		width: 100%;
		border-radius: var(--radius-xs);
		transition: height 220ms ease;
	}

	.muij-bar-column-label {
		width: 100%;
		text-align: center;
	}
</style>
