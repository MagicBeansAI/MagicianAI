<script lang="ts">
	import { createEventDispatcher, tick } from 'svelte';
	import { createMenuKeydown } from '$lib/shared/menuKeydown';
	export let label: string;
	export let value: string | null = null;
	export let options: { id: string; label: string; group?: string; disabled?: boolean; note?: string; title?: string; search?: string }[] = [];
	export let placeholder = 'Choose…';
	export let disabled = false;
	export let inline = false;
	const dispatch = createEventDispatcher<{ change: string }>();
	const panelId = `choice-options-${Math.random().toString(36).slice(2, 8)}`;
	let open = false;
	let search = '';
	let root: HTMLDivElement;
	let panel: HTMLDivElement;
	let trigger: HTMLButtonElement;
	$: selected = options.find(option => option.id === value);
	$: filtered = options.filter(option => `${option.label} ${option.group ?? ''} ${option.search ?? ''}`.toLowerCase().includes(search.toLowerCase().trim()));
	$: groups = [...new Set(filtered.map(option => option.group ?? ''))];
	function close(): void { open = false; trigger?.focus(); }
	const navigate = createMenuKeydown({ getMenuEl: () => panel, itemSelector: '.choice-row:not(:disabled)', close });
	async function toggle(): Promise<void> {
		open = !open;
		search = '';
		if (open) {
			await tick();
			(panel?.querySelector<HTMLElement>('[aria-pressed="true"]') ?? panel?.querySelector<HTMLElement>('.choice-row:not(:disabled)'))?.focus();
		}
	}
	function choose(id: string): void { dispatch('change', id); if (!inline) close(); }
	function keydown(event: KeyboardEvent): void {
		if (event.key === 'Escape' && inline) return;
		navigate(event);
		if (event.defaultPrevented) event.stopPropagation();
	}
</script>

<svelte:window on:pointerdown={event => { if (open && !root?.contains(event.target as Node)) open = false; }} />
<div class="choice-dropdown" bind:this={root}>
	<span class="field-label">{label}</span>
	{#if !inline}
		<button type="button" class="choice-trigger" bind:this={trigger} {disabled} aria-label={label} aria-expanded={open} aria-controls={panelId} on:click={toggle}>
			<span>{selected?.label ?? placeholder}</span>
			<svg class="choice-chevron" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m6 9 6 6 6-6" /></svg>
		</button>
	{/if}
	{#if inline || open}
		<!-- Keyboard navigation is delegated from the focusable option buttons. -->
		<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
		<div class="choice-panel" id={panelId} role="region" aria-label={`${label} choices`} bind:this={panel} on:keydown={keydown}>
			{#if options.length > 6}<input type="search" aria-label={`Search ${label.toLowerCase()}s`} placeholder={`Search ${label.toLowerCase()}s…`} bind:value={search} {disabled} />{/if}
			<div class="choice-list">
				{#each groups as group}
					{#if group}<div class="group-label">{group}</div>{/if}
					{#each filtered.filter(option => (option.group ?? '') === group) as option (option.id)}
						<button type="button" class="choice-row" class:selected={option.id === value} aria-label={`${option.label}${option.note ? ` · ${option.note}` : ''}`} aria-pressed={option.id === value} disabled={disabled || option.disabled} title={option.title} on:click={() => choose(option.id)}>
							<span class="row-label">{option.label}</span>{#if option.note}<small>{option.note}</small>{/if}{#if option.id === value}<span class="check" aria-hidden="true">✓</span>{/if}
						</button>
					{/each}
				{/each}
				{#if !filtered.length}<div class="empty">No matching choices</div>{/if}
			</div>
		</div>
	{/if}
</div>

<style>
	.choice-dropdown { display: grid; gap: .2rem; min-width: 0; width: 100%; }
	.field-label, .group-label { font-size: .63rem; font-weight: 650; color: var(--text-muted, #8f9799); }
	.group-label { padding: .35rem .45rem .15rem; text-transform: uppercase; letter-spacing: .06em; }
	button { font: inherit; color: inherit; cursor: pointer; }
	.choice-trigger { display: flex; align-items: center; justify-content: space-between; gap: .5rem; box-sizing: border-box; width: 100%; min-height: 26px; padding: 3px 8px; border: 1px solid var(--border-soft, #8884); border-radius: 6px; background: var(--bg-soft, #8881); text-align: left; font-size: .74rem; line-height: 16px; }
	.choice-chevron { display: block; flex: 0 0 12px; align-self: center; }
	.choice-trigger > span:first-child, .row-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
	.choice-panel { min-width: 0; overflow: hidden; border: 1px solid var(--border-soft, #8883); border-radius: 6px; padding: .2rem; }
	input { box-sizing: border-box; width: 100%; padding: .35rem .45rem; margin-bottom: .2rem; border: 0; border-bottom: 1px solid var(--border-soft, #8883); font: inherit; font-size: .75rem; color: inherit; background: var(--bg-soft, #8881); }
	.choice-list { max-height: min(13rem, 28vh); overflow-y: auto; overscroll-behavior: contain; }
	.choice-row { display: flex; align-items: center; gap: .4rem; width: 100%; min-height: 1.75rem; padding: .25rem .45rem; border: 0; border-radius: 4px; background: none; text-align: left; font-size: .75rem; line-height: 1.3; }
	.choice-row:hover { background: var(--bg-soft, #8881); }
	.choice-row.selected { background: var(--accent-primary-soft, #6b8afd22); }
	.row-label { flex: 1; }
	small, .empty { font-size: .65rem; color: var(--text-muted, #8f9799); }
	.empty { padding: .45rem; }
	.check { color: var(--accent-primary, #6b8afd); }
	button:focus-visible, input:focus-visible { outline: 2px solid var(--accent-primary, #6b8afd); outline-offset: -2px; }
	button:disabled { opacity: .5; cursor: not-allowed; }
	@media (pointer: coarse) { .choice-trigger { min-height: 36px; } }
</style>
