<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	import Icon from '$lib/shared/icons/Icon.svelte';
	import type {
		ResurfacingActionKind,
		ResurfacingCard,
		ResurfacingContextualActionResult,
		ResurfacingDetail,
		ResurfacingOriginalContent
	} from './resurfacingQueries';
	import {
		mergeResurfacingCapabilities,
		resurfacingBrief,
		resurfacingConcreteSummary,
		resurfacingDisplayTitle,
		resurfacingRecommendation
	} from './resurfacingPresentation';

	export let card: ResurfacingCard;
	export let detail: ResurfacingDetail | null = null;
	export let loading = false;
	export let loadError: string | null = null;
	export let originalLoading = false;
	export let actionBusy = false;
	export let deeperSummary: Extract<
		ResurfacingContextualActionResult,
		{ kind: 'deeper_summary' }
	> | null = null;

	const dispatch = createEventDispatcher<{
		close: void;
		action: { kind: ResurfacingActionKind };
	}>();

	$: brief = resurfacingBrief(card, detail);
	$: title = resurfacingDisplayTitle(card, detail);
	$: summary = resurfacingConcreteSummary(card, detail);
	$: recommendation = resurfacingRecommendation(card, detail);
	$: capabilityKinds = new Set(
		mergeResurfacingCapabilities(card, detail).map((capability) => capability.kind)
	);
	$: original = detail?.original ?? null;
	$: sourceStatus = detail ? statusText(detail.status) : '';

	function statusText(status: ResurfacingDetail['status']): string {
		switch (status) {
			case 'newer_available': return 'A newer source item is available.';
			case 'stale': return 'This source has changed since it was summarized.';
			case 'offline': return 'The source provider is offline.';
			case 'deleted': return 'The source item was deleted.';
			case 'suppressed': return 'This source is restricted by content policy.';
			case 'unsupported': return 'This source does not support live detail.';
			case 'unavailable': return 'The source is currently unavailable.';
			default: return '';
		}
	}

	function originalTitle(content: ResurfacingOriginalContent): string {
		switch (content.kind) {
			case 'comm': return content.subject || 'Original message';
			case 'task': return content.title || 'Current task';
			case 'memory': return content.key || 'Memory record';
			case 'web': return content.title || 'Web source';
		}
	}
</script>

