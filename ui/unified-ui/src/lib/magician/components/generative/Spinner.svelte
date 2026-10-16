<script lang="ts">
	export let size: 'sm' | 'md' | 'lg' = 'md';
	export let label: string = 'Loading';
	export let centered: boolean = false;

	$: safeSize = size === 'sm' || size === 'lg' ? size : 'md';
</script>

<div class="muij-spinner-wrap" class:muij-spinner-centered={centered} role="status" aria-live="polite">
	<span class={`muij-spinner muij-spinner-${safeSize}`}></span>
	<span class="muij-spinner-label">{label}</span>
</div>

<style>
	.muij-spinner-wrap {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
	}

	.muij-spinner {
		border-radius: 999px;
		border: 2px solid var(--border-soft);
		border-top-color: var(--accent-primary);
		animation: muij-spinner-rotate 700ms linear infinite;
	}

	.muij-spinner-sm {
		width: 12px;
		height: 12px;
	}

	.muij-spinner-md {
		width: 16px;
		height: 16px;
	}

	.muij-spinner-lg {
		width: 22px;
		height: 22px;
		border-width: 3px;
	}

	.muij-spinner-centered {
		display: flex;
		justify-content: center;
		width: 100%;
		padding: 2rem 0;
	}

	@keyframes muij-spinner-rotate {
		to { transform: rotate(360deg); }
	}
</style>
