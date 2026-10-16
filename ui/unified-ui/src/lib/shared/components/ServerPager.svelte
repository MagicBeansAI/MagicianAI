<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	import Icon from '$lib/shared/icons/Icon.svelte';

	export let currentPage = 1;
	export let pageCount = 1;
	export let startItem = 0;
	export let endItem = 0;
	export let totalItems = 0;
	export let pageCountExact = true;
	export let totalItemsExact = true;
	export let ariaLabel = 'Pagination';
	export let disabled = false;
	export let loading = false;

	const dispatch = createEventDispatcher<{
		pagechange: { page: number };
	}>();

	$: safePageCount = Math.max(1, Math.floor(pageCount || 1));
	$: safeCurrentPage = Math.min(
		safePageCount,
		Math.max(1, Math.floor(currentPage || 1))
	);
	$: safeStartItem = Math.max(0, Math.floor(startItem || 0));
	$: safeEndItem = Math.max(safeStartItem, Math.floor(endItem || 0));
	$: safeTotalItems = Math.max(0, Math.floor(totalItems || 0));
	$: controlsDisabled = disabled || loading;

	function goToPage(page: number): void {
		if (controlsDisabled) return;
		const nextPage = Math.min(safePageCount, Math.max(1, Math.floor(page)));
		if (nextPage === safeCurrentPage) return;
		dispatch('pagechange', { page: nextPage });
	}
</script>

<nav class="server-pager-shell" aria-label={ariaLabel}>
	<div class="server-pager">
		<button
			type="button"
			class="server-pager__button"
			disabled={controlsDisabled || safeCurrentPage <= 1}
			title="First page"
			aria-label="First page"
			on:click={() => goToPage(1)}
		>
			<Icon name="chevrons-left" size={13} />
		</button>
		<button
			type="button"
			class="server-pager__button"
			disabled={controlsDisabled || safeCurrentPage <= 1}
			title="Previous page"
			aria-label="Previous page"
			on:click={() => goToPage(safeCurrentPage - 1)}
		>
			<Icon name="chevron-left" size={13} />
		</button>
		<span class="server-pager__summary">
			Page {safeCurrentPage} of {safePageCount}{pageCountExact ? '' : '+'}
			<span>{safeStartItem}-{safeEndItem} of {safeTotalItems}{totalItemsExact ? '' : '+'}</span>
		</span>
		<button
			type="button"
			class="server-pager__button"
			disabled={controlsDisabled || safeCurrentPage >= safePageCount}
			title="Next page"
			aria-label="Next page"
			on:click={() => goToPage(safeCurrentPage + 1)}
		>
			<Icon name="chevron-right" size={13} />
		</button>
		<button
			type="button"
			class="server-pager__button"
			disabled={controlsDisabled || !pageCountExact || safeCurrentPage >= safePageCount}
			title="Last page"
			aria-label="Last page"
			on:click={() => goToPage(safePageCount)}
		>
			<Icon name="chevrons-right" size={13} />
		</button>
	</div>
</nav>

<style>
	.server-pager-shell {
		display: flex;
		justify-content: flex-end;
		padding: 0.15rem 0;
	}

	.server-pager {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		max-width: 100%;
		padding: 0.18rem;
		border: 1px solid color-mix(in srgb, var(--border-soft, #d9dee8) 82%, transparent);
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--bg-card, #fff) 86%, transparent);
	}

	.server-pager__button {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.65rem;
		height: 1.65rem;
		padding: 0;
		border: 1px solid transparent;
		border-radius: var(--radius-full, 999px);
		background: color-mix(in srgb, var(--bg-soft, #f5f7fb) 72%, transparent);
		color: var(--text-secondary, #5f6672);
		cursor: pointer;
	}

	.server-pager__button:hover,
	.server-pager__button:focus-visible {
		border-color: var(--border-soft, #d9dee8);
		color: var(--text-primary, #111827);
		background: var(--bg-soft, #f5f7fb);
		outline: none;
	}

	.server-pager__button:disabled {
		cursor: default;
		opacity: 0.45;
	}

	.server-pager__button:disabled:hover {
		border-color: transparent;
		color: var(--text-secondary, #5f6672);
		background: color-mix(in srgb, var(--bg-soft, #f5f7fb) 72%, transparent);
	}

	.server-pager__summary {
		display: inline-flex;
		align-items: baseline;
		gap: 0.4rem;
		padding: 0 0.45rem;
		color: var(--text-secondary, #5f6672);
		font-size: var(--text-xs, 0.78rem);
		white-space: nowrap;
		font-variant-numeric: tabular-nums;
	}

	.server-pager__summary span {
		color: var(--text-muted, #7f8794);
	}

	@media (max-width: 720px) {
		.server-pager-shell {
			justify-content: flex-start;
			overflow-x: auto;
			padding-bottom: 0.25rem;
		}

		.server-pager {
			justify-content: flex-start;
		}
	}
</style>
