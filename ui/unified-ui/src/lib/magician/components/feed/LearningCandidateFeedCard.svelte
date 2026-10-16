<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import type { FeedItem } from '$lib/feed/types';
	import {
		candidateReviewLabel,
		confidenceLabel,
		formatRelative,
		learningCandidateId,
		learningEvidenceCount,
		learningEvidenceHref,
		learningEvidenceRefs,
		learningTargetLabel,
		learningValueLabel,
		readMetadataString,
		titleCase
	} from '$lib/feed/learningCards';
	import Badge from '$lib/magician/components/generative/Badge.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';

	export let item: FeedItem;
	export let actionKey: string | null = null;
	export let compact = false;
	export let highlighted = false;
	export let showEdit = true;
	export let showEvidence = true;
	export let showOpen = true;

	const dispatch = createEventDispatcher<{
		open: FeedItem;
		openEvidence: FeedItem;
		confirm: FeedItem;
		edit: FeedItem;
		archive: FeedItem;
	}>();

	$: candidateId = learningCandidateId(item);
	$: evidenceRefs = learningEvidenceRefs(item);
	$: evidenceCount = learningEvidenceCount(item);
	$: confidence = confidenceLabel(item);
	$: evidenceHref = learningEvidenceHref(item);
	$: isBusy = actionKey !== null;
</script>

<article
	class:learning-card--compact={compact}
	class:learning-card--highlighted={highlighted}
	class="learning-card learning-card--candidate"
