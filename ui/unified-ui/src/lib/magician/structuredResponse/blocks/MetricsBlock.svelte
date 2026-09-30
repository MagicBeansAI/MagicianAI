<script lang="ts">
	import type { StructuredMetricsBlockV1 } from '../types';

	export let block: StructuredMetricsBlockV1;

	const metricValue = (value: string, unit: string | undefined): string => {
		return unit ? `${value} ${unit}` : value;
	};

	const trendLabel = (trend: string | undefined): string => {
		if (trend === 'up') return '↗';
		if (trend === 'down') return '↘';
		if (trend === 'flat') return '→';
		return '';
	};
</script>

<section class="sr-block sr-block--metrics" data-block-kind="metrics">
	{#if block.title}<h4 class="sr-block__title">{block.title}</h4>{/if}
	<div class="sr-metrics">
		{#each block.items as metric}
			<div class="sr-metric">
				<div class="sr-metric__label">{metric.label}</div>
				<div class="sr-metric__value">
					{trendLabel(metric.trend)} {metricValue(metric.value, metric.unit)}
				</div>
			</div>
		{/each}
	</div>
</section>

<style>
	.sr-block {
		display: block;
	}

	.sr-block__title {
		font-size: var(--text-sm);
		font-weight: 650;
		margin: 0 0 0.35rem;
	}

	.sr-metrics {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
		gap: 0.5rem;
	}

	.sr-metric {
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
		padding: 0.45rem 0.55rem;
	}

	.sr-metric__label {
		font-size: var(--text-xs);
		color: var(--text-muted);
	}

	.sr-metric__value {
		font-size: var(--text-sm);
		font-weight: 650;
		margin-top: 0.15rem;
	}
</style>
