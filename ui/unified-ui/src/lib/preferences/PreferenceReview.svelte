<script lang="ts">
	/**
	 * Review what I've learned: the preferences Magician inferred, newest
	 * first, a page at a time. Confirm makes one the owner's own (it can then
	 * make an outward act wait); Dismiss retires it (checks stop judging it,
	 * its history is kept). The parent applies each action and reloads.
	 */
	import { createEventDispatcher } from 'svelte';

	import {
		ALL_SOURCES,
		REVIEW_PAGE_SIZE,
		reviewActions,
		reviewQueue,
		reviewSources
	} from './preferenceReview';
	import { presentStatement, type Preference } from './preferencesApi';

	export let preferences: Preference[] = [];
	export let busy = false;

	const dispatch = createEventDispatcher<{ confirm: Preference; dismiss: Preference }>();

	const STATUS_WORDS: Record<string, string> = {
		inferred: 'Inferred from your activity',
		inferred_untrusted: 'From a page, screen or meeting',
		unknown_provenance: 'Source unknown'
	};

	let source = ALL_SOURCES;
	let limit = REVIEW_PAGE_SIZE;

	const day = (ms: number | null): string | null => (ms ? new Date(ms).toLocaleDateString() : null);

	$: sources = reviewSources(preferences);
	$: if (source && !sources.some((entry) => entry.source === source)) source = ALL_SOURCES;
	$: queue = reviewQueue(preferences, source);
	$: shown = queue.slice(0, limit);
</script>

<section class="review" data-testid="preference-review">
	<p class="muted">
		Every one of these is weighed before Magician sends, books, orders or buys for you, but only as a
		hint. Confirm the ones that are right so they count as yours; dismiss the ones that are not.
	</p>

	{#if queue.length === 0 && !source}
		<p class="muted" data-testid="preference-review-empty">
			Nothing to review. Everything Magician checks against is something you said or confirmed.
		</p>
	{:else}
		<div class="toolbar">
			<p class="count" data-testid="preference-review-count">
				{queue.length} learned {queue.length === 1 ? 'preference' : 'preferences'} to review
			</p>
			{#if sources.length > 1}
				<label>
					<span class="muted">From</span>
					<select
						bind:value={source}
						on:change={() => (limit = REVIEW_PAGE_SIZE)}
						data-testid="preference-review-source"
					>
						<option value={ALL_SOURCES}>Everywhere</option>
						{#each sources as entry (entry.source)}
							<option value={entry.source}>{entry.source} ({entry.count})</option>
						{/each}
					</select>
				</label>
			{/if}
		</div>

		<ul>
			{#each shown as preference (preference.id)}
				{@const presented = presentStatement(preference.statement)}
				{@const actions = reviewActions(preference)}
				<li class="item" data-testid="preference-review-row">
					<p class="statement">{presented ? presented.title : preference.statement}</p>
					{#if presented}
						<dl class="fields">
							{#each presented.details as field (field.label)}
								<dt>{field.label}</dt>
								<dd>{field.value}</dd>
							{/each}
						</dl>
					{/if}
					{#if preference.conditions}
						<p class="muted">Applies {preference.conditions}</p>
					{/if}
					<p class="facts">
						<span>{STATUS_WORDS[preference.status] ?? preference.status}</span>
						<span class="muted">· {preference.source}</span>
						{#if day(preference.learnedAtMs)}
							<span class="muted">· learned {day(preference.learnedAtMs)}</span>
						{/if}
						{#if day(preference.lastCheckedAtMs)}
							<span class="muted">· last checked {day(preference.lastCheckedAtMs)}</span>
						{/if}
					</p>
					<div class="actions">
						{#if actions.canConfirm}
							<button
								type="button"
								class="primary"
								disabled={busy}
								on:click={() => dispatch('confirm', preference)}>Confirm</button
							>
						{/if}
						{#if actions.canDismiss}
							<button type="button" disabled={busy} on:click={() => dispatch('dismiss', preference)}
								>Dismiss</button
							>
						{/if}
						{#if actions.note}
							<span class="muted note">{actions.note}</span>
						{/if}
					</div>
				</li>
			{/each}
		</ul>

		{#if queue.length > shown.length}
			<button
				type="button"
				class="more"
				data-testid="preference-review-more"
				on:click={() => (limit += REVIEW_PAGE_SIZE)}
			>
				Show {Math.min(REVIEW_PAGE_SIZE, queue.length - shown.length)} more of {queue.length - shown.length}
			</button>
		{/if}
	{/if}
</section>

<style>
	.review {
		display: grid;
		gap: 0.5rem;
	}
	p {
		margin: 0.25rem 0;
		font-size: 0.9rem;
		overflow-wrap: anywhere;
	}
	.muted {
		color: var(--text-secondary);
	}
	.toolbar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
	}
	.count {
		font-weight: 700;
	}
	label {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.85rem;
	}
	select {
		min-width: 0;
		padding: 0.3rem 0.5rem;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.12));
		border-radius: 8px;
		background: var(--bg-secondary, transparent);
		color: inherit;
		font: inherit;
	}
	ul {
		display: grid;
		gap: 0.5rem;
		margin: 0;
		padding: 0;
	}
	.item {
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 10px;
		background: var(--bg-card, #fff);
		padding: 0.75rem 0.9rem;
		list-style: none;
	}
	.statement {
		font-weight: 600;
		color: var(--text-primary, #2d3436);
	}
	.fields {
		display: grid;
		grid-template-columns: minmax(7rem, max-content) 1fr;
		gap: 0.15rem 0.75rem;
		margin: 0.35rem 0 0.2rem;
		font-size: 0.85rem;
	}
	.fields dt {
		color: var(--text-secondary);
	}
	.fields dd {
		margin: 0;
		overflow-wrap: anywhere;
	}
	@media (max-width: 480px) {
		.fields {
			grid-template-columns: 1fr;
		}
	}
	.facts {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		font-size: 0.8rem;
	}
	.actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.4rem;
		margin-top: 0.55rem;
	}
	.note {
		font-size: 0.8rem;
	}
	/* The memory page's button language (.btn, .btn--small, variants). */
	button {
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.1));
		border-radius: 8px;
		background: var(--bg-card, #fff);
		color: var(--text-primary, #2d3436);
		padding: 0.3rem 0.65rem;
		font: 600 var(--text-sm, 0.85rem) var(--font-body, inherit);
		cursor: pointer;
		transition: border-color 120ms ease, background 120ms ease;
	}
	button:hover:not(:disabled) {
		border-color: var(--accent-primary, #ff6b6b);
	}
	button:focus-visible {
		outline: 2px solid var(--accent-primary, #ff6b6b);
		outline-offset: 2px;
	}
	button:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}
	button.primary {
		background: var(--accent-primary, #ff6b6b);
		border-color: var(--accent-primary, #ff6b6b);
		color: var(--button-primary-color, #fff);
	}
	button.more {
		justify-self: start;
	}
</style>
