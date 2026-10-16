<script lang="ts">
	import type { StructuredMetricV1, StructuredMetricsBlockV1 } from '$lib/magician/structuredResponse/types';
	import { tintVars } from '../tint';

	export let block: StructuredMetricsBlockV1;

	const TRENDS: Record<NonNullable<StructuredMetricV1['trend']>, { glyph: string; label: string }> = {
		up: { glyph: '↑', label: 'Trending up' },
		down: { glyph: '↓', label: 'Trending down' },
		flat: { glyph: '→', label: 'Flat' }
	};

	$: hero = block.items[0];
	$: rest = block.items.slice(1);
</script>

<section class="cn-metrics" data-next-block="metrics">
	{#if block.title}<h4 class="cn-metrics__title">{block.title}</h4>{/if}
	{#if hero}
		<div class="cn-metrics__hero" data-testid="cn-metric-hero" style={tintVars(hero.label)}>
			<span class="cn-metrics__hero-label">{hero.label}</span>
			<span class="cn-metrics__hero-value">
				<span class="cn-metrics__number">{hero.value}</span>
				{#if hero.unit}<span class="cn-metrics__unit">{hero.unit}</span>{/if}
				{#if hero.trend && TRENDS[hero.trend]}
					<span class="cn-trend" role="img" aria-label={TRENDS[hero.trend].label}
						>{TRENDS[hero.trend].glyph}</span
					>
				{/if}
			</span>
		</div>
	{/if}
	{#if rest.length > 0}
		<ul class="cn-metrics__pills">
			{#each rest as metric}
				<li class="cn-pill" data-testid="cn-metric-pill" style={tintVars(metric.label)}>
					<span class="cn-pill__label">{metric.label}</span>
					<span class="cn-pill__value">
						{metric.value}{#if metric.unit}&nbsp;<span class="cn-metrics__unit">{metric.unit}</span>{/if}
					</span>
					{#if metric.trend && TRENDS[metric.trend]}
						<span class="cn-trend" role="img" aria-label={TRENDS[metric.trend].label}
							>{TRENDS[metric.trend].glyph}</span
						>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</section>

<style>
	.cn-metrics {
		display: grid;
		gap: 0.5rem;
	}

	.cn-metrics__title {
		margin: 0;
		font-size: var(--text-sm);
		font-weight: 650;
	}

	.cn-metrics__hero {
		display: grid;
		gap: 0.1rem;
		justify-self: start;
		padding: 0.6rem 0.9rem 0.7rem;
		border-radius: var(--radius-md);
		background: color-mix(in srgb, var(--cn-tint) 55%, var(--bg-card));
		color: var(--cn-tint-ink);
		box-shadow: var(--shadow-float);
	}

	.cn-metrics__hero-label {
		font-size: var(--text-xs);
		color: var(--text-muted);
	}

	.cn-metrics__hero-value {
		display: inline-flex;
		align-items: baseline;
		gap: 0.3rem;
	}

	.cn-metrics__number {
		font-family: var(--font-display);
		font-size: 2.25rem;
		font-weight: 700;
		line-height: 1.05;
		font-variant-numeric: tabular-nums;
	}

	.cn-metrics__unit {
		font-size: var(--text-sm);
		color: var(--text-muted);
	}

	/* Direction only, no good/bad colour: a falling latency is good news. */
	.cn-trend {
		font-weight: 700;
	}

	.cn-metrics__hero .cn-trend {
		font-size: 1.25rem;
	}

	.cn-metrics__pills {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.cn-pill {
		display: inline-flex;
		align-items: baseline;
		gap: 0.4rem;
		padding: 0.25rem 0.7rem;
		border-radius: var(--radius-chip);
		background: var(--cn-tint);
		color: var(--cn-tint-ink);
		font-size: var(--text-sm);
	}

	.cn-pill__label {
		font-size: var(--text-xs);
		color: var(--text-muted);
	}

	.cn-pill__value {
		font-weight: 650;
		font-variant-numeric: tabular-nums;
	}
</style>
