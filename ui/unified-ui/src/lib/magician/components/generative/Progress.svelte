<script lang="ts">
	export let percent: number = 0;
	export let status: 'active' | 'success' | 'error' = 'active';
	export let label: string = '';
	export let showPercent: boolean = true;

	$: clamped = Math.max(0, Math.min(100, Number.isFinite(+percent) ? +percent : 0));
	$: safeStatus = status === 'success' || status === 'error' ? status : 'active';
</script>

<!-- R662: aria-live announces status changes (success/error) to screen readers (WCAG 4.1.3) -->
<div class="muij-progress-container" aria-live="polite" aria-atomic="true">
	{#if label || showPercent}
		<div class="muij-progress-header">
			{#if label}
				<span class="muij-progress-label">{label}</span>
			{/if}
			{#if showPercent}
				<span class="muij-progress-percent">{Math.round(clamped)}%</span>
			{/if}
		</div>
	{/if}
	<div
		class="muij-progress"
		role="progressbar"
		aria-valuenow={Math.round(clamped)}
		aria-valuemin={0}
		aria-valuemax={100}
		aria-label={label || 'Progress'}
	>
		<div
			class="muij-progress-bar"
			class:muij-progress-active={safeStatus === 'active'}
			class:muij-progress-success={safeStatus === 'success'}
			class:muij-progress-error={safeStatus === 'error'}
			style="width: {clamped}%;"
		></div>
	</div>
</div>

<style>
	.muij-progress-container {
		width: 100%;
	}

	.muij-progress-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: 4px;
	}

	.muij-progress-label {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
	}

	.muij-progress-percent {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--text-muted);
	}

	.muij-progress {
		height: 8px;
		background: var(--bg-soft);
		border-radius: var(--radius-full, 9999px);
		overflow: hidden;
	}

	.muij-progress-bar {
		height: 100%;
		border-radius: var(--radius-full, 9999px);
		transition: width 0.3s ease;
	}

	.muij-progress-active {
		background: var(--accent-primary);
	}

	.muij-progress-success {
		background: var(--color-success);
	}

	.muij-progress-error {
		background: var(--color-error);
	}
</style>
