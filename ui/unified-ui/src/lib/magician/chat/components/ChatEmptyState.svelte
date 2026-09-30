<script lang="ts">
	import { settleIn } from '$lib/shared/motion';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import type { IconName } from '$lib/shared/icons/paths';
	import { formatRelativeTime } from '$lib/shared/formatRelativeTime';
	import type { ChatSession } from '$lib/stores/chatStore';

	export let isReadOnly = false;
	// Up to 3 recent sessions for the launcher, computed by ChatPanel
	// (archived sessions + the store's active session, current one
	// excluded — see `recentLauncherSessions` there).
	export let recentSessions: ChatSession[] = [];
	// Fill the composer with a suggested prompt (no auto-send) — the
	// composer refs live in ChatPanel.
	export let onSuggest: (prompt: string) => void;
	// Switch to / view a recent session (ChatPanel's handleViewExecution).
	export let onOpenSession: (sessionId: string) => void;

	// ── Empty-state capability launcher ─────────────────────────────
	// Suggested prompts shown when a session has no messages yet. Voice
	// and several prompts are borrowed from the landing page's sample prompts.
	// Each maps to a
	// real capability: research, meetings, ordering, screen watching,
	// coding (VibeDev) and briefings.
	const EMPTY_STATE_PROMPTS: readonly { icon: IconName; label: string; prompt: string }[] = [
		{
			icon: 'search',
			label: 'Research flights',
			prompt: 'Find the cheapest Kyoto flights for next month and shortlist the three best options.'
		},
		{
			icon: 'calendar',
			label: 'Catch up on a meeting',
			prompt: 'What did I miss in yesterday’s design sync? Decisions, and anything I owe people.'
		},
		{
			icon: 'rotate-ccw',
			label: 'Restock the groceries',
			prompt: 'We’re out of groceries. Order my usual list and stay within my spending limit.'
		},
		{
			icon: 'monitor',
			label: 'Review my screen time',
			prompt: 'Look at my recent screen observations and summarize where my time went today.'
		},
		{
			icon: 'git-branch',
			label: 'Build a small app',
			prompt: 'Build me a small web app — a habit tracker to start.'
		},
		{
			icon: 'inbox',
			label: 'Morning briefing',
			prompt: 'Give me a briefing: calendar, inbox, and anything I missed since yesterday.'
		}
	];
</script>

<div class="chat-empty-state" in:settleIn={{ y: 10 }}>
	<div class="chat-empty-icon">
		<Icon name="sparkle" size={28} />
	</div>
	<h3 class="chat-empty-title">
		{isReadOnly ? 'Nothing in this session' : 'What should I take care of?'}
	</h3>
	{#if !isReadOnly}
		<p class="chat-empty-text">Pick a starting point, or describe the task in your own words.</p>
		<div class="chat-empty-chips">
			{#each EMPTY_STATE_PROMPTS as suggestion (suggestion.label)}
				<button
					type="button"
					class="chat-empty-chip"
					on:click={() => onSuggest(suggestion.prompt)}
				>
					<Icon name={suggestion.icon} size={14} />
					<span>{suggestion.label}</span>
				</button>
			{/each}
		</div>
	{/if}
	{#if recentSessions.length > 0}
		<div class="chat-empty-recents">
			<span class="chat-empty-recents-label">Pick up where you left off</span>
			{#each recentSessions as session (session.id)}
				<button
					type="button"
					class="chat-empty-recent-row"
					on:click={() => onOpenSession(session.id)}
				>
					<span class="chat-empty-recent-title">{session.title || 'Untitled session'}</span>
					<span class="chat-empty-recent-time">{formatRelativeTime(session.updated_at)}</span>
				</button>
			{/each}
		</div>
	{/if}
</div>

<style>
	/* Capability launcher (empty session): greeting + prompt chips +
	   recent sessions, centered in the messages area. `flex: 1 0 auto`
	   fills the area when content fits but refuses to shrink below its
	   content — centering inside a shrunk flex child would clip the top
	   rows unreachably on short viewports. */
	.chat-empty-state {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		flex: 1 0 auto;
		text-align: center;
		padding: 2rem 1rem;
		gap: 0.5rem;
		width: 100%;
		max-width: var(--chat-col);
		margin: 0 auto;
	}

	.chat-empty-icon {
		margin-bottom: 0.25rem;
		color: var(--accent-primary, #ff6b6b);
	}

	.chat-empty-title {
		font-size: var(--text-lg, 1.15rem);
		font-weight: 600;
		color: var(--text-primary, #2d3436);
		margin: 0;
	}

	.chat-empty-text {
		font-size: var(--text-sm);
		color: var(--text-secondary, #5f6668);
		margin: 0;
		max-width: 360px;
	}

	.chat-empty-chips {
		display: flex;
		flex-wrap: wrap;
		justify-content: center;
		gap: 0.5rem;
		margin-top: 0.75rem;
		max-width: 560px;
	}

	.chat-empty-chip {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.4rem 0.85rem;
		border: 1px solid var(--border-soft, #eee4dc);
		border-radius: var(--radius-full, 9999px);
		background: var(--bg-card, #ffffff);
		color: var(--text-secondary, #5f6668);
		font-family: var(--font-primary);
		font-size: var(--text-xs);
		font-weight: 600;
		cursor: pointer;
		transition: border-color 120ms ease, color 120ms ease, transform 120ms ease;
	}

	.chat-empty-chip :global(svg) {
		color: var(--accent-primary, #ff6b6b);
		flex-shrink: 0;
	}

	.chat-empty-chip:hover,
	.chat-empty-chip:focus-visible {
		border-color: var(--accent-primary, #ff6b6b);
		color: var(--text-primary, #2d3436);
		transform: translateY(-1px);
	}

	.chat-empty-recents {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
		margin-top: 1.25rem;
		width: min(360px, 100%);
	}

	.chat-empty-recents-label {
		font-size: var(--text-2xs);
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--text-muted, #8f9799);
		margin-bottom: 0.25rem;
	}

	.chat-empty-recent-row {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.45rem 0.75rem;
		border: 1px solid transparent;
		border-radius: var(--radius-md, 10px);
		background: transparent;
		font-family: var(--font-primary);
		cursor: pointer;
		transition: background 120ms ease, border-color 120ms ease;
	}

	.chat-empty-recent-row:hover,
	.chat-empty-recent-row:focus-visible {
		background: var(--bg-card, #ffffff);
		border-color: var(--border-soft, #eee4dc);
	}

	.chat-empty-recent-title {
		flex: 1;
		min-width: 0;
		font-size: var(--text-sm);
		font-weight: 500;
		color: var(--text-primary, #2d3436);
		text-align: left;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.chat-empty-recent-time {
		flex-shrink: 0;
		font-size: var(--text-2xs);
		color: var(--text-muted, #8f9799);
	}
</style>