>
	<div class="learning-card__content">
		<div class="learning-card__header">
			<div>
				<div class="learning-card__eyebrow">
					<span>{candidateReviewLabel(item)}</span>
					<span>{formatRelative(item.updated_at)}</span>
				</div>
				<h3>{item.title}</h3>
			</div>
			{#if showOpen}
				<Button
					label="View"
					variant="outline"
					size="sm"
					on:click={() => dispatch('open', item)}
				/>
			{/if}
		</div>

		<p class="learning-card__value">{learningValueLabel(item)}</p>

		<div class="learning-card__meta">
			<Badge text={learningTargetLabel(item)} color="info" />
			{#if readMetadataString(item, 'risk_level')}
				<Badge text={`Risk ${titleCase(readMetadataString(item, 'risk_level'))}`} color="default" />
			{/if}
			{#if confidence}
				<Badge text={`Confidence ${confidence}`} color="default" />
			{/if}
			{#if readMetadataString(item, 'memory_key')}
				<span>{readMetadataString(item, 'memory_key')}</span>
			{/if}
			{#if evidenceCount > 0}
				<span>{evidenceCount} evidence {evidenceCount === 1 ? 'ref' : 'refs'}</span>
			{/if}
		</div>

		{#if showEvidence && evidenceRefs.length > 0}
			<details class="learning-card__evidence">
				<summary>Evidence</summary>
				<ul>
					{#each evidenceRefs as ref, index}
						<li>
							<strong>{titleCase(ref.kind || 'evidence')}</strong>
							{#if ref.summary}
								<span>{ref.summary}</span>
							{/if}
							{#if ref.uri}
								<a href={ref.uri} target="_blank" rel="noreferrer">Open URI</a>
							{:else if ref.path}
								<code>{ref.path}</code>
							{:else if ref.id}
								<code>{ref.id}</code>
							{:else}
								<code>ref {index + 1}</code>
							{/if}
						</li>
					{/each}
				</ul>
			</details>
		{:else if showEvidence}
			<a class="learning-card__evidence-link" href={evidenceHref} on:click|preventDefault={() => dispatch('openEvidence', item)}>
				Open source context
			</a>
		{/if}

		<div class="learning-card__actions">
			<Button
				label="Confirm"
				size="sm"
				disabled={!candidateId || isBusy}
				on:click={() => dispatch('confirm', item)}
			/>
			{#if showEdit}
				<Button
					label="Edit + file"
					variant="outline"
					size="sm"
					disabled={!candidateId || isBusy}
					on:click={() => dispatch('edit', item)}
				/>
			{/if}
			<Button
				label="Archive"
				variant="outline"
				size="sm"
				disabled={!candidateId || isBusy}
				on:click={() => dispatch('archive', item)}
			/>
		</div>
	</div>
</article>

<style>
	.learning-card {
		display: grid;
		gap: 0.75rem;
		padding: 1rem;
		border-radius: 1rem;
		border: 1px solid color-mix(in srgb, var(--accent-primary, #bf6f45) 34%, var(--border-soft, #d8d0c5));
		background:
			linear-gradient(135deg, color-mix(in srgb, var(--accent-primary, #bf6f45) 9%, transparent), transparent 52%),
			color-mix(in srgb, var(--bg-base, #fffdf8) 95%, transparent);
		box-shadow: var(--shadow-sm, 0 18px 40px color-mix(in srgb, var(--text-primary, #2d2a26) 8%, transparent));
		min-width: 0;
	}

	.learning-card--compact {
		box-shadow: none;
	}

	.learning-card--highlighted {
		border-color: color-mix(in srgb, var(--accent-primary, #bf6f45) 56%, var(--border-soft, #d8d0c5));
		box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent-primary, #bf6f45) 18%, transparent);
	}

	.learning-card__content,
	.learning-card__evidence,
	.learning-card__evidence ul {
		display: grid;
		gap: 0.65rem;
		min-width: 0;
	}

	.learning-card__header {
		display: flex;
		justify-content: space-between;
		gap: 0.85rem;
		align-items: flex-start;
		min-width: 0;
	}

	.learning-card__eyebrow,
	.learning-card__meta,
	.learning-card__actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		align-items: center;
	}

	.learning-card__eyebrow {
		color: var(--text-muted, #8a847a);
		font-family: var(--font-mono);
		font-size: 0.72rem;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	h3 {
		margin: 0.15rem 0 0;
		color: var(--text-primary, #2d2a26);
		font-size: 1rem;
		line-height: 1.25;
		overflow-wrap: anywhere;
	}

	p {
		margin: 0;
		overflow-wrap: anywhere;
		word-break: break-word;
	}

	.learning-card__value {
		color: var(--text-primary, #2d2a26);
		font-size: 0.96rem;
		line-height: 1.5;
	}

	.learning-card__meta span {
		font-size: 0.78rem;
		color: var(--text-muted, #8a847a);
	}

	.learning-card__actions :global(.muij-button) {
		border-radius: 999px;
	}

	.learning-card__evidence {
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 64%, transparent);
		border-radius: 0.85rem;
		background: color-mix(in srgb, var(--bg-card, #fff) 72%, transparent);
		padding: 0.6rem 0.72rem;
	}

	.learning-card__evidence summary {
		cursor: pointer;
		color: var(--text-secondary, #5d5850);
		font-size: 0.82rem;
		font-weight: 600;
	}

	.learning-card__evidence ul {
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.learning-card__evidence li {
		display: grid;
		gap: 0.25rem;
		padding-top: 0.55rem;
		color: var(--text-secondary, #5d5850);
		font-size: 0.82rem;
		min-width: 0;
	}

	.learning-card__evidence code {
		color: var(--text-muted, #8a847a);
		background: color-mix(in srgb, var(--bg-soft, #f6f1e8) 76%, transparent);
		border-radius: 0.45rem;
		padding: 0.18rem 0.35rem;
		overflow-wrap: anywhere;
	}

	.learning-card__evidence a,
	.learning-card__evidence-link {
		color: var(--accent-primary, #3f7fbf);
		text-decoration: none;
		font-size: 0.84rem;
	}

	.learning-card__evidence a:hover,
	.learning-card__evidence-link:hover {
		text-decoration: underline;
	}

	@media (max-width: 760px) {
		.learning-card__header {
			flex-direction: column;
		}
	}
</style>
