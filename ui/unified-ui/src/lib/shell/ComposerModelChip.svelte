<script lang="ts">
	import ProfileSelectionBurst from '$lib/magician/components/chat/ProfileSelectionBurst.svelte';
	import type { ModelFeedback } from '$lib/llm/modelCatalog';

	export let engine: string | null = null;
	export let model: string | null = null;
	export let modelIdentifier: string | null = null;
	export let feedback: ModelFeedback | null = null;
	export let thinking: string | null = null;
	export let expanded = false;
	export let disabled = false;
	export let ariaLabel: string;
	export let title = '';
	export let triggerElement: HTMLButtonElement | null = null;
</script>

<button type="button" class="profile" class:automatic={thinking === 'Auto'} bind:this={triggerElement}
	{disabled} {title} aria-label={ariaLabel} aria-haspopup="dialog" aria-expanded={expanded} on:click>
	<svg class="pp-icon" width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
		<path d="M12 3l1.9 4.9L19 10l-4.9 1.9L12 17l-1.9-5.1L5 10l5.1-2L12 3z" />
		<path d="M5 19l.7-1.7L7 16.5l-1.3-.8L5 14l-.7 1.7L3 16.5l1.3.8L5 19z" />
	</svg>
	{#if engine}<span class="pp-engine">{engine}</span>{/if}
	{#if thinking}<span class="pp-badge" title={`Thinking: ${thinking}`}>{thinking}</span>{/if}
	{#if model}<span class="pp-model">{model}</span>{/if}
	<svg class="pp-chevron" width="8" height="8" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><polyline points="6 9 12 15 18 9"/></svg>
	<ProfileSelectionBurst profileName={model} profileModel={modelIdentifier} {feedback} />
	<slot />
</button>

<style>
	/* Sparkles glyph leads the icon-only profile chip — visible on
	   desktop AND mobile. The verbose profile name now lives in the
	   button's `title` tooltip; the badge + model tag carry the
	   at-a-glance identity (e.g. "Auto · GPT Sol"). */
	/* The chip's children all set their own font-size, so they do NOT
	   inherit `.profile`'s. Every one of them has to be sized explicitly or
	   the box shrinks while the text inside stays put. Same for the two
	   inline SVGs, whose width/height attributes need a CSS override. */
	.profile .pp-icon {
		flex-shrink: 0;
		color: var(--accent-primary);
		width: 10px;
		height: 10px;
	}

	.profile .pp-chevron {
		flex-shrink: 0;
		width: 8px;
		height: 8px;
	}

	.profile {
		position: relative;
		display: inline-flex;
		align-items: center;
		gap: 4px;
		padding: 2px 6px;
		border-radius: 5px;
		font-family: var(--font-primary);
		font-size: 9.5px;
		line-height: 14px;
		max-width: 100%;
		color: var(--text-secondary, #444);
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		border: 0;
		cursor: pointer;
		transition: background 0.15s ease;
		min-width: 0;
		overflow: visible;
	}

	.profile:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.05));
	}

	.profile .pp-model {
		font-family: var(--font-mono);
		font-size: 9.5px;
		color: var(--text-muted, #888);
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.profile .pp-engine {
		font-weight: 650;
		white-space: nowrap;
	}

	.profile .pp-badge {
		/* Chips use the themed display font (Fredoka in the default
		   theme, swapped per theme via the same `--font-display` var)
		   so they pick up the same branding as headings + nav rather
		   than inheriting the body font and reading as system-default
		   at this small size. */
		font-family: var(--font-display);
		font-size: 8.5px;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		padding: 0 4px;
		border-radius: 3px;
		background: var(--accent-soft, rgba(194, 80, 42, 0.12));
		color: var(--accent-primary, #c2502a);
		border: 1px solid var(--accent-border, rgba(194, 80, 42, 0.28));
		flex-shrink: 0;
	}

	.profile.automatic {
		background: var(--accent-soft, rgba(194, 80, 42, 0.06));
	}

	.profile:disabled { opacity: .5; cursor: not-allowed; }
	.profile:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 2px; }
	@media (max-width: 767px) {
		.profile .pp-badge, .profile .pp-model, .profile .pp-engine, .profile .pp-chevron { display: none; }
		.profile .pp-icon { width: 16px; height: 16px; }
		.profile { width: 40px; height: 40px; min-width: 40px; padding: 0; justify-content: center; gap: 0; }
	}
</style>
