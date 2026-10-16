<script lang="ts">
	import { sanitizeCssValue } from './cssUtil';

	export let percent: number = 0;
	export let label: string = '';
	export let ariaLabel: string = '';
	export let color: string = 'var(--accent-primary)';

	$: clampedPercent = Number.isFinite(percent) ? Math.min(100, Math.max(0, percent)) : 0;
	$: safeColor = sanitizeCssValue(color).trim() || 'var(--accent-primary)';
	$: resolvedAriaLabel = ariaLabel.trim() || label.trim() || 'Progress';
</script>

<div class="muij-progressbar">
	{#if label.trim().length > 0}
		<div class="muij-progressbar-label">{label}</div>
	{/if}
	<div
		class="muij-progressbar-track"
		role="progressbar"
		aria-label={resolvedAriaLabel}
		aria-valuemin="0"
		aria-valuemax="100"
		aria-valuenow={clampedPercent}
	>
		<div class="muij-progressbar-fill" style={`width:${clampedPercent}%;background:${safeColor}`}></div>
	</div>
	<div class="muij-progressbar-value">{clampedPercent.toFixed(0)}%</div>
</div>

<style>
	.muij-progressbar {
		display: grid;
		gap: 4px;
	}

	.muij-progressbar-label,
	.muij-progressbar-value {
		font-family: var(--font-primary);
		font-size: 0.6875rem;
		color: var(--text-secondary);
	}

	.muij-progressbar-track {
		height: 10px;
		background: var(--bg-soft);
		border-radius: 999px;
		overflow: hidden;
	}

	.muij-progressbar-fill {
		height: 100%;
		border-radius: 999px;
		transition: width 220ms ease;
	}
</style>
