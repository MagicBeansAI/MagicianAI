<script lang="ts">
	/**
	 * CLI Agent — the Workbench terminal, opened full-screen (below the top bar)
	 * over the cockpit. The cockpit stays mounted underneath, so closing returns
	 * you exactly where you were with no loss. Reuses `WorkbenchColumn` as-is.
	 * (The standalone `/dev` route shows the same server-side sessions.)
	 */
	import { createEventDispatcher } from 'svelte';
	import WorkbenchColumn from '$lib/shell/WorkbenchColumn.svelte';

	export let open = false;

	const dispatch = createEventDispatcher<{ close: void }>();
</script>

{#if open}
	<section class="term" aria-label="CLI Agent">
		<header class="term__head">
			<span class="term__title">⌥ CLI Agent</span>
			<button type="button" class="term__close" on:click={() => dispatch('close')} aria-label="Close CLI Agent">✕</button>
		</header>
		<div class="term__body">
			<WorkbenchColumn threadId={null} placement="main" />
		</div>
	</section>
{/if}

<style>
	.term {
		position: fixed;
		left: 0;
		right: 0;
		bottom: 0;
		/* Full-screen below the top bar — same instance, cockpit stays mounted
		   underneath, so closing returns you exactly where you were. */
		top: var(--app-header-height, 48px);
		z-index: 70;
		display: flex;
		flex-direction: column;
		border-top: 1px solid var(--vibe-border, var(--border-default));
		background: var(--vibe-surface, var(--bg-card));
		box-shadow: var(--shadow-lg, 0 -16px 40px -20px rgba(0, 0, 0, 0.45));
		animation: term-rise 0.2s var(--ease-settle, ease);
	}
	.term__head {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.4rem 0.7rem;
		border-bottom: 1px solid var(--vibe-border, var(--border-soft));
	}
	.term__title {
		font-family: var(--font-display, inherit);
		font-size: 0.8rem;
		font-weight: 600;
		color: var(--vibe-text, var(--text-primary));
	}
	.term__close {
		margin-left: auto;
		border: 0;
		background: transparent;
		color: var(--vibe-text-muted, var(--text-muted));
		font: inherit;
		cursor: pointer;
		padding: 0.15rem 0.4rem;
	}
	.term__close:hover {
		color: var(--vibe-text, var(--text-primary));
	}
	.term__body {
		flex: 1;
		min-height: 0;
		overflow: hidden;
		/* WorkbenchColumn's root is `flex: 1 1 auto`, so it only fills when its
		   parent is a flex column — without this it collapsed to content height. */
		display: flex;
		flex-direction: column;
	}
	.term__body :global(> *) {
		flex: 1;
		min-height: 0;
	}
	@keyframes term-rise {
		from {
			transform: translateY(100%);
		}
		to {
			transform: translateY(0);
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.term {
			animation: none;
		}
	}
</style>
