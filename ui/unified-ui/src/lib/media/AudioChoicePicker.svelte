<script lang="ts">
	import { createEventDispatcher, tick } from 'svelte';

	// The same compact trigger, themed popup and checked rows as CodingEnginePicker.
	export let label: string;
	export let value = '';
	export let choices: { id: string; label: string; detail?: string; disabled?: boolean }[] = [];
	export let disabled = false;
	const dispatch = createEventDispatcher<{ select: { id: string } }>();
	const listId = `audio-choices-${Math.random().toString(36).slice(2)}`;
	let trigger: HTMLButtonElement;
	let panel: HTMLDivElement;
	let search: HTMLInputElement;
	let list: HTMLDivElement;
	let open = false;
	let query = '';
	let active = '';
	let left = 0;
	let top = 0;
	let width = 0;
	let maxHeight = 320;
	$: selected = choices.find((choice) => choice.id === value);
	$: filtered = choices.filter((choice) => `${choice.label} ${choice.id} ${choice.detail ?? ''}`.toLowerCase().includes(query.toLowerCase()));
	$: selectable = filtered.filter((choice) => !choice.disabled);
	$: if (open && !selectable.some((choice) => choice.id === active)) active = selectable[0]?.id ?? '';
	$: if (disabled) open = false;

	function close(restore = false): void {
		open = false;
		if (restore) trigger?.focus();
	}
	function place(): void {
		if (!trigger || !open) return;
		const rect = trigger.getBoundingClientRect();
		width = Math.min(Math.max(rect.width, 260), 384, window.innerWidth - 24);
		left = Math.max(12, Math.min(rect.left, window.innerWidth - width - 12));
		const below = Math.max(0, window.innerHeight - rect.bottom - 18);
		const above = Math.max(0, rect.top - 18);
		maxHeight = Math.min(384, Math.max(below, above));
		// Anchor the actual popup height, including short lists opening upwards.
		const height = Math.min(panel?.offsetHeight ?? 280, maxHeight);
		top = below >= Math.min(height, above) ? rect.bottom + 6 : Math.max(12, rect.top - height - 6);
	}
	async function toggle(): Promise<void> {
		if (open) { close(); return; }
		query = '';
		active = selected && !selected.disabled ? value : choices.find((choice) => !choice.disabled)?.id ?? '';
		open = true;
		place();
		await tick();
		place();
		(search ?? list)?.focus();
		panel?.querySelector('[aria-selected="true"]')?.scrollIntoView?.({ block: 'nearest' });
	}
	function choose(id: string): void {
		if (!selectable.some((choice) => choice.id === id)) return;
		dispatch('select', { id });
		close(true);
	}
	function keydown(event: KeyboardEvent): void {
		if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close(true); }
		else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
			event.preventDefault();
			const index = selectable.findIndex((choice) => choice.id === active);
			active = selectable[(index + (event.key === 'ArrowDown' ? 1 : -1) + selectable.length) % selectable.length]?.id ?? '';
			void tick().then(() => document.getElementById(`${listId}-${active}`)?.scrollIntoView?.({ block: 'nearest' }));
		} else if (event.key === 'Enter' || (event.key === ' ' && event.target === list)) {
			event.preventDefault(); choose(active);
		} else if (event.key === 'Tab') close();
	}
	function outside(event: PointerEvent): void {
		if (open && !trigger?.contains(event.target as Node) && !panel?.contains(event.target as Node)) close();
	}
	function portal(node: HTMLDivElement): { destroy: () => void } {
		// Keep the popup above the scrolling voice panel and preserve its theme.
		const style = getComputedStyle(trigger);
		for (const name of ['--vibe-text', '--vibe-text-muted', '--vibe-surface', '--vibe-border', '--vibe-accent', '--text-primary', '--text-secondary', '--bg-card', '--bg-elevated', '--border-soft', '--accent-primary', '--bg-soft', '--font-primary']) {
			node.style.setProperty(name, style.getPropertyValue(name));
		}
		document.body.appendChild(node);
		const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(place);
		observer?.observe(node);
		window.addEventListener('scroll', place, true);
		return { destroy: () => { observer?.disconnect(); window.removeEventListener('scroll', place, true); node.remove(); } };
	}
</script>

