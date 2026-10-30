<script lang="ts">
	/**
	 * The composer's `/` command list. Presentational only — FloatingComposer
	 * owns the state machine (see `composerCommands.ts`) and docks this where
	 * the `@` picker docks, with the same look, keyboard and tap behaviour.
	 */
	import { createEventDispatcher } from 'svelte';
	import type { SlashView, SlashItem } from './composerCommands';

	export let view: SlashView;
	export let activeIndex = 0;
	/** True below the command list: Backspace / Escape step back a level. */
	export let canGoBack = false;

	const dispatch = createEventDispatcher<{ select: SlashItem; back: void }>();
</script>

<div class="slash-picker" role="listbox" aria-label="Slash commands" data-testid="slash-picker">
	<div class="slash-head">
		{#if canGoBack}
			<button
				type="button"
				class="slash-back"
				aria-label="Back"
				title="Back — Backspace or Esc"
				on:mousedown|preventDefault
				on:click={() => dispatch('back')}
			>‹</button>
		{/if}
		<span class="slash-title">{view.title}</span>
		<span class="slash-hint" aria-hidden="true">↵ choose{canGoBack ? ' · ⌫ back' : ''} · esc</span>
	</div>
	{#if view.summary}
		<div class="slash-summary" data-testid="slash-summary">{view.summary}</div>
	{/if}
	{#if view.note}
		<div class="slash-note" role="status">{view.note}</div>
	{/if}
	{#if view.items.length > 0}
		{#each view.items as item, index (item.id)}
			<button
				type="button"
				class="slash-option"
				class:active={index === activeIndex}
				class:disabled={item.disabled}
				role="option"
				aria-selected={index === activeIndex}
				aria-current={item.current ? 'true' : undefined}
				aria-disabled={item.disabled ? 'true' : undefined}
				on:mousedown|preventDefault
				on:click={() => dispatch('select', item)}
			>
				<span class="slash-badge">{item.badge}</span>
				<span class="slash-copy">
					<span class="slash-label">{item.label}</span>
					{#if item.detail}
						<span class="slash-detail">{item.detail}</span>
					{/if}
				</span>
				{#if item.current}
					<span class="slash-mark" aria-label="Current">✓</span>
				{:else if item.next}
					<span class="slash-mark slash-mark--next" aria-hidden="true">›</span>
				{/if}
			</button>
		{/each}
	{:else}
		<div class="slash-empty">{view.emptyText}</div>
	{/if}
</div>

<style>
	.slash-picker {
		padding: 6px;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: 10px;
		background: color-mix(in srgb, var(--bg-elevated, #fff) 92%, var(--accent-primary, #c2502a) 8%);
		box-shadow: var(--shadow-md, 0 14px 32px -18px rgba(0, 0, 0, 0.35));
		max-height: 300px;
		overflow: auto;
	}

	.slash-head {
		display: flex;
		align-items: center;
		gap: 6px;
		padding: 2px 6px 6px;
		font-family: var(--font-primary);
		font-size: 11.5px;
		color: var(--text-muted, #888);
	}

	.slash-back {
		border: 0;
		background: transparent;
		color: var(--text-muted, #888);
		font-size: 16px;
		line-height: 1;
		padding: 0 4px;
		cursor: pointer;
		border-radius: 5px;
	}

	.slash-back:hover {
		background: var(--bg-soft, rgba(0, 0, 0, 0.06));
		color: var(--text-primary, #1a1a1a);
	}

	.slash-title {
		font-weight: 650;
		color: var(--text-secondary, var(--text-primary, #1a1a1a));
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		min-width: 0;
	}

	.slash-hint {
		margin-left: auto;
		font-family: var(--font-mono);
		font-size: 10px;
		white-space: nowrap;
	}

	.slash-summary {
		margin: 0 6px 6px;
		font-family: var(--font-primary);
		font-size: 11.5px;
		line-height: 1.3;
		color: var(--text-secondary, var(--text-muted, #888));
	}

	.slash-note {
		margin: 0 6px 6px;
		padding: 6px 8px;
		border-radius: 7px;
		font-family: var(--font-primary);
		font-size: 11.5px;
		color: var(--status-warning, #9a6700);
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
	}

	.slash-option {
		width: 100%;
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: center;
		gap: 10px;
		padding: 8px 9px;
		border: 0;
		border-radius: 7px;
		background: transparent;
		color: var(--text-primary, #1a1a1a);
		text-align: left;
		cursor: pointer;
	}

	.slash-option:hover,
	.slash-option.active {
		background: var(--bg-soft, rgba(0, 0, 0, 0.06));
	}

	.slash-option.disabled {
		cursor: default;
		opacity: 0.55;
	}

	.slash-badge {
		min-width: 70px;
		text-align: center;
		font-family: var(--font-mono);
		font-size: 10px;
		line-height: 1;
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--accent-primary, #c2502a);
		border: 1px solid var(--accent-border, rgba(194, 80, 42, 0.28));
		border-radius: var(--radius-chip);
		background: var(--accent-soft, rgba(194, 80, 42, 0.1));
		padding: 4px 7px;
	}

	.slash-copy {
		display: grid;
		gap: 2px;
		min-width: 0;
	}

	.slash-label {
		font-family: var(--font-primary);
		font-size: 13px;
		font-weight: 650;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.slash-detail {
		font-family: var(--font-primary);
		font-size: 11.5px;
		line-height: 1.25;
		color: var(--text-muted, #888);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.slash-mark {
		font-size: 13px;
		font-weight: 700;
		color: var(--accent-primary, #c2502a);
	}

	.slash-mark--next {
		color: var(--text-muted, #888);
		font-weight: 400;
	}

	.slash-empty {
		padding: 10px;
		font-family: var(--font-primary);
		font-size: 12px;
		color: var(--text-muted, #888);
	}
</style>
