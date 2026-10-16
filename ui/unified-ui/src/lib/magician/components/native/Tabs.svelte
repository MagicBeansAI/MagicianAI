<script lang="ts" context="module">
	let nativeTabsSequence = 0;

	function nextNativeTabsId(): string {
		nativeTabsSequence += 1;
		return `native-tabs-${nativeTabsSequence}`;
	}

	function idPart(value: string): string {
		return value.trim().replace(/[^a-zA-Z0-9_-]+/g, '-').replace(/^-+|-+$/g, '') || 'tabs';
	}
</script>

<script lang="ts">
	import { onDestroy } from 'svelte';

	interface NativeTab {
		label: string;
		content?: string;
	}

	export let tabs: NativeTab[] = [];
	export let activeIndex = 0;
	export let idBase = '';

	const generatedId = nextNativeTabsId();
	let tabsEl: HTMLElement | undefined;
	let pendingRaf: number | null = null;

	$: tabsId = idBase ? `native-tabs-${idPart(idBase)}` : generatedId;
	$: safeActiveIndex = Number.isFinite(+activeIndex) ? Math.floor(+activeIndex) : 0;
	$: clampedIndex =
		tabs.length > 0 ? Math.max(0, Math.min(tabs.length - 1, safeActiveIndex)) : 0;
	$: activePanelId = panelId(clampedIndex);

	function tabId(index: number): string {
		return `${tabsId}-tab-${index}`;
	}

	function panelId(index: number): string {
		return `${tabsId}-panel-${index}`;
	}

	function selectTab(index: number): void {
		activeIndex = Math.max(0, Math.min(tabs.length - 1, index));
	}

	function focusTab(index: number): void {
		if (typeof window === 'undefined') return;
		const targetId = tabId(index);
		if (pendingRaf !== null) cancelAnimationFrame(pendingRaf);
		pendingRaf = requestAnimationFrame(() => {
			pendingRaf = null;
			const target = document.getElementById(targetId);
			if (target) {
				target.focus();
			} else {
				tabsEl?.querySelector<HTMLElement>('[role="tab"]')?.focus();
			}
		});
	}

	function handleTabKeydown(event: KeyboardEvent, index: number): void {
		let next = index;
		if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
		else if (event.key === 'ArrowLeft') next = (index - 1 + tabs.length) % tabs.length;
		else if (event.key === 'Home') next = 0;
		else if (event.key === 'End') next = tabs.length - 1;
		else return;

		event.preventDefault();
		selectTab(next);
		focusTab(next);
	}

	onDestroy(() => {
		if (pendingRaf !== null) cancelAnimationFrame(pendingRaf);
	});
</script>

{#if tabs.length > 0}
	<div class="native-tabs" bind:this={tabsEl}>
		<div class="native-tabs__bar" role="tablist" aria-orientation="horizontal">
			{#each tabs as tab, i (tab.label)}
				<button
					id={tabId(i)}
					type="button"
					class={['native-tabs__tab', i === clampedIndex ? 'native-tabs__tab--active' : '']
						.filter(Boolean)
						.join(' ')}
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
			class="native-tabs__panel"
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
				<div id={panelId(i)} role="tabpanel" aria-labelledby={tabId(i)} hidden></div>
			{/if}
		{/each}
	</div>
{/if}

<style>
	.native-tabs {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	.native-tabs__bar {
		display: flex;
		gap: 0.125rem;
		border-bottom: 1px solid var(--border-soft);
	}

	.native-tabs__tab {
		border: 0;
		border-bottom: 2px solid transparent;
		background: transparent;
		color: var(--text-muted);
		cursor: pointer;
		font-family: var(--font-primary);
		font-size: 0.8125rem;
		font-weight: 600;
		padding: 0.5rem 0.875rem;
		transition:
			color 140ms ease,
			border-color 140ms ease,
			background 140ms ease;
	}

	.native-tabs__tab:hover {
		color: var(--text-primary);
		background: var(--bg-soft);
	}

	.native-tabs__tab:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: -2px;
	}

	.native-tabs__tab--active {
		color: var(--text-primary);
		border-bottom-color: var(--accent-primary);
	}

	.native-tabs__panel {
		padding: var(--space-md);
		font-family: var(--font-primary);
		font-size: 0.875rem;
		line-height: 1.6;
		color: var(--text-body);
		overflow-wrap: anywhere;
	}
</style>