<svelte:document on:pointerdown={outside} />
<svelte:window on:resize={() => close()} />
<button type="button" class="choice-trigger" role="combobox" aria-label={label}
	aria-haspopup="listbox" aria-expanded={open} aria-controls={listId}
	{disabled} bind:this={trigger} on:click={toggle}>
	<span>{selected?.label ?? (value || 'Choose…')}</span>
	<svg width="12" height="12" viewBox="0 0 24 24" aria-hidden="true"><path d="m6 9 6 6 6-6" fill="none" stroke="currentColor" stroke-width="2.4" /></svg>
</button>
{#if open}
	<div class="choice-panel" bind:this={panel} use:portal style:left={`${left}px`} style:top={`${top}px`} style:width={`${width}px`} style:max-height={`${maxHeight}px`} on:keydown={keydown} on:click={(event) => event.stopPropagation()} role="presentation">
		{#if choices.length > 10}
			<input type="search" bind:this={search} bind:value={query} aria-label={`Search ${label}`} placeholder={`Search ${label.toLowerCase()}…`} aria-controls={listId} aria-activedescendant={`${listId}-${active}`} />
		{/if}
		<!-- svelte-ignore a11y-no-noninteractive-tabindex -->
		<div role="listbox" id={listId} aria-label={label} tabindex="0" bind:this={list} aria-activedescendant={`${listId}-${active}`}>
			{#each filtered as choice (choice.id)}
				<button type="button" role="option" tabindex="-1" id={`${listId}-${choice.id}`} aria-selected={value === choice.id} aria-disabled={choice.disabled ?? false}
					class:active={active === choice.id} class:selected={value === choice.id} class="choice-row"
					on:pointerenter={() => { if (!choice.disabled) active = choice.id; }} on:click={() => choose(choice.id)}>
					<span><strong>{choice.label}</strong>{#if choice.detail}<small>{choice.detail}</small>{/if}</span>
					{#if value === choice.id}<span class="check" aria-hidden="true">✓</span>{/if}
				</button>
			{:else}<p class="empty">No matches</p>{/each}
		</div>
	</div>
{/if}

<style>
	.choice-trigger { display: flex; align-items: center; gap: .35rem; width: 100%; min-width: 0; height: 1.74rem; padding: 0 .5rem; border: 0; border-radius: 7px; background: var(--bg-soft, color-mix(in srgb, var(--vibe-text, var(--text-primary)) 5%, transparent)); color: var(--vibe-text, var(--text-primary)); font: inherit; font-size: .76rem; font-weight: 700; cursor: pointer; }
	.choice-trigger > span { flex: 1; min-width: 0; text-align: left; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.choice-trigger > svg { flex-shrink: 0; }
	.choice-trigger:disabled { opacity: .58; cursor: not-allowed; }
	.choice-trigger:focus-visible, .choice-panel :focus-visible { outline: 2px solid var(--vibe-accent, var(--accent-primary, #6b8afd)); outline-offset: 1px; }
	.choice-panel { position: fixed; z-index: 10050; display: flex; flex-direction: column; box-sizing: border-box; padding: .3rem; border: 1px solid var(--vibe-border, var(--border-soft)); border-radius: 10px; background: var(--vibe-surface, var(--bg-card, #fff)); color: var(--vibe-text, var(--text-primary)); box-shadow: 0 10px 30px #0003; font-family: var(--font-primary, inherit); }
	.choice-panel input { flex-shrink: 0; width: 100%; box-sizing: border-box; padding: .5rem; margin-bottom: .25rem; border: 1px solid var(--vibe-border, var(--border-soft)); border-radius: 7px; background: var(--bg-elevated, transparent); color: inherit; font: inherit; font-size: .78rem; }
	[role='listbox'] { overflow-y: auto; min-height: 0; overscroll-behavior: contain; }
	.choice-row { display: flex; align-items: center; gap: .35rem; width: 100%; padding: .38rem .45rem; border: 0; border-radius: 7px; background: transparent; color: inherit; text-align: left; cursor: pointer; font: inherit; font-size: .78rem; }
	.choice-row > span:first-child { flex: 1; min-width: 0; overflow-wrap: anywhere; }
	strong { font-weight: 650; }
	small { display: block; margin-top: .12rem; font-size: .7rem; color: var(--vibe-text-muted, var(--text-secondary)); }
	.choice-row.active { background: var(--bg-soft, color-mix(in srgb, var(--vibe-text, var(--text-primary)) 7%, transparent)); }
	.choice-row[aria-disabled='true'] { opacity: .55; cursor: not-allowed; }
	.check { color: var(--vibe-accent, var(--accent-primary)); }
	.empty { padding: .5rem; font-size: .78rem; color: var(--text-secondary); }
</style>
