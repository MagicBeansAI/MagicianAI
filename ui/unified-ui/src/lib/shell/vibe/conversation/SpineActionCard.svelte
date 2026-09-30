<script lang="ts" context="module">
	import type { SpineCard } from '$lib/shell/vibe/conversation/spineModel';

	/** Absolute wall-clock label for the expanded card meta line (date + time).
	 *  Module-context export: ConversationSpine reuses these for the other card
	 *  kinds' shared meta footer, so the formatting has a single source. */
	export function fmtTime(ms: number | null | undefined): string {
		if (!ms || !Number.isFinite(ms)) return '';
		return new Date(ms).toLocaleString(undefined, {
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		});
	}

	/** Latency = the card's true [start, end] span (0 for instant single-event cards). */
	export function cardDurationMs(card: SpineCard): number {
		const start = card.startTs ?? card.ts;
		const end = card.endTs ?? card.ts;
		return Math.max(0, end - start);
	}

	export function fmtDuration(ms: number): string {
		if (ms < 1000) return `${Math.round(ms)}ms`;
		if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
		const m = Math.floor(ms / 60_000);
		const s = Math.round((ms % 60_000) / 1000);
		return `${m}m ${s}s`;
	}
</script>

<script lang="ts">
	/**
	 * One tool-action (or red→green test) card in the conversation spine —
	 * collapsed head (icon · title · one-line preview · status glyph) expanding
	 * to args/result + a when/how-long meta footer. Single source for the two
	 * places the spine renders action cards: the main timeline and the expanded
	 * members of a folded action strip (which previously duplicated this markup
	 * and drifted). Expansion state stays in the parent (keyed by card id), so
	 * it survives re-derivation of the grouped view.
	 */
	import { createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { IconName } from '$lib/shared/icons/paths';
	import CodeBlock from '$lib/magician/components/generative/CodeBlock.svelte';
	import { cardPreview } from '$lib/shell/vibe/conversation/spineModel';

	export let card: SpineCard;
	export let expanded = false;

	const dispatch = createEventDispatcher<{ toggle: { id: string } }>();

	$: preview = cardPreview(card);

	function toolIcon(toolName: string | null | undefined): IconName {
		const name = (toolName ?? '').toLowerCase();
		if (/read|cat|view|open/.test(name)) return 'eye';
		if (/grep|search|find|ripgrep|rg/.test(name)) return 'search';
		if (/edit|write|patch|apply|create/.test(name)) return 'pencil';
		if (/bash|shell|run|exec|command/.test(name)) return 'chevron-right';
		if (/test|vitest|jest|pytest|cargo/.test(name)) return 'check';
		if (/delete|rm|remove/.test(name)) return 'x';
		// Generic-action fallback — 'zap' is reserved for the autopilot badge.
		return 'dots-horizontal';
	}

	// Status glyphs (✓ ✗ ⏸) render as TEXT deliberately — typographic, inherit
	// font/color; interactive/semantic icons use <Icon>.
	function statusGlyph(c: SpineCard): string {
		if (c.status === 'failed') return '✗';
		if (c.status === 'done') return '✓';
		if (c.status === 'waiting') return '⏸';
		return '';
	}
</script>

<article class="card card--{card.kind} is-{card.status}" aria-label={`${card.kind}: ${card.title}`}>
	<button
		type="button"
		class="card__action-head"
		aria-expanded={expanded}
		on:click={() => dispatch('toggle', { id: card.id })}
	>
		<span class="card__icon" aria-hidden="true"><Icon name={toolIcon(card.toolName)} size={14} /></span>
		<span class="card__title">{card.title}</span>
		{#if !expanded && preview}
			<span class="card__preview">{preview}</span>
		{/if}
		{#if statusGlyph(card)}<span class="card__status-glyph">{statusGlyph(card)}</span>{/if}
		{#if card.status === 'running'}<span class="card__spinner" aria-hidden="true"></span>{/if}
		<span class="card__chev" class:open={expanded} aria-hidden="true">▸</span>
	</button>
	{#if expanded}
		{#if card.args}
			<div class="card__io"><span class="card__io-label">args</span><CodeBlock code={card.args} language="json" maxHeight="14rem" /></div>
		{/if}
		{#if card.result}
			<div class="card__io"><span class="card__io-label">result</span><CodeBlock code={card.result} language="plaintext" maxHeight="20rem" /></div>
		{/if}
		<div class="card__meta">
			<span class="card__meta-time">{fmtTime(card.startTs ?? card.ts)}</span>
			{#if cardDurationMs(card) > 0}
				<span class="card__meta-sep" aria-hidden="true">·</span>
				<span class="card__meta-dur">took {fmtDuration(cardDurationMs(card))}</span>
			{/if}
		</div>
	{/if}
</article>

<style>
	/* Mirrors ConversationSpine's `.card--action` / `.card--test` treatment —
	   this component only ever renders those two kinds. */
	.card {
		border: 1px solid var(--vibe-border);
		border-radius: var(--radius-md, 18px);
		background: color-mix(in srgb, var(--vibe-page-surface) 60%, var(--vibe-surface));
		padding: 0.5rem 0.7rem;
		animation: card-in 0.24s var(--ease-settle, cubic-bezier(0.16, 1, 0.3, 1));
	}
	.card.is-failed {
		border-color: color-mix(in srgb, var(--vibe-error) 45%, var(--vibe-border));
		background: color-mix(in srgb, var(--vibe-error) 6%, var(--vibe-surface));
	}

	.card__action-head {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		width: 100%;
		border: 0;
		background: transparent;
		color: inherit;
		font: inherit;
		text-align: left;
		cursor: pointer;
		padding: 0;
	}
	.card__icon {
		display: inline-grid;
		place-items: center;
		width: 1.25rem;
		font-size: 0.85rem;
		color: var(--vibe-text-muted);
	}
	.card__title {
		font-family: var(--font-display, inherit);
		font-weight: 600;
		font-size: 0.9rem;
		color: var(--vibe-text);
	}
	.card__chev {
		margin-left: auto;
		color: var(--vibe-text-muted);
		transition: transform 0.15s var(--ease-settle, ease);
	}
	.card__chev.open {
		transform: rotate(90deg);
	}
	.card__status-glyph {
		font-weight: 700;
		font-size: 0.85rem;
	}
	.is-done .card__status-glyph {
		color: var(--vibe-success);
	}
	.is-failed .card__status-glyph {
		color: var(--vibe-error);
	}

	/* Collapsed one-line content preview (command / path). `flex:1; min-width:0`
	   lets it absorb free space + ellipsize, so the chevron's `margin-left:auto`
	   stays pinned right. */
	.card__preview {
		flex: 1;
		min-width: 0;
		font-family: var(--font-mono, monospace);
		font-size: 0.74rem;
		line-height: 1.3;
		color: var(--vibe-text-muted);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		text-align: left;
	}

	.card__io {
		margin-top: 0.45rem;
	}
	.card__io-label {
		display: block;
		font-size: var(--text-2xs);
		font-weight: 700;
		letter-spacing: 0.04em;
		color: var(--vibe-text-muted);
		margin-bottom: 0.2rem;
	}

	/* Expanded-card footer: when it happened + how long it took. */
	.card__meta {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		margin-top: 0.4rem;
		font-size: var(--text-2xs);
		color: var(--vibe-text-muted);
		opacity: 0.85;
	}
	.card__meta-time {
		font-variant-numeric: tabular-nums;
	}
	.card__meta-dur {
		font-variant-numeric: tabular-nums;
	}

	.card__spinner {
		width: 0.7rem;
		height: 0.7rem;
		border-radius: 999px;
		border: 2px solid currentColor;
		border-right-color: transparent;
		animation: spine-spin 0.7s linear infinite;
		color: var(--vibe-accent);
	}

	@keyframes card-in {
		from {
			opacity: 0;
			transform: translateY(6px);
		}
		to {
			opacity: 1;
			transform: translateY(0);
		}
	}
	@keyframes spine-spin {
		to {
			transform: rotate(360deg);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.card,
		.card__spinner {
			animation: none;
		}
	}
</style>
