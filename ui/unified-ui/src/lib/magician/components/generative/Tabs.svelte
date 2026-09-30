<script lang="ts">
	import { onDestroy } from 'svelte';
	import { buildStableDomId } from './idUtil';

	interface MuijTab {
		label: string;
		content?: string;
	}

	export let tabs: MuijTab[] = [];
	export let activeIndex: number = 0;
	export let idBase: string = '';
	$: tabsId = buildStableDomId('muij-tabs', idBase);
	$: safeActiveIndex = Number.isFinite(+activeIndex) ? Math.floor(+activeIndex) : 0;

	// R90: preserve user tab selection across re-renders, but honor an explicit
	// server-side activeIndex change when the incoming prop value actually shifts.
	// R136: userOverride is index-based — reorder deltas could cause wrong tab selection.
	// Since no production code emits Reorder (R112), this is accepted as a known limitation.
	let userOverride: number | null = null;
	let lastServerActiveIndex: number | null = null;
	$: if (lastServerActiveIndex === null) {
		lastServerActiveIndex = safeActiveIndex;
	} else if (safeActiveIndex !== lastServerActiveIndex) {
		lastServerActiveIndex = safeActiveIndex;
		userOverride = null;
	}
	// R130: Clear stale userOverride when tabs array shrinks below the selected index
	$: if (userOverride !== null && tabs.length > 0 && userOverride >= tabs.length) {
		userOverride = null;
	}
	$: effectiveIndex = userOverride ?? lastServerActiveIndex ?? 0;
	// R686: Guard against NaN from non-finite effectiveIndex and empty tabs array
	$: clampedIndex = tabs.length > 0
		? Math.max(0, Math.min(tabs.length - 1, Number.isFinite(effectiveIndex) ? effectiveIndex : 0))
		: 0;

	function selectTab(index: number) {
		userOverride = index;
	}

	function tabId(index: number): string {
		return `${tabsId}-tab-${index}`;
	}

	function panelId(index: number): string {
		return `${tabsId}-panel-${index}`;
	}

	$: activePanelId = panelId(clampedIndex);

	// R557: Track rAF handle for cleanup on component destroy
	let pendingRaf: number | null = null;
	// R687: Bind to container element to scope DOM queries to this instance
	let tabsEl: HTMLElement | undefined;

	function handleTabKeydown(event: KeyboardEvent, index: number): void {
		let next = index;
		if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
		else if (event.key === 'ArrowLeft') next = (index - 1 + tabs.length) % tabs.length;
		else if (event.key === 'Home') next = 0;
		else if (event.key === 'End') next = tabs.length - 1;
		else return;

		event.preventDefault();
		selectTab(next);
		if (typeof window !== 'undefined') {
			const targetId = tabId(next);  // R137: Capture ID before rAF
			if (pendingRaf !== null) cancelAnimationFrame(pendingRaf);
			pendingRaf = requestAnimationFrame(() => {
				pendingRaf = null;
				// R137: Verify element still exists (delta may have removed it mid-rAF)
				const el = document.getElementById(targetId);
				if (el) {
					el.focus();
				} else {
					// R687: Scope fallback query to this component's DOM tree
					// to avoid matching tab buttons from overlapping Tabs instances.
					const scope = tabsEl ?? document;
					// R583: CSS.escape guards against special chars in tabsId
					const escapedId = typeof CSS !== 'undefined' && CSS.escape ? CSS.escape(tabsId) : tabsId;
					const fallback = scope.querySelector(`[id^="${escapedId}-tab-"]`) as HTMLElement;
					fallback?.focus();
				}
			});
		}
	}

	onDestroy(() => {
		if (pendingRaf !== null) cancelAnimationFrame(pendingRaf);
	});
</script>

	{#if tabs.length > 0}
		<div class="muij-tabs" bind:this={tabsEl}>
			<div class="muij-tabs-bar" role="tablist" aria-orientation="horizontal">
				<!-- R578: Key by label to enable identity-based diffing on tab reorder/insert -->
				{#each tabs as tab, i (tab.label)}
					<button
					id={tabId(i)}
					type="button"
					class="muij-tabs-tab"
					class:muij-tabs-tab-active={i === clampedIndex}
					role="tab"
					aria-selected={i === clampedIndex}
					aria-controls={panelId(i)}
					tabindex={i === clampedIndex ? 0 : -1}
					on:click={() => selectTab(i)}
					on:keydown={(event) => handleTabKeydown(event, i)}
				>
					{tab.label}
				</button>
			{/each}
		</div>
			<div
				id={activePanelId}
				class="muij-tabs-panel"
				role="tabpanel"
				aria-labelledby={tabId(clampedIndex)}
				tabindex="0"
			>
				<slot activeIndex={clampedIndex}>
					{tabs[clampedIndex]?.content ?? ''}
				</slot>
			</div>
			{#each tabs as _tab, i (i)}
				{#if i !== clampedIndex}
					<!-- R353: Keep aria-controls references valid for inactive tabs. -->
					<!-- R677: `hidden` alone is sufficient — `aria-hidden` is redundant. -->
					<div
						id={panelId(i)}
						role="tabpanel"
						aria-labelledby={tabId(i)}
						hidden
					></div>
				{/if}
			{/each}
		</div>
	{/if}

<style>
	.muij-tabs {
		display: flex;
		flex-direction: column;
	}

	.muij-tabs-bar {
		display: flex;
		border-bottom: 1px solid var(--border-soft);
		gap: 0;
	}

	.muij-tabs-tab {
		padding: 8px 16px;
		border: none;
		background: transparent;
		cursor: pointer;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		color: var(--text-muted);
		border-bottom: 2px solid transparent;
		transition: color 0.15s, border-color 0.15s;
	}

	.muij-tabs-tab:hover {
		color: var(--text-primary);
	}

	.muij-tabs-tab:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: -2px;
	}

	.muij-tabs-tab-active {
		color: var(--text-primary);
		border-bottom-color: var(--accent-primary);
	}

	.muij-tabs-panel {
		padding: var(--space-md);
		font-family: var(--font-primary);
		font-size: 0.875rem;
		color: var(--text-body);
		line-height: 1.6;
		overflow-wrap: anywhere;
	}
</style>
