<script lang="ts">
	/**
	 * VibeDev Empty State — mirrors Chat's beautiful empty state (greeting,
	 * atmospheric glow, tinted capability cards, and recent builds pick-up).
	 */
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { IconName } from '$lib/shared/icons/paths';
	import { formatRelativeTime } from '$lib/shared/formatRelativeTime';
	import { tintVars } from '$lib/magician/chat/next/tint';
	import { rise } from '$lib/magician/chat/next/motion';
	import { stripRunTitlePrefix } from '$lib/shell/vibe/conversation/submit';

	import {
		VIBE_EMPTY_SUGGESTIONS,
		type VibeEmptySuggestion
	} from './vibeEmptySuggestions';

	export let activeProjectName: string | null = null;
	export let recentRuns: Array<{
		id: string;
		title: string;
		updatedAt?: string;
		createdAt?: string;
	}> = [];
	export let onSuggest: (prompt: string) => void;
	export let onSelectRun: (taskId: string) => void;

	function greetingFor(hour: number): string {
		if (hour < 12) return 'Good morning';
		if (hour < 18) return 'Good afternoon';
		return 'Good evening';
	}

	function formatRunTime(val?: string | number): string {
		if (!val) return '';
		const ms = typeof val === 'number' ? val : Date.parse(val);
		return Number.isFinite(ms) ? formatRelativeTime(ms) : '';
	}

	const greeting = greetingFor(new Date().getHours());
	$: recents = recentRuns.slice(0, 5);
</script>

<div class="vibe-empty" data-testid="vibe-empty-state">
	<div class="vibe-empty__hero" in:rise|global={{ y: 8 }}>
		<h2 class="vibe-empty__title">{greeting}</h2>
		<p class="vibe-empty__sub">
			{#if activeProjectName}
				What should we build in <strong>{activeProjectName}</strong>? Pick a start, or say it your way.
			{:else}
				What should we build? Describe it. Watch your build take shape live, and steer it as it grows.
			{/if}
		</p>
	</div>

	<div class="vibe-empty__cards">
		{#each VIBE_EMPTY_SUGGESTIONS as suggestion, i (suggestion.label)}
			<button
				type="button"
				class="vibe-empty-card"
				style={tintVars(suggestion.prompt)}
				title={suggestion.prompt}
				in:rise|global={{ delay: (i + 1) * 50 }}
				on:click={() => onSuggest(suggestion.prompt)}
			>
				<span class="vibe-empty-card__icon" aria-hidden="true">
					<Icon name={suggestion.icon} size={16} />
				</span>
				<span class="vibe-empty-card__label">{suggestion.label}</span>
				<span class="vibe-empty-card__hint">{suggestion.hint}</span>
			</button>
		{/each}
	</div>

	{#if recents.length > 0}
		<div class="vibe-empty__recents">
			<span class="vibe-empty__recents-label">Pick up where you left off</span>
			<div class="vibe-empty__chips">
				{#each recents as run (run.id)}
					<button
						type="button"
						class="vibe-empty-recent"
						style={tintVars(run.id)}
						on:click={() => onSelectRun(run.id)}
					>
						<span class="vibe-empty-recent__title">{stripRunTitlePrefix(run.title) || 'Untitled build'}</span>
						<span class="vibe-empty-recent__time">{formatRunTime(run.updatedAt || run.createdAt)}</span>
					</button>
				{/each}
			</div>
		</div>
	{/if}
</div>

<style>
	.vibe-empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		flex: 1 1 0px;
		min-height: 0;
		overflow-y: auto;
		gap: 1.25rem;
		width: 100%;
		max-width: var(--chat-col, 900px);
		margin: 0 auto;
		padding: 2rem 1rem;
		text-align: center;
		user-select: none;
	}

	.vibe-empty__hero {
		position: relative;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.35rem;
	}

	.vibe-empty__hero::before {
		content: '';
		position: absolute;
		inset: -3rem -5rem;
		z-index: -1;
		background: radial-gradient(closest-side, var(--accent-primary-soft, rgba(194, 80, 42, 0.1)), transparent);
		opacity: 0.7;
		pointer-events: none;
	}

	.vibe-empty__title {
		margin: 0;
		font-family: var(--font-display, inherit);
		font-size: var(--text-xl, 1.4rem);
		font-weight: 600;
		color: var(--text-primary);
	}

	.vibe-empty__sub {
		margin: 0;
		max-width: 440px;
		font-size: var(--text-sm, 0.875rem);
		color: var(--text-secondary);
		line-height: 1.5;
	}

	.vibe-empty__sub strong {
		color: var(--text-primary);
		font-weight: 600;
	}

	.vibe-empty__cards {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 0.75rem;
		width: min(560px, 100%);
	}

	@media (max-width: 480px) {
		.vibe-empty__cards {
			grid-template-columns: minmax(0, 1fr);
		}
	}

	.vibe-empty-card {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		grid-template-areas:
			'icon label'
			'icon hint';
		column-gap: 0.65rem;
		row-gap: 0.15rem;
		align-items: center;
		padding: 0.8rem 0.9rem;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: var(--radius-lg, 14px);
		background: var(--bg-card, #fff);
		box-shadow: var(--shadow-float, 0 4px 16px rgba(0, 0, 0, 0.06));
		font-family: var(--font-primary, inherit);
		text-align: left;
		cursor: pointer;
		transition:
			transform var(--dur-base, 280ms) var(--spring-snappy, cubic-bezier(0.16, 1, 0.3, 1)),
			border-color var(--dur-quick, 160ms);
	}

	.vibe-empty-card:hover {
		transform: translateY(-2px);
		border-color: var(--cn-tint-ink);
	}

	.vibe-empty-card:focus-visible,
	.vibe-empty-recent:focus-visible {
		outline: none;
		box-shadow: var(--ring-focus, 0 0 0 2px var(--accent-primary));
	}

	.vibe-empty-card__icon {
		grid-area: icon;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 2rem;
		height: 2rem;
		border-radius: var(--radius-md, 8px);
		background: var(--cn-tint);
		color: var(--cn-tint-ink);
	}

	.vibe-empty-card__label {
		grid-area: label;
		font-size: var(--text-sm, 0.875rem);
		font-weight: 600;
		color: var(--text-primary);
	}

	.vibe-empty-card__hint {
		grid-area: hint;
		overflow: hidden;
		font-size: var(--text-xs, 0.75rem);
		color: var(--text-muted);
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.vibe-empty__recents {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.5rem;
		width: min(560px, 100%);
		margin-top: 0.25rem;
	}

	.vibe-empty__recents-label {
		font-size: var(--text-2xs, 0.6875rem);
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-muted);
	}

	.vibe-empty__chips {
		display: flex;
		flex-wrap: wrap;
		justify-content: center;
		gap: 0.5rem;
	}

	.vibe-empty-recent {
		display: inline-flex;
		align-items: baseline;
		gap: 0.5rem;
		max-width: 100%;
		padding: 0.35rem 0.8rem;
		border: none;
		border-radius: var(--radius-chip, 999px);
		background: var(--cn-tint);
		color: var(--cn-tint-ink);
		font-family: var(--font-primary, inherit);
		font-size: var(--text-xs, 0.75rem);
		cursor: pointer;
		transition: transform var(--dur-base, 280ms) var(--spring-snappy, cubic-bezier(0.16, 1, 0.3, 1));
	}

	.vibe-empty-recent:hover {
		transform: translateY(-1px);
	}

	.vibe-empty-recent__title {
		min-width: 0;
		overflow: hidden;
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.vibe-empty-recent__time {
		flex-shrink: 0;
		opacity: 0.75;
	}
</style>
