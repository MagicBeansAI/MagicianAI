<script lang="ts">
	// Chat-next empty state: a drop-in for ChatEmptyState (same props, same
	// prompt source). See docs/components/unified-ui/chat-next.md#empty-state.
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { formatRelativeTime } from '$lib/shared/formatRelativeTime';
	import type { ChatSession } from '$lib/stores/chatStore';
	import { EMPTY_STATE_PROMPTS } from '$lib/magician/chat/components/emptyStatePrompts';
	import { tintVars } from './tint';
	import { rise } from './motion';

	export let isReadOnly = false;
	export let recentSessions: ChatSession[] = [];
	export let onSuggest: (prompt: string) => void;
	export let onOpenSession: (sessionId: string) => void;

	const CARD_COUNT = 4;
	const RECENT_CAP = 5;
	const suggestions = EMPTY_STATE_PROMPTS.slice(0, CARD_COUNT);

	function greetingFor(hour: number): string {
		if (hour < 12) return 'Good morning';
		if (hour < 18) return 'Good afternoon';
		return 'Good evening';
	}

	const greeting = greetingFor(new Date().getHours());
	$: recents = recentSessions.slice(0, RECENT_CAP);
</script>

<div class="cn-empty" data-testid="chat-empty-next">
	<div class="cn-empty__hero" in:rise|global={{ y: 8 }}>
		<h3 class="cn-empty__title">{isReadOnly ? 'Nothing in this session' : greeting}</h3>
		{#if !isReadOnly}
			<p class="cn-empty__sub">What should I take care of? Pick a start, or say it your way.</p>
		{/if}
	</div>

	{#if !isReadOnly}
		<div class="cn-empty__cards">
			{#each suggestions as suggestion, i (suggestion.label)}
				<button
					type="button"
					class="cn-empty-card"
					style={tintVars(suggestion.prompt)}
					title={suggestion.prompt}
					in:rise|global={{ delay: (i + 1) * 50 }}
					on:click={() => onSuggest(suggestion.prompt)}
				>
					<span class="cn-empty-card__icon" aria-hidden="true">
						<Icon name={suggestion.icon} size={16} />
					</span>
					<span class="cn-empty-card__label">{suggestion.label}</span>
					<span class="cn-empty-card__hint">{suggestion.prompt}</span>
				</button>
			{/each}
		</div>
	{/if}

	{#if recents.length > 0}
		<div class="cn-empty__recents">
			<span class="cn-empty__recents-label">Pick up where you left off</span>
			<div class="cn-empty__chips">
				{#each recents as session (session.id)}
					<button
						type="button"
						class="cn-empty-recent"
						style={tintVars(session.id)}
						on:click={() => onOpenSession(session.id)}
					>
						<span class="cn-empty-recent__title">{session.title || 'Untitled session'}</span>
						<span class="cn-empty-recent__time">{formatRelativeTime(session.updated_at)}</span>
					</button>
				{/each}
			</div>
		</div>
	{/if}
</div>

<style>
	/* Same layout contract as the classic empty state: fill the messages area
	   but never shrink below content (centering in a shrunk flex child clips
	   the top rows on short viewports). The page's `.chat-next` wash is the
	   background; only a soft glow sits behind the greeting. */
	.cn-empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		flex: 1 0 auto;
		gap: 1.25rem;
		width: 100%;
		max-width: var(--chat-col);
		margin: 0 auto;
		padding: 2rem 1rem;
		text-align: center;
	}

	.cn-empty__hero {
		position: relative;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.35rem;
	}

	.cn-empty__hero::before {
		content: '';
		position: absolute;
		inset: -3rem -5rem;
		z-index: -1;
		background: radial-gradient(closest-side, var(--accent-primary-soft), transparent);
		opacity: 0.7;
		pointer-events: none;
	}

	.cn-empty__title {
		margin: 0;
		font-family: var(--font-display);
		font-size: var(--text-xl);
		font-weight: 600;
		color: var(--text-primary);
	}

	.cn-empty__sub {
		margin: 0;
		max-width: 380px;
		font-size: var(--text-sm);
		color: var(--text-secondary);
	}

	.cn-empty__cards {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 0.75rem;
		width: min(560px, 100%);
	}

	@media (max-width: 480px) {
		.cn-empty__cards {
			grid-template-columns: minmax(0, 1fr);
		}
	}

	.cn-empty-card {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr);
		grid-template-areas:
			'icon label'
			'icon hint';
		column-gap: 0.65rem;
		row-gap: 0.15rem;
		align-items: center;
		padding: 0.8rem 0.9rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-lg);
		background: var(--bg-card);
		box-shadow: var(--shadow-float);
		font-family: var(--font-primary);
		text-align: left;
		cursor: pointer;
		transition:
			transform var(--dur-base) var(--spring-snappy),
			border-color var(--dur-quick);
	}

	.cn-empty-card:hover {
		transform: translateY(-2px);
		border-color: var(--cn-tint-ink);
	}

	.cn-empty-card:focus-visible,
	.cn-empty-recent:focus-visible {
		outline: none;
		box-shadow: var(--ring-focus);
	}

	.cn-empty-card__icon {
		grid-area: icon;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 2rem;
		height: 2rem;
		border-radius: var(--radius-md);
		background: var(--cn-tint);
		color: var(--cn-tint-ink);
	}

	.cn-empty-card__label {
		grid-area: label;
		font-size: var(--text-sm);
		font-weight: 600;
		color: var(--text-primary);
	}

	.cn-empty-card__hint {
		grid-area: hint;
		overflow: hidden;
		font-size: var(--text-xs);
		color: var(--text-muted);
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.cn-empty__recents {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.5rem;
		width: min(560px, 100%);
	}

	.cn-empty__recents-label {
		font-size: var(--text-2xs);
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-muted);
	}

	.cn-empty__chips {
		display: flex;
		flex-wrap: wrap;
		justify-content: center;
		gap: 0.5rem;
	}

	.cn-empty-recent {
		display: inline-flex;
		align-items: baseline;
		gap: 0.5rem;
		max-width: 100%;
		padding: 0.35rem 0.8rem;
		border: none;
		border-radius: var(--radius-chip);
		background: var(--cn-tint);
		color: var(--cn-tint-ink);
		font-family: var(--font-primary);
		font-size: var(--text-xs);
		cursor: pointer;
		transition: transform var(--dur-base) var(--spring-snappy);
	}

	.cn-empty-recent:hover {
		transform: translateY(-1px);
	}

	.cn-empty-recent__title {
		min-width: 0;
		overflow: hidden;
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.cn-empty-recent__time {
		flex-shrink: 0;
		opacity: 0.75;
	}
</style>
