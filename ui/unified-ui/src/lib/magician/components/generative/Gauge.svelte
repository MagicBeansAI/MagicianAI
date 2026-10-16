<script lang="ts">
	export let fill: number | undefined = undefined;
	export let label: string = '';
	export let sublabel: string | undefined = undefined;
	export let iteration: number | undefined = undefined;
	export let max_iterations: number | undefined = undefined;

	const SIZE = 120;
	const STROKE = 8;
	const RADIUS = (SIZE - STROKE) / 2;
	const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

	function asFiniteNumber(value: unknown): number | undefined {
		const parsed = typeof value === 'number' ? value : (typeof value === 'string' ? Number(value) : NaN);
		return Number.isFinite(parsed) ? parsed : undefined;
	}

	$: safeFill = asFiniteNumber(fill);
	$: safeIteration = asFiniteNumber(iteration);
	$: safeMaxIterations = asFiniteNumber(max_iterations);
	$: isActive = safeFill != null && safeFill >= 0;
	$: clampedFill = Math.max(0, Math.min(1, safeFill ?? 0));
	$: offset = isActive ? CIRCUMFERENCE * (1 - clampedFill) : 0;
	$: percent = isActive ? Math.round(clampedFill * 100) : null;
	$: derivedSublabel =
		sublabel ??
		(safeIteration != null && safeMaxIterations != null
			? `${safeIteration} / ${safeMaxIterations}`
			: undefined);
	$: progressText = percent != null ? `${percent}%` : 'N/A';
	$: gaugeTooltip = [
		label || 'Progress',
		progressText,
		derivedSublabel
	].filter(Boolean).join(' · ');
</script>

<div
	class="muij-gauge"
	role="meter"
	aria-valuenow={percent ?? undefined}
	aria-valuemin={0}
	aria-valuemax={100}
	aria-valuetext={gaugeTooltip}
	aria-label={gaugeTooltip}
	title={gaugeTooltip}
>
	<svg viewBox="0 0 {SIZE} {SIZE}" class="muij-gauge-svg" aria-hidden="true" focusable="false">
		<title>{gaugeTooltip}</title>
		<!-- background ring -->
		<circle
			cx={SIZE / 2}
			cy={SIZE / 2}
			r={RADIUS}
			class="muij-ring-bg"
			fill="none"
			stroke-width={STROKE}
		/>
		<!-- foreground ring (active) or dashed ring (N/A) -->
		{#if isActive}
			<circle
				cx={SIZE / 2}
				cy={SIZE / 2}
				r={RADIUS}
				class="muij-ring-fg"
				fill="none"
				stroke-width={STROKE}
				stroke-linecap="round"
				stroke-dasharray={CIRCUMFERENCE}
				stroke-dashoffset={offset}
				transform="rotate(-90 {SIZE / 2} {SIZE / 2})"
			/>
		{:else}
			<circle
				cx={SIZE / 2}
				cy={SIZE / 2}
				r={RADIUS}
				class="muij-ring-na"
				fill="none"
				stroke-width={STROKE}
				stroke-dasharray="6 4"
				transform="rotate(-90 {SIZE / 2} {SIZE / 2})"
			/>
		{/if}
	</svg>
	<div class="muij-gauge-label">
		<span class="muij-gauge-percent" title={gaugeTooltip}>{progressText}</span>
		{#if label}<span class="muij-gauge-title" title={label}>{label}</span>{/if}
		{#if derivedSublabel}<span class="muij-gauge-sublabel" title={derivedSublabel}>{derivedSublabel}</span>{/if}
	</div>
</div>

<style>
	.muij-gauge {
		position: relative;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 120px;
		height: 120px;
	}

	.muij-gauge-svg {
		width: 100%;
		height: 100%;
	}

	.muij-ring-bg {
		stroke: var(--border-soft);
	}

	.muij-ring-fg {
		stroke: var(--accent-primary);
		transition: stroke-dashoffset 150ms linear;
	}

	.muij-ring-na {
		stroke: var(--text-muted);
		opacity: 0.5;
	}

	.muij-gauge-label {
		position: absolute;
		inset: 0;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		pointer-events: none;
	}

	.muij-gauge-percent {
		font-family: var(--font-mono);
		font-size: 1.25rem;
		font-weight: 700;
		color: var(--text-primary);
		line-height: 1;
	}

	.muij-gauge-title {
		font-family: var(--font-primary);
		font-size: 0.625rem;
		color: var(--text-muted);
		margin-top: 4px;
		text-align: center;
		max-width: 80px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.muij-gauge-sublabel {
		font-family: var(--font-mono);
		font-size: 0.6rem;
		color: var(--text-muted);
		margin-top: 2px;
	}
</style>
