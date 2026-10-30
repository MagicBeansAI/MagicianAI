<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { isDetailedHint } from '$lib/hitl/hitlDetails';

	import {
		ATTENTION_FAILED_COLOR,
		ATTENTION_FAILED_LABEL,
		ATTENTION_FILTER_LABELS,
		ATTENTION_SOURCE_COLORS,
		ATTENTION_SOURCE_LABELS,
		attentionQuickChoices,
		attentionRelativeTime,
		attentionScopeLabel,
		countAttentionRowsBySource,
		filterAttentionRows,
		skillEvolutionActionLabel,
		type AttentionDisplayRow,
		type AttentionInboxFeedback,
		type AttentionSourceFilter,
		type SkillEvolutionGateAction,
		type SkillEvolutionRollbackDecision
	} from './model';

	export let rows: AttentionDisplayRow[] = [];
	export let compact = false;
	export let sourceFilter: AttentionSourceFilter = 'all';
	export let search = '';
	export let pageLimit = 25;
	export let initialLoading = false;
	export let emptyError: string | null = null;
	export let feedback: AttentionInboxFeedback[] = [];
	export let canShowMore = false;
	export let showMoreBusy = false;
	export let hydratingKey: string | null = null;
	export let skillEvolutionActionKey: string | null = null;
	export let rollbackActionKey: string | null = null;
	/** Row key of the stalled wait whose run is being cancelled (C3). */
	export let stalledActionKey: string | null = null;
	/**
	 * Offer a pending agentic choice/confirmation's answers as buttons on its
	 * row (`attentionQuickChoices`), dispatched as `choose`. Off unless the host
	 * handles `choose`.
	 */
	export let quickChoices = false;
	/** `<row key>:<choice id>` of the quick answer being posted. */
	export let choosingKey: string | null = null;
	export let showSearch = true;
	export let showFilters = true;
	export let skeletonRows = 6;

	const dispatch = createEventDispatcher<{
		filterchange: { sourceFilter: AttentionSourceFilter };
		searchchange: { search: string };
		activate: { row: AttentionDisplayRow };
		skillaction: { row: AttentionDisplayRow; action: SkillEvolutionGateAction };
		rollbackdecision: {
			row: AttentionDisplayRow;
			decision: SkillEvolutionRollbackDecision;
		};
		/** Cancel the run behind a stalled-wait row (C3). */
		cancelstalled: { row: AttentionDisplayRow };
		/** Answer the row with one of its quick choices. */
		choose: { row: AttentionDisplayRow; choiceId: string };
		showmore: void;
	}>();

	$: filteredRows = filterAttentionRows(rows, sourceFilter, search);
	$: pagedRows = filteredRows.slice(0, pageLimit);
	$: countsBySource = countAttentionRowsBySource(rows);

	function skillActionDisabled(
		row: AttentionDisplayRow,
		action: SkillEvolutionGateAction
	): boolean {
		const details = row.skillEvolution;
		if (!details || skillEvolutionActionKey !== null) return true;
		if (action === 'approve' || action === 'reject') return details.gate !== 'proposal_review';
		return !details.actionEnabled || !details.targetSurface;
	}
</script>

