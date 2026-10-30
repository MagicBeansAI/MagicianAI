<script lang="ts">
	import { onMount, tick } from 'svelte';
	import type { SettingsNavCategory } from './settingsIndex';

	interface Props {
		categories: SettingsNavCategory[];
		activeId: string;
		query: string;
		/** Matching cards while searching, or null with no query. */
		matchCount: number | null;
		onselect: (id: string) => void;
		onsubmit: () => void;
		/** Hide the "Settings" heading: a phone app titles the screen itself. */
		hideTitle?: boolean;
	}

	let { categories, activeId, query = $bindable(), matchCount, onselect, onsubmit, hideTitle = false }: Props = $props();

	let searchInput: HTMLInputElement | undefined = $state();
	let list: HTMLUListElement | undefined = $state();

	// On narrow screens the list is a horizontal chip row: keep the active chip
	// in view as the page scrolls. Sets the row's own scrollLeft so the page
	// itself never scrolls.
	$effect(() => {
		const id = activeId;
		if (!list) return;
		void tick().then(() => {
			if (!list || list.scrollWidth <= list.clientWidth) return;
			const chip = list.querySelector<HTMLElement>(`[data-category="${CSS.escape(id)}"]`);
			if (!chip) return;
			const start = chip.offsetLeft - list.offsetLeft;
			const end = start + chip.offsetWidth;
			if (start < list.scrollLeft || end > list.scrollLeft + list.clientWidth) {
				list.scrollTo({ left: Math.max(0, start - 16), behavior: 'smooth' });
			}
		});
	});

	function handleSearchKeydown(event: KeyboardEvent) {
		if (event.key === 'Escape' && query) {
			event.preventDefault();
			event.stopPropagation();
			query = '';
		} else if (event.key === 'Enter') {
			event.preventDefault();
			onsubmit();
		}
	}

	function handleCategoryClick(event: MouseEvent, id: string) {
		if (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return;
		event.preventDefault();
		onselect(id);
	}

	onMount(() => {
		// `/` focuses the search unless the user is typing somewhere else or
		// another surface (the command palette) already took the key.
		const handleKeydown = (event: KeyboardEvent) => {
			if (event.key !== '/' || event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) {
				return;
			}
			const target = event.target;
			if (
				target instanceof HTMLElement &&
				(['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName) || target.isContentEditable)
			) {
				return;
			}
			event.preventDefault();
			searchInput?.focus();
			searchInput?.select();
		};
		window.addEventListener('keydown', handleKeydown);
		return () => window.removeEventListener('keydown', handleKeydown);
	});
</script>

<nav class="settings-nav" aria-label="Settings categories">
	<div class="settings-nav-head">
		{#if hideTitle}<h1 class="visually-hidden">Settings</h1>{:else}<h1>Settings</h1>{/if}
		<label class="settings-nav-search">
			<svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
				<circle cx="7" cy="7" r="4.5" fill="none" stroke="currentColor" stroke-width="1.6" />
				<path d="M10.5 10.5 14 14" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
			</svg>
			<input
				bind:this={searchInput}
				bind:value={query}
				type="search"
				placeholder="Search settings"
				aria-label="Search settings"
				autocomplete="off"
				spellcheck="false"
				onkeydown={handleSearchKeydown}
			/>
			{#if !query}<kbd aria-hidden="true">/</kbd>{/if}
		</label>
	</div>
	{#if matchCount !== null}
		<p class="settings-nav-summary" aria-live="polite">
			{matchCount === 1 ? '1 matching setting' : `${matchCount} matching settings`}
		</p>
	{/if}
	<ul class="settings-nav-list" bind:this={list}>
		{#each categories as category (category.id)}
			<li>
				<a
					href={`#${category.id}`}
					data-category={category.id}
					class:active={category.id === activeId}
					class:empty={category.count === 0}
					aria-current={category.id === activeId ? 'location' : undefined}
					aria-disabled={category.count === 0 ? 'true' : undefined}
					tabindex={category.count === 0 ? -1 : undefined}
					onclick={(event) => handleCategoryClick(event, category.id)}
				>
					<span class="settings-nav-label">{category.label}</span>
					<span class="settings-nav-count">{category.count}</span>
				</a>
			</li>
		{/each}
	</ul>
</nav>

<style>
	.settings-nav {
		align-self: start;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		box-sizing: border-box;
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		max-height: calc(100dvh - 2rem);
		overflow-y: auto;
		padding: 1rem 0.75rem;
		position: sticky;
		top: 1rem;
	}

	.settings-nav-head {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	h1 {
		color: var(--text-primary);
		font-family: var(--font-display, var(--font-primary));
		font-size: 1.45rem;
		font-weight: 700;
		letter-spacing: 0;
		line-height: 1.15;
		margin: 0;
		padding: 0 0.25rem;
	}

	.settings-nav-search {
		align-items: center;
		background: color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft) 8%);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-secondary);
		display: flex;
		gap: 0.45rem;
		min-width: 0;
		padding: 0 0.55rem;
	}

	.settings-nav-search:focus-within {
		border-color: color-mix(in srgb, var(--accent-primary) 60%, var(--border-soft));
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent-primary) 16%, transparent);
	}

	.settings-nav-search svg {
		flex: none;
	}

	.settings-nav-search input {
		background: transparent;
		border: 0;
		color: var(--text-primary);
		flex: 1;
		font: inherit;
		font-size: 0.9rem;
		min-width: 0;
		outline: none;
		padding: 0.5rem 0;
	}

	.settings-nav-search input::placeholder {
		color: var(--text-secondary);
	}

	kbd {
		border: 1px solid var(--border-soft);
		border-radius: 4px;
		color: var(--text-secondary);
		font-family: var(--font-mono);
		font-size: 0.72rem;
		line-height: 1;
		padding: 0.15rem 0.35rem;
	}

	.settings-nav-summary {
		color: var(--text-secondary);
		font-size: 0.78rem;
		margin: -0.25rem 0 0;
		padding: 0 0.25rem;
	}

	.settings-nav-list {
		display: flex;
		flex-direction: column;
		gap: 0.15rem;
		list-style: none;
		margin: 0;
		padding: 0;
	}

	.settings-nav-list a {
		align-items: center;
		border: 1px solid transparent;
		border-radius: 6px;
		color: var(--text-secondary);
		display: flex;
		font-size: 0.88rem;
		font-weight: 600;
		gap: 0.5rem;
		justify-content: space-between;
		line-height: 1.25;
		padding: 0.5rem 0.6rem;
		text-decoration: none;
	}

	.settings-nav-list a:hover {
		background: color-mix(in srgb, var(--accent-primary) 7%, transparent);
		color: var(--text-primary);
	}

	.settings-nav-list a:focus-visible {
		outline: 2px solid color-mix(in srgb, var(--accent-primary) 70%, transparent);
		outline-offset: 1px;
	}

	.settings-nav-list a.active {
		background: color-mix(in srgb, var(--accent-primary) 14%, var(--bg-soft));
		border-color: color-mix(in srgb, var(--accent-primary) 30%, var(--border-soft));
		color: var(--text-primary);
	}

	.settings-nav-list a.empty {
		opacity: 0.45;
		pointer-events: none;
	}

	.settings-nav-count {
		color: var(--text-secondary);
		font-size: 0.74rem;
		font-variant-numeric: tabular-nums;
		font-weight: 700;
	}

	/* Narrow screens: a sticky top bar — title and search on one row, the
	   categories as a horizontally scrolling chip row underneath. */
	@media (max-width: 899px) {
		.settings-nav {
			background: color-mix(in srgb, var(--bg-card) 94%, transparent);
			-webkit-backdrop-filter: blur(12px);
			backdrop-filter: blur(12px);
			border-radius: 0;
			border-width: 0 0 1px;
			gap: 0.55rem;
			margin: 0 -1rem;
			max-height: none;
			overflow: visible;
			padding: 0.6rem 1rem;
			top: 0;
			z-index: 5;
		}

		.settings-nav-head {
			align-items: center;
			flex-direction: row;
			gap: 0.65rem;
		}

		h1 {
			flex: none;
			font-size: 1.15rem;
			padding: 0;
		}

		.settings-nav-search {
			flex: 1;
		}

		.settings-nav-search input {
			font-size: 1rem; /* iOS zooms into inputs under 16px */
			padding: 0.4rem 0;
		}

		kbd,
		.settings-nav-summary {
			display: none;
		}

		.settings-nav-list {
			flex-direction: row;
			gap: 0.4rem;
			margin: 0 -1rem;
			overflow-x: auto;
			overscroll-behavior-x: contain;
			padding: 0 1rem 0.1rem;
			scroll-padding-inline: 1rem;
			scrollbar-width: none;
		}

		.settings-nav-list::-webkit-scrollbar {
			display: none;
		}

		.settings-nav-list li {
			flex: none;
		}

		.settings-nav-list a {
			border-color: var(--border-soft);
			border-radius: 999px;
			font-size: 0.82rem;
			gap: 0.4rem;
			padding: 0.35rem 0.7rem;
			white-space: nowrap;
		}
	}

	.visually-hidden {
		border: 0;
		clip: rect(0 0 0 0);
		height: 1px;
		margin: -1px;
		overflow: hidden;
		padding: 0;
		position: absolute;
		white-space: nowrap;
		width: 1px;
	}
</style>
