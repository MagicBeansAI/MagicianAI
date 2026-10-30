<script lang="ts">
	/**
	 * The ⚡ Act button and its menu, shared by the Morning Brief deck and the
	 * Broadsheet cards. It only lists and reports the chosen entry; the host
	 * runs it (see `deckActMenu.ts`). "Open in <provider>" is a plain link.
	 */
	import { createEventDispatcher, tick } from 'svelte';

	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { DeckActEntry } from './deckActMenu';

	export let entries: DeckActEntry[] = [];
	/** `today-deck-act` → `today-deck-act`, `today-deck-act-menu`, `today-deck-act-<entry id>`. */
	export let testIdPrefix = 'today-act';
	export let open = false;
	export let disabled = false;
	/** The deck sits at the page bottom and opens upward; cards open downward. */
	export let placement: 'up' | 'down' = 'down';
	export let title = 'Choose an action';
	/** Trigger size: the deck's large control or a Broadsheet card button. */
	export let variant: 'deck' | 'card' = 'card';

	const dispatch = createEventDispatcher<{ choose: DeckActEntry }>();
	const menuId = `today-act-menu-${Math.random().toString(36).slice(2, 10)}`;

	let wrapEl: HTMLElement | null = null;
	let triggerEl: HTMLButtonElement | null = null;
	let menuEl: HTMLElement | null = null;

	$: if (open && (disabled || entries.length === 0)) open = false;

	function menuItems(): HTMLElement[] {
		return menuEl ? Array.from(menuEl.querySelectorAll<HTMLElement>('[role="menuitem"]')) : [];
	}

	export async function openMenu(): Promise<void> {
		if (disabled || entries.length === 0) return;
		open = true;
		await tick();
		menuItems()[0]?.focus();
	}

	export function closeMenu(restoreFocus = false): void {
		if (!open) return;
		open = false;
		if (restoreFocus) triggerEl?.focus();
	}

	function toggle(): void {
		if (open) closeMenu(true);
		else void openMenu();
	}

	function moveFocus(delta: number): void {
		const items = menuItems();
		if (items.length === 0) return;
		const current = items.indexOf(document.activeElement as HTMLElement);
		const next = current < 0 ? (delta > 0 ? 0 : items.length - 1) : (current + delta + items.length) % items.length;
		items[next]?.focus();
	}

	function choose(entry: DeckActEntry): void {
		closeMenu(false);
		dispatch('choose', entry);
	}

	function handleWindowClick(event: MouseEvent): void {
		if (!open || wrapEl?.contains(event.target as Node)) return;
		closeMenu(false);
	}

	// Enter / Space activate the focused item natively.
	function handleWindowKeydown(event: KeyboardEvent): void {
		if (!open) return;
		if (event.key === 'Escape') {
			event.preventDefault();
			closeMenu(true);
		} else if (event.key === 'ArrowDown') {
			event.preventDefault();
			moveFocus(1);
		} else if (event.key === 'ArrowUp') {
			event.preventDefault();
			moveFocus(-1);
		}
	}
</script>

<svelte:window on:click={handleWindowClick} on:keydown={handleWindowKeydown} />

{#if entries.length > 0}
	<div class="today-act" bind:this={wrapEl}>
		<button
			type="button"
			class="today-act-trigger"
			class:today-act-trigger--deck={variant === 'deck'}
			data-testid={testIdPrefix}
			{title}
			{disabled}
			aria-haspopup="menu"
			aria-expanded={open}
			aria-controls={open ? menuId : undefined}
			bind:this={triggerEl}
			on:click={toggle}
		>
			<span aria-hidden="true">⚡</span> <span>Act</span>
		</button>
		{#if open}
			<div
				id={menuId}
				class="today-act-menu"
				class:today-act-menu--up={placement === 'up'}
				role="menu"
				aria-label="Actions for this card"
				data-testid={`${testIdPrefix}-menu`}
				bind:this={menuEl}
			>
				{#each entries as entry (entry.id)}
					{#if entry.kind === 'open_provider'}
						<a
							class="today-act-item"
							role="menuitem"
							href={entry.href}
							target="_blank"
							rel="noopener noreferrer"
							data-testid={`${testIdPrefix}-${entry.id}`}
							on:click={() => choose(entry)}
						>
							<span>{entry.label}</span>
							<Icon name="arrow-up-right" size={12} />
						</a>
					{:else}
						<button
							type="button"
							class="today-act-item"
							role="menuitem"
							data-testid={`${testIdPrefix}-${entry.id}`}
							on:click={() => choose(entry)}
						>
							{#if entry.kind === 'channel_action' && entry.icon}
								<span aria-hidden="true">{entry.icon}</span>
							{/if}
							<span>{entry.label}</span>
						</button>
					{/if}
				{/each}
			</div>
		{/if}
	</div>
{/if}

<style>
	.today-act {
		position: relative;
		display: inline-flex;
	}

	.today-act-trigger {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		font-family: var(--font-primary, sans-serif);
		font-size: 0.78rem;
		font-weight: 600;
		line-height: 1;
		padding: 0.38rem 0.65rem;
		border-radius: var(--radius-sm, 6px);
		border: 1px solid transparent;
		background: var(--accent-fill, #b45309);
		color: #ffffff;
		cursor: pointer;
		white-space: nowrap;
		transition: background 0.15s ease, transform 0.12s ease;
	}

	.today-act-trigger:hover:not(:disabled) {
		background: var(--accent-fill-hover);
	}

	.today-act-trigger:disabled {
		opacity: 0.6;
		cursor: default;
	}

	.today-act-trigger--deck {
		gap: 0.45rem;
		font-size: 0.88rem;
		padding: 0.65rem 1.15rem;
		border-radius: var(--radius-md, 8px);
		box-shadow: 0 2px 4px rgba(0, 0, 0, 0.04);
	}

	.today-act-trigger--deck:active {
		transform: scale(0.96);
	}

	.today-act-menu {
		position: absolute;
		left: 0;
		top: calc(100% + 0.4rem);
		z-index: 20;
		min-width: 14rem;
		max-width: min(20rem, calc(100vw - 2rem));
		max-height: 18rem;
		overflow-y: auto;
		display: grid;
		padding: 0.25rem;
		background: var(--bg-card, #ffffff);
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: var(--radius-md, 8px);
		box-shadow: 0 8px 24px rgba(0, 0, 0, 0.16);
		user-select: none;
	}

	.today-act-menu--up {
		left: auto;
		right: 0;
		top: auto;
		bottom: calc(100% + 0.4rem);
	}

	.today-act-item {
		display: flex;
		align-items: center;
		gap: 0.45rem;
		width: 100%;
		padding: 0.5rem 0.65rem;
		border: 0;
		border-radius: 6px;
		background: transparent;
		font-family: var(--font-primary, sans-serif);
		font-size: 0.85rem;
		font-weight: 400;
		text-align: left;
		text-decoration: none;
		color: var(--text-primary, #18181b);
		cursor: pointer;
		box-sizing: border-box;
	}

	.today-act-item:hover,
	.today-act-item:focus-visible {
		background: color-mix(in srgb, var(--accent-primary, #b45309) 10%, transparent);
		outline: none;
	}
</style>
