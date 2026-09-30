<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	export let loading: boolean = false;
	export let label: string = 'Refetch card data';

	const dispatch = createEventDispatcher<{ refresh: void }>();

	function handleClick(event: MouseEvent): void {
		event.stopPropagation();
		dispatch('refresh');
	}
</script>

<button
	type="button"
	class="muij-live-refresh"
	aria-label={label}
	title={label}
	disabled={loading}
	on:click={handleClick}
>
	<span class:muij-live-refresh-spinning={loading} aria-hidden="true">↻</span>
</button>

<style>
	.muij-live-refresh {
		position: absolute;
		top: 6px;
		right: 6px;
		z-index: 2;
		display: inline-grid;
		width: 26px;
		height: 26px;
		place-items: center;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm, 6px);
		background: color-mix(in srgb, var(--bg-card) 88%, transparent);
		color: var(--text-secondary);
		box-shadow: var(--shadow-xs, 0 1px 2px rgba(15, 23, 42, 0.08));
		cursor: pointer;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		line-height: 1;
		transition:
			background 120ms ease,
			border-color 120ms ease,
			color 120ms ease,
			transform 120ms ease;
	}

	.muij-live-refresh:hover:not(:disabled) {
		background: var(--bg-surface);
		border-color: var(--accent-primary);
		color: var(--accent-primary);
		transform: translateY(-1px);
	}

	.muij-live-refresh:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: 2px;
	}

	.muij-live-refresh:disabled {
		cursor: progress;
		opacity: 0.72;
	}

	.muij-live-refresh-spinning {
		animation: muij-live-refresh-spin 800ms linear infinite;
	}

	@keyframes muij-live-refresh-spin {
		to {
			transform: rotate(360deg);
		}
	}
</style>