<div class="worth-detail" aria-live="polite" data-testid="resurfacing-detail-panel">
	<div class="worth-detail__top">
		<div class="worth-detail__identity">
			<span>{card.source_kind.replace(/_/g, ' ') || 'source'}</span>
			{#if detail?.source_updated || detail?.has_newer}
				<strong>Updated source</strong>
			{/if}
		</div>
		<button
			type="button"
			class="worth-icon-btn"
			title="Close details"
			aria-label="Close details"
			on:click={() => dispatch('close')}
		>
			<Icon name="x" size={16} />
		</button>
	</div>

	{#if loading && !detail}
		<div class="worth-detail__loading" aria-label="Loading current details">
			<span></span><span></span><span></span>
		</div>
	{:else if loadError && !detail}
		<div class="worth-detail__notice worth-detail__notice--error" role="alert">
			<span>{loadError}</span>
			<button type="button" class="worth-link-btn" on:click={() => dispatch('action', { kind: 'view_details' })}>
				Retry
			</button>
		</div>
	{:else}
		{#if sourceStatus}
			<div
				class="worth-detail__notice"
				class:worth-detail__notice--error={detail?.status === 'deleted' || detail?.status === 'suppressed'}
				role="status"
			>
				<Icon name="alert" size={15} />
				<span>{sourceStatus}</span>
			</div>
		{/if}

		<div class="worth-detail__heading">
			<h3>{title}</h3>
			{#if !detail && card.line && card.line !== title && card.line !== summary}
				<p>{card.line}</p>
			{/if}
		</div>

		<div class="worth-detail__sections">
			{#if summary}
				<section>
					<h4>Summary</h4>
					<p>{summary}</p>
				</section>
			{/if}

			{#if brief?.changes.length}
				<section>
					<h4>What changed</h4>
					<dl class="worth-detail__changes">
						{#each brief.changes as change, index (`${change.aspect}-${index}`)}
							<div>
								<dt>{change.aspect || 'Change'}</dt>
								<dd>
									{#if change.before && change.after}
										<span>{change.before}</span><Icon name="arrow-right" size={13} /><strong>{change.after}</strong>
									{:else}
										<strong>{change.after || change.before || 'Changed'}</strong>
									{/if}
									{#if change.effective_text}<small>{change.effective_text}</small>{/if}
								</dd>
							</div>
						{/each}
					</dl>
				</section>
			{/if}

			{#if brief?.key_facts.length}
				<section>
					<h4>Key facts</h4>
					<ul>
						{#each brief.key_facts as fact}<li>{fact}</li>{/each}
					</ul>
				</section>
			{/if}

			{#if brief?.temporal_facts.length}
				<section>
					<h4>Dates and deadlines</h4>
					<dl class="worth-detail__facts">
						{#each brief.temporal_facts as fact, index (`${fact.kind}-${index}`)}
							<div><dt>{fact.kind.replace(/_/g, ' ')}</dt><dd>{fact.text}</dd></div>
						{/each}
					</dl>
				</section>
			{/if}

			{#if brief?.missing_details.length}
				<section class="worth-detail__missing">
					<h4>Information not supplied by the source</h4>
					<ul>
						{#each brief.missing_details as missing}<li>{missing}</li>{/each}
					</ul>
				</section>
			{/if}

			{#if !detail && card.why_now}
				<section>
					<h4>Why now</h4>
					<p>{card.why_now}</p>
				</section>
			{/if}

			{#if recommendation}
				<section>
					<h4>Recommended next step</h4>
					<p><strong>{recommendation.label}</strong>{recommendation.rationale ? ` — ${recommendation.rationale}` : ''}</p>
				</section>
			{/if}

			{#if deeperSummary}
				<section class="worth-detail__deep">
					<h4>Deeper summary</h4>
					<p>{deeperSummary.summary}</p>
					{#if deeperSummary.key_points.length}
						<ul>{#each deeperSummary.key_points as point}<li>{point}</li>{/each}</ul>
					{/if}
					{#if deeperSummary.recommended_actions.length}
						<h5>Possible next steps</h5>
						<ul>{#each deeperSummary.recommended_actions as action}<li>{action}</li>{/each}</ul>
					{/if}
					{#if deeperSummary.caveats.length}
						<h5>Caveats</h5>
						<ul>{#each deeperSummary.caveats as caveat}<li>{caveat}</li>{/each}</ul>
					{/if}
				</section>
			{/if}
		</div>

		{#if originalLoading}
			<div class="worth-original worth-original--loading" aria-label="Loading original source">
				<span></span><span></span><span></span>
			</div>
		{:else if original}
			<section class="worth-original" aria-label="Original source content">
				<div class="worth-original__header">
					<h4>{originalTitle(original)}</h4>
					{#if original.kind === 'comm' && original.evidence_messages.some((message) => message.truncated)}
						<span>Bounded preview</span>
					{/if}
				</div>
				<div class="worth-original__body">
					{#if original.kind === 'comm'}
						{#if original.evidence_messages.length > 1}
							{#each original.evidence_messages as message, index (message.message_id)}
								<article>
									<h5>{message.subject || `Message ${index + 1}`}</h5>
									<pre>{message.body || message.summary || 'Message body unavailable.'}</pre>
								</article>
							{/each}
						{:else}
							<pre>{original.body || original.summary || original.evidence_messages[0]?.body || 'Message body unavailable.'}</pre>
						{/if}
					{:else if original.kind === 'task'}
						<dl><div><dt>Status</dt><dd>{original.status}</dd></div></dl>
						<p>{original.outcome || 'No task outcome is stored.'}</p>
					{:else}
						<pre>{original.summary}</pre>
					{/if}
				</div>
			</section>
		{/if}

		<div class="worth-detail__actions">
			{#if capabilityKinds.has('show_original') && !original}
				<button
					type="button"
					class="worth-detail-btn"
					disabled={actionBusy || originalLoading}
					on:click={() => dispatch('action', { kind: 'show_original' })}
				>
					<Icon name="eye" size={15} />
					{originalLoading ? 'Loading…' : 'Original'}
				</button>
			{/if}
			{#if capabilityKinds.has('open_source')}
				<button
					type="button"
					class="worth-detail-btn"
					disabled={actionBusy}
					on:click={() => dispatch('action', { kind: 'open_source' })}
				>
					<Icon name="arrow-up-right" size={15} />
					Open source
				</button>
			{/if}
		</div>
	{/if}
</div>

<style>
	.worth-detail {
		display: grid;
		gap: 0.8rem;
		padding: 0.8rem 0.25rem 0.25rem;
		border-top: 2px solid color-mix(in srgb, var(--accent-primary) 32%, var(--border-soft));
		color: var(--text-primary);
	}

	.worth-detail__top,
	.worth-detail__actions,
	.worth-original__header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.55rem;
		min-width: 0;
	}

	.worth-detail__identity {
		display: flex;
		align-items: center;
		gap: 0.45rem;
		min-width: 0;
		font-size: var(--text-2xs);
		text-transform: capitalize;
		color: var(--text-muted);
	}

	.worth-detail__identity strong {
		padding: 0.12rem 0.35rem;
		border-radius: var(--radius-sm);
		background: color-mix(in srgb, var(--color-warning) 12%, transparent);
		color: var(--color-warning);
		font-weight: 700;
		white-space: nowrap;
	}

	.worth-icon-btn {
		display: inline-grid;
		place-items: center;
		width: 2rem;
		height: 2rem;
		padding: 0;
		border: 1px solid var(--border-default, var(--border-soft));
		border-radius: var(--radius-sm);
		background: var(--bg-card);
		color: var(--text-secondary);
		cursor: pointer;
	}

	.worth-detail__loading,
	.worth-original--loading {
		display: grid;
		gap: 0.5rem;
		padding: 0.35rem 0;
	}

	.worth-detail__loading span,
	.worth-original--loading span {
		display: block;
		height: 0.8rem;
		border-radius: var(--radius-sm);
		background: color-mix(in srgb, var(--border-soft) 54%, transparent);
		animation: worth-detail-pulse 1.1s ease-in-out infinite alternate;
	}

	.worth-detail__loading span:nth-child(2),
	.worth-original--loading span:nth-child(2) { width: 86%; }
	.worth-detail__loading span:nth-child(3),
	.worth-original--loading span:nth-child(3) { width: 68%; }

	@keyframes worth-detail-pulse {
		from { opacity: 0.4; }
		to { opacity: 0.9; }
	}

	.worth-detail__notice {
		display: flex;
		align-items: flex-start;
		gap: 0.4rem;
		padding: 0.48rem 0.58rem;
		border-left: 3px solid var(--color-warning);
		background: color-mix(in srgb, var(--color-warning) 8%, transparent);
		color: var(--text-secondary);
		font-size: var(--text-xs);
		line-height: 1.4;
	}

	.worth-detail__notice--error {
		border-left-color: var(--color-error);
		background: color-mix(in srgb, var(--color-error) 8%, transparent);
	}

	.worth-link-btn {
		margin-left: auto;
		border: 0;
		background: transparent;
		color: var(--accent-primary);
		font: inherit;
		font-weight: 650;
		cursor: pointer;
	}

	.worth-detail__heading h3,
	.worth-original h4 {
		margin: 0;
		font-size: var(--text-sm);
		line-height: 1.35;
		overflow-wrap: anywhere;
	}

	.worth-detail__heading p {
		margin: 0.18rem 0 0;
		color: var(--text-muted);
		font-size: var(--text-xs);
		line-height: 1.4;
	}

	.worth-detail__sections {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 0.85rem 1.1rem;
	}

	.worth-detail__sections section {
		min-width: 0;
	}

	.worth-detail__sections h4,
	.worth-detail__sections h5 {
		margin: 0 0 0.25rem;
		color: var(--text-muted);
		font-size: var(--text-2xs);
		font-weight: 750;
		line-height: 1.3;
		text-transform: uppercase;
		letter-spacing: 0;
	}

	.worth-detail__sections h5 {
		margin-top: 0.6rem;
	}

	.worth-detail__sections p,
	.worth-detail__sections ul,
	.worth-detail__sections dl {
		margin: 0;
		color: var(--text-secondary);
		font-size: var(--text-xs);
		line-height: 1.48;
		overflow-wrap: anywhere;
	}

	.worth-detail__sections ul {
		padding-left: 1rem;
	}

	.worth-detail__changes,
	.worth-detail__facts {
		display: grid;
		gap: 0.38rem;
	}

	.worth-detail__changes > div,
	.worth-detail__facts > div {
		display: grid;
		grid-template-columns: minmax(6rem, 0.65fr) minmax(0, 1fr);
		gap: 0.45rem;
	}

	.worth-detail__changes dt,
	.worth-detail__facts dt {
		color: var(--text-muted);
		text-transform: capitalize;
	}

	.worth-detail__changes dd,
	.worth-detail__facts dd {
		margin: 0;
	}

	.worth-detail__changes dd {
		display: flex;
		align-items: center;
		gap: 0.3rem;
		flex-wrap: wrap;
	}

	.worth-detail__changes small {
		flex-basis: 100%;
		color: var(--text-muted);
	}

	.worth-detail__missing {
		padding-left: 0.55rem;
		border-left: 3px solid var(--color-warning);
	}

	.worth-detail__deep {
		grid-column: 1 / -1;
		padding-top: 0.7rem;
		border-top: 1px solid var(--border-soft);
	}

	.worth-original {
		display: grid;
		gap: 0.5rem;
		padding-top: 0.7rem;
		border-top: 1px solid var(--border-soft);
	}

	.worth-original__header span {
		color: var(--text-muted);
		font-size: var(--text-2xs);
		white-space: nowrap;
	}

	.worth-original__body {
		max-height: 18rem;
		overflow: auto;
		overscroll-behavior: contain;
		padding: 0.65rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		background: var(--bg-soft);
	}

	.worth-original article + article {
		margin-top: 0.75rem;
		padding-top: 0.75rem;
		border-top: 1px solid var(--border-soft);
	}

	.worth-original h5 {
		margin: 0 0 0.3rem;
		font-size: var(--text-xs);
	}

	.worth-original pre,
	.worth-original p,
	.worth-original dl {
		margin: 0;
		white-space: pre-wrap;
		word-break: break-word;
		font: inherit;
		font-size: var(--text-xs);
		line-height: 1.5;
		color: var(--text-secondary);
	}

	.worth-detail__actions {
		justify-content: flex-end;
		flex-wrap: wrap;
	}

	.worth-detail-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		min-height: 2rem;
		padding: 0.38rem 0.62rem;
		border: 1px solid var(--border-default, var(--border-soft));
		border-radius: var(--radius-sm);
		background: var(--bg-card);
		color: var(--text-secondary);
		font: inherit;
		font-size: var(--text-xs);
		line-height: 1;
		white-space: nowrap;
		cursor: pointer;
	}

	.worth-detail-btn:disabled {
		opacity: 0.55;
		cursor: default;
	}

	@media (max-width: 720px) {
		.worth-detail__sections {
			grid-template-columns: 1fr;
		}

		.worth-detail__deep {
			grid-column: auto;
		}
	}
</style>