{#if showSearch}
	<div class="attention-page__surface-controls">
		<input
			type="search"
			class="attention-page__search"
			placeholder="Filter by prompt, task, agent…"
			value={search}
			on:input={(event) =>
				dispatch('searchchange', {
					search: (event.currentTarget as HTMLInputElement).value
				})}
		/>
	</div>
{/if}

{#if showFilters}
	<nav class="attention-page__filters" aria-label="Filter by source">
		<button
			type="button"
			class="attention-page__chip"
			class:active={sourceFilter === 'all'}
			on:click={() => dispatch('filterchange', { sourceFilter: 'all' })}
		>
			All <span class="attention-page__chip-count">{rows.length}</span>
		</button>
		{#each Object.entries(ATTENTION_FILTER_LABELS) as [source, label] (source)}
			{@const count = countsBySource[source] ?? 0}
			{#if count > 0 || sourceFilter === source}
				<button
					type="button"
					class="attention-page__chip"
					class:active={sourceFilter === source}
					on:click={() =>
						dispatch('filterchange', { sourceFilter: source as AttentionSourceFilter })}
				>
					{label} <span class="attention-page__chip-count">{count}</span>
				</button>
			{/if}
		{/each}
	</nav>
{/if}

<slot name="before-list" />

{#each feedback as item, index (`${item.kind ?? 'notice'}:${index}:${item.message}`)}
	<div
		class="attention-page__empty attention-page__empty--quiet"
		class:attention-page__empty--error={item.kind === 'error'}
		role={item.kind === 'error' ? 'alert' : 'status'}
	>
		<p>{item.message}</p>
		{#if item.action}
			<a class="attention-page__feedback-action" href={item.action.href}>{item.action.label}</a>
		{/if}
	</div>
{/each}

{#if initialLoading && filteredRows.length === 0}
	<ul class="attention-page__list attention-page__list--skeleton" class:attention-page__list--compact={compact} aria-label="Loading attention items">
		{#each Array(Math.max(1, skeletonRows)) as _, index (index)}
			<li class="attention-skeleton" aria-hidden="true">
				<span class="attention-skeleton__source"></span>
				<div class="attention-skeleton__body">
					<span class="attention-skeleton__line attention-skeleton__line--title"></span>
					<span class="attention-skeleton__line attention-skeleton__line--detail"></span>
					<span class="attention-skeleton__line attention-skeleton__line--meta"></span>
				</div>
				<span class="attention-skeleton__action"></span>
			</li>
		{/each}
	</ul>
{:else if filteredRows.length === 0}
	<div class="attention-page__empty" role={emptyError ? 'alert' : 'status'}>
		<p>
			{emptyError
					? `Attention feed unavailable: ${emptyError}`
					: rows.length === 0
						? 'Inbox zero.'
						: 'No items match the current filter.'}
		</p>
	</div>
{:else}
	<ul class="attention-page__list" class:attention-page__list--compact={compact}>
		{#each pagedRows as row (row.key)}
			{@const rowColor = row.failed
				? ATTENTION_FAILED_COLOR
				: ATTENTION_SOURCE_COLORS[row.source]}
			{@const rowChoices = quickChoices ? attentionQuickChoices(row) : []}
			{@const rowChoosing = choosingKey !== null && choosingKey.startsWith(`${row.key}:`)}
			<li class="attention-page__row">
				<button
					type="button"
					class="attention-page__row-btn"
                    class:attention-page__row-btn--opener={!row.failed}
					disabled={(!row.failed && hydratingKey === row.key) || rowChoosing}
					on:click={() => dispatch('activate', { row })}
				>
					<span
						class="attention-page__source"
						style={`--attention-row-color: ${rowColor ?? 'var(--text-secondary)'}`}
					>
						{row.failed
							? ATTENTION_FAILED_LABEL
							: row.stalledWait
								? 'Stalled'
								: (ATTENTION_SOURCE_LABELS[row.source] ?? row.source)}
					</span>

					<div class="attention-page__body">
						<div class="attention-page__prompt">{row.prompt}</div>
						{#if row.hint && isDetailedHint(row.hint)}
							<!-- The details are a proposal to read; the row is a button and
							     cannot hold them, so it points to the prompt that shows them. -->
							<div class="attention-page__hint">Open to read the details before answering.</div>
						{:else if row.hint}
							<div class="attention-page__hint">{row.hint}</div>
						{/if}
						<div class="attention-page__meta">
							{#if row.delegated_agent_id}
								<span class="attention-page__from">from {row.delegated_agent_id}</span>
								<span>·</span>
							{/if}
							<span>{attentionScopeLabel(row.scope)}</span>
							<span>·</span>
							<span>{attentionRelativeTime(row.at)}</span>
						</div>
					</div>

					<span class="attention-page__cta">
						{row.failed
							? 'Dismiss ✕'
							: row.review_href
								? `${row.review_label ?? 'Review'} →`
								: hydratingKey === row.key
									? 'Loading…'
									: 'Respond →'}
					</span>
				</button>

				{#if rowChoices.length > 0}
					<!--
						One click answers the ask. A change/other answer opens the
						prompt, where its words are typed; the row click still opens
						the prompt for everything.
					-->
					<div class="attention-page__row-actions" role="group" aria-label={`Answers: ${row.prompt}`}>
						{#each rowChoices as choice, index (choice.id)}
							<!-- The button keeps its answer's name while it posts; aria-busy says it is sending. -->
							<button
								type="button"
								class="attention-page__action-btn"
								class:attention-page__action-btn--primary={index === 0 && !choice.opensPrompt}
								aria-busy={choosingKey === `${row.key}:${choice.id}` ? 'true' : undefined}
								disabled={choosingKey !== null || hydratingKey === row.key}
								on:click={() =>
									choice.opensPrompt
										? dispatch('activate', { row })
										: dispatch('choose', { row, choiceId: choice.id })}
							>
								{choice.label}
							</button>
						{/each}
					</div>
				{/if}

				{#if row.skillEvolution}
					<div class="attention-page__row-actions">
						<button
							type="button"
							class="attention-page__action-btn attention-page__action-btn--primary"
							disabled={skillActionDisabled(row, row.skillEvolution.action)}
							on:click={() =>
								dispatch('skillaction', { row, action: row.skillEvolution!.action })}
						>
							{skillEvolutionActionKey === `${row.key}:${row.skillEvolution.action}`
								? 'Working…'
								: skillEvolutionActionLabel(row.skillEvolution.action)}
						</button>
						{#if row.skillEvolution.gate === 'proposal_review'}
							<button
								type="button"
								class="attention-page__action-btn"
								disabled={skillActionDisabled(row, 'reject')}
								on:click={() => dispatch('skillaction', { row, action: 'reject' })}
							>
								{skillEvolutionActionKey === `${row.key}:reject` ? 'Working…' : 'Reject'}
							</button>
						{/if}
						{#if row.review_href}
							<button
								type="button"
								class="attention-page__action-btn"
								on:click={() => dispatch('activate', { row })}
							>
								Review
							</button>
						{/if}
					</div>
				{/if}

				{#if row.stalledWait}
					<!--
						C3: the run is waiting on the reader and no question reached them.
						There is nothing to answer, so the row offers the two things the
						reader can do about it — stop the run, or go and look at it.
					-->
					<div class="attention-page__row-actions">
						{#if row.stalledWait.actions.includes('cancel') && (row.stalledWait.executionId ?? row.execution_id)}
							<button
								type="button"
								class="attention-page__action-btn attention-page__action-btn--primary"
								disabled={stalledActionKey !== null}
								on:click={() => dispatch('cancelstalled', { row })}
							>
								{stalledActionKey === row.key ? 'Cancelling…' : 'Cancel run'}
							</button>
						{/if}
						{#if row.review_href}
							<button
								type="button"
								class="attention-page__action-btn"
								on:click={() => dispatch('activate', { row })}
							>
								{row.review_label ?? 'Open task'}
							</button>
						{/if}
					</div>
				{/if}

				{#if row.rollbackRecommendation}
					<div class="attention-page__row-actions">
						<button
							type="button"
							class="attention-page__action-btn attention-page__action-btn--primary"
							disabled={rollbackActionKey !== null}
							on:click={() => dispatch('rollbackdecision', { row, decision: 'dismissed' })}
						>
							{rollbackActionKey === `${row.key}:dismissed` ? 'Working…' : 'Dismiss'}
						</button>
						<button
							type="button"
							class="attention-page__action-btn"
							disabled={rollbackActionKey !== null}
							on:click={() => dispatch('rollbackdecision', { row, decision: 'superseded' })}
						>
							{rollbackActionKey === `${row.key}:superseded` ? 'Working…' : 'Supersede'}
						</button>
						{#if row.review_href}
							<button
								type="button"
								class="attention-page__action-btn"
								on:click={() => dispatch('activate', { row })}
							>
								Review
							</button>
						{/if}
					</div>
				{/if}
			</li>
		{/each}
	</ul>
	{#if canShowMore || filteredRows.length > pageLimit}
		<div class="attention-page__more">
			<button
				type="button"
				class="attention-page__chip"
				disabled={showMoreBusy}
				on:click={() => dispatch('showmore')}
			>
				{showMoreBusy ? 'Loading…' : 'Show more'}
			</button>
		</div>
	{/if}
{/if}

<style>
	.attention-page__surface-controls {
		display: flex;
		justify-content: flex-end;
	}

	.attention-page__action-btn[aria-busy='true'] {
		cursor: progress;
		opacity: 0.7;
	}

	.attention-page__search {
		min-width: 240px;
		padding: 0.5rem 0.85rem;
		border-radius: var(--radius-md, 8px);
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		color: var(--text-primary);
		font-size: var(--text-md, 0.95rem);
	}

	.attention-page__search:focus {
		outline: none;
		border-color: var(--accent-primary);
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent-primary) 18%, transparent);
	}

	.attention-page__filters {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		padding: 0.25rem 0;
	}

	.attention-page__chip {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.3rem 0.75rem;
		border-radius: 999px;
		border: 1px solid var(--border-soft);
		background: var(--bg-card);
		color: var(--text-secondary, var(--text-primary));
		font-size: var(--text-sm, 0.85rem);
		cursor: pointer;
		transition: background 120ms ease, color 120ms ease, border-color 120ms ease;
	}

	.attention-page__chip:hover:not(:disabled) {
		color: var(--text-primary);
		background: var(--bg-soft);
	}

	.attention-page__chip:disabled {
		cursor: default;
		opacity: 0.6;
	}

	.attention-page__chip.active {
		background: var(--accent-primary-soft, color-mix(in srgb, var(--accent-primary) 14%, transparent));
		color: var(--text-primary);
		border-color: color-mix(in srgb, var(--accent-primary) 35%, transparent);
	}

	.attention-page__chip-count {
		font-variant-numeric: tabular-nums;
		font-weight: 600;
		opacity: 0.75;
		font-size: var(--text-xs, 0.78rem);
	}

	.attention-page__empty {
		padding: 3rem 1rem;
		text-align: center;
		color: var(--text-secondary, var(--text-primary));
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-lg, 12px);
		background: var(--bg-card);
	}

	.attention-page__empty--quiet {
		padding: 1.25rem 1rem;
	}

	.attention-page__feedback-action {
		display: inline-block;
		margin-top: 0.45rem;
		color: var(--accent-primary);
		font-size: var(--text-xs, 0.78rem);
		font-weight: 600;
	}

	.attention-page__empty--error {
		border-color: color-mix(in srgb, var(--color-error, #d23a3a) 35%, transparent);
		color: var(--color-error, #d23a3a);
		background: color-mix(in srgb, var(--color-error, #d23a3a) 6%, var(--bg-card));
		opacity: 1;
	}

	.attention-page__list {
		list-style: none;
		padding: 0;
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: 0.55rem;
	}

	.attention-page__list--skeleton {
		pointer-events: none;
	}

	.attention-skeleton {
		display: grid;
		grid-template-columns: 84px minmax(0, 1fr) 56px;
		align-items: start;
		gap: 14px;
		min-height: 92px;
		padding: 14px;
		border: 1px solid var(--border-soft, #e5ded8);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-card, #fff);
	}

	.attention-skeleton__source,
	.attention-skeleton__line,
	.attention-skeleton__action {
		display: block;
		background: linear-gradient(
			90deg,
			var(--bg-soft, #f0ebe6) 25%,
			color-mix(in srgb, var(--bg-soft, #f0ebe6) 55%, var(--bg-card, #fff)) 50%,
			var(--bg-soft, #f0ebe6) 75%
		);
		background-size: 200% 100%;
		animation: attention-skeleton-shimmer 1.3s ease-in-out infinite;
	}

	.attention-skeleton__source {
		width: 68px;
		height: 22px;
		border-radius: var(--radius-sm, 4px);
	}

	.attention-skeleton__body {
		display: grid;
		gap: 9px;
	}

	.attention-skeleton__line {
		height: 11px;
		border-radius: var(--radius-sm, 4px);
	}

	.attention-skeleton__line--title { width: min(76%, 520px); height: 14px; }
	.attention-skeleton__line--detail { width: min(92%, 720px); }
	.attention-skeleton__line--meta { width: min(46%, 300px); }

	.attention-skeleton__action {
		width: 48px;
		height: 12px;
		margin-top: 4px;
		border-radius: var(--radius-sm, 4px);
	}

	@keyframes attention-skeleton-shimmer {
		to { background-position: -200% 0; }
	}

	.attention-page__more {
		display: flex;
		justify-content: center;
		padding: 0.8rem 0 0.2rem;
	}

	.attention-page__row {
		position: relative;
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		overflow: hidden;
		transition: border-color 140ms ease, transform 140ms ease, box-shadow 140ms ease;
	}

	.attention-page__row:hover {
		border-color: color-mix(in srgb, var(--accent-primary) 35%, var(--border-soft));
		transform: translateY(-1px);
		box-shadow: 0 4px 12px color-mix(in srgb, var(--text-primary) 8%, transparent);
	}

	.attention-page__row-btn {
		display: grid;
		grid-template-columns: auto 1fr auto;
		align-items: center;
		gap: 1rem;
		width: 100%;
		padding: 0.85rem 1rem;
		background: transparent;
		border: none;
		text-align: left;
		cursor: pointer;
		color: inherit;
		font: inherit;
	}


	/* Stretch the existing opener over the mini card, including its padding
	   and action-row gaps. Actual action buttons remain above it. */
	.attention-page__row-btn--opener::after {
		content: '';
		position: absolute;
		inset: 0;
	}

	.attention-page__row-btn--opener:focus-visible::after {
		outline: 2px solid var(--accent-primary);
		outline-offset: -2px;
	}

	.attention-page__action-btn {
		position: relative;
		z-index: 1;
	}

	.attention-page__row-btn:disabled {
		cursor: wait;
	}

	.attention-page__source {
		display: inline-flex;
		align-items: center;
		padding: 0.18rem 0.55rem;
		border-radius: 999px;
		border: 1px solid color-mix(in srgb, var(--attention-row-color) 40%, var(--border-soft));
		background: color-mix(in srgb, var(--attention-row-color) 14%, var(--bg-card));
		color: var(--text-primary, #2d3436);
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 600;
		letter-spacing: 0;
		white-space: nowrap;
	}

	.attention-page__body {
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.attention-page__prompt,
	.attention-page__hint {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.attention-page__prompt {
		color: var(--text-primary);
		font-weight: 500;
		font-size: var(--text-md, 0.95rem);
	}

	.attention-page__hint {
		color: var(--text-secondary, var(--text-primary));
		font-size: var(--text-sm, 0.85rem);
	}

	.attention-page__meta {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		color: var(--text-secondary, var(--text-primary));
		font-size: var(--text-xs, 0.78rem);
	}

	.attention-page__from {
		color: var(--text-primary);
		font-weight: 600;
	}

	.attention-page__cta {
		color: var(--accent-primary);
		font-size: var(--text-sm, 0.85rem);
		font-weight: 600;
		opacity: 0.9;
		transition: opacity 140ms ease, transform 140ms ease;
		white-space: nowrap;
	}

	.attention-page__row:hover .attention-page__cta {
		opacity: 1;
		transform: translateX(2px);
	}

	.attention-page__row-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		padding: 0 1rem 0.85rem;
	}

	.attention-page__action-btn {
		min-height: 32px;
		padding: 0.38rem 0.72rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: color-mix(in srgb, var(--bg-card) 92%, var(--accent-primary));
		color: var(--text-primary);
		font: inherit;
		font-size: var(--text-sm, 0.85rem);
		font-weight: 600;
		cursor: pointer;
		transition: transform 140ms ease, border-color 140ms ease, background 140ms ease;
	}

	.attention-page__action-btn--primary {
		border-color: color-mix(in srgb, var(--accent-primary) 45%, var(--border-soft));
		background: color-mix(in srgb, var(--accent-primary) 14%, var(--bg-card));
	}

	.attention-page__action-btn:hover:not(:disabled) {
		border-color: color-mix(in srgb, var(--accent-primary) 55%, var(--border-soft));
		transform: translateY(-1px);
	}

	.attention-page__action-btn:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	@media (max-width: 720px) {
		.attention-page__surface-controls,
		.attention-page__search {
			width: 100%;
		}

		.attention-page__row-btn {
			grid-template-columns: 1fr auto;
			gap: 0.65rem;
		}

		.attention-page__source {
			grid-column: 1 / -1;
			justify-self: start;
		}

		.attention-page__row-actions {
			justify-content: flex-start;
			flex-wrap: wrap;
		}
	}

	.attention-page__list--compact {
		gap: 0.5rem;
	}
	.attention-page__list--compact .attention-page__row-btn {
		grid-template-columns: minmax(0, 1fr) auto;
		gap: 0.25rem 0.625rem;
		padding: 0.625rem 0.75rem;
	}
	.attention-page__list--compact .attention-page__source {
		grid-column: 1;
		grid-row: 1;
		justify-self: start;
		font-size: 0.625rem;
		padding: 0.125rem 0.375rem;
	}
	.attention-page__list--compact .attention-page__body {
		grid-column: 1;
		grid-row: 2;
		gap: 0.1875rem;
	}
	.attention-page__list--compact .attention-page__prompt {
		font-size: 0.875rem;
		line-height: 1.35;
		white-space: normal;
		display: -webkit-box;
		-webkit-box-orient: vertical;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		overflow-wrap: anywhere;
	}
	.attention-page__list--compact .attention-page__hint {
		font-size: 0.75rem;
	}
	.attention-page__list--compact .attention-page__meta {
		font-size: 0.6875rem;
		gap: 0.25rem;
		overflow-wrap: anywhere;
	}
	.attention-page__list--compact .attention-page__cta {
		grid-column: 2;
		grid-row: 1 / 3;
		font-size: 0.75rem;
	}
	.attention-page__list--compact .attention-page__row-actions {
		padding: 0 0.75rem 0.625rem;
		gap: 0.375rem;
	}
	.attention-page__list--compact .attention-page__action-btn {
		font-size: 0.75rem;
		min-height: 30px;
	}
	.attention-page__list--compact .attention-skeleton {
		min-height: 80px;
		padding: 0.625rem 0.75rem;
	}

	@media (prefers-reduced-motion: reduce) {
		.attention-skeleton__source,
		.attention-skeleton__line,
		.attention-skeleton__action {
			animation: none;
		}
	}
</style>
