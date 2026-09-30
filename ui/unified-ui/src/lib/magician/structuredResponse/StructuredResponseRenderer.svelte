<script lang="ts">
	import { toPlainText } from './toPlainText';
	import { isStructuredResponseV1, validateStructuredResponse } from './schema';
	import type { StructuredResponseActionV1, StructuredResponseV1 } from './types';
	import { isActionRenderable, presentationForAction } from './actions';
	import ArtifactsBlock from './blocks/ArtifactsBlock.svelte';
	import CalloutBlock from './blocks/CalloutBlock.svelte';
	import KeyValuesBlock from './blocks/KeyValuesBlock.svelte';
	import ListBlock from './blocks/ListBlock.svelte';
	import MarkdownBlock from './blocks/MarkdownBlock.svelte';
	import MetricsBlock from './blocks/MetricsBlock.svelte';
	import SourcesBlock from './blocks/SourcesBlock.svelte';
	import TableBlock from './blocks/TableBlock.svelte';
	import TextBlock from './blocks/TextBlock.svelte';

	const FALLBACK_NOTE =
		'Unable to render this structured response; falling back to plain-text projection.';

	export let response: StructuredResponseV1;
	export let sessionId: string | null = null;
	export let onAction:
		| ((action: StructuredResponseActionV1, sessionId: string | null) => void | Promise<void>)
		| null = null;

	let validationReason: string | null = null;
	let isRenderable = true;
	let fallbackText = FALLBACK_NOTE;

	$: {
		try {
			const validation = validateStructuredResponse(response);
			if (!validation.ok) {
				isRenderable = false;
				validationReason = validation.reason;
				fallbackText = `${FALLBACK_NOTE} ${validation.reason}`;
			} else if (!isStructuredResponseV1(response)) {
				isRenderable = false;
				validationReason = 'response did not satisfy required structure';
				fallbackText = `${FALLBACK_NOTE} Invalid response contract.`;
			} else {
				isRenderable = true;
				validationReason = null;
				fallbackText = toPlainText(response);
			}
		} catch (error) {
			isRenderable = false;
			validationReason = error instanceof Error ? error.message : 'render_failure';
			fallbackText = `${FALLBACK_NOTE} ${validationReason}`;
		}
	}

	function actionPresentation(action: StructuredResponseActionV1) {
		return presentationForAction(action);
	}

	function toneClass(tone: string | undefined): string {
		if (tone === 'success' || tone === 'warning' || tone === 'danger' || tone === 'info') {
			return tone;
		}
		return 'neutral';
	}

	async function handleAction(action: StructuredResponseActionV1): Promise<void> {
		if (!onAction) return;
		if (!isActionRenderable(action)) return;
		await onAction(action, sessionId);
	}
</script>

{#if !isRenderable}
	<section class="sr-fallback" role="status" aria-live="polite" data-testid="sr-fallback">
		<p class="sr-fallback__title">{FALLBACK_NOTE}</p>
		{#if validationReason}
			<p class="sr-fallback__reason">{validationReason}</p>
		{/if}
		<p class="sr-fallback__plain">{fallbackText}</p>
	</section>
{:else}
	<article class={`sr-response sr-response--${toneClass(response.tone)}`} data-testid="sr-renderer">
		{#if response.title}
			<header class="sr-response__title">{response.title}</header>
		{/if}
		{#if response.summary}
			<p class="sr-response__summary">{response.summary}</p>
		{/if}
		<div class="sr-response__blocks">
			{#each response.blocks as block, index (index)}
				{#if block.kind === 'markdown'}
					<MarkdownBlock {sessionId} {block} />
				{:else if block.kind === 'text'}
					<TextBlock {block} />
				{:else if block.kind === 'callout'}
					<CalloutBlock {block} />
				{:else if block.kind === 'key_values'}
					<KeyValuesBlock {block} />
				{:else if block.kind === 'table'}
					<TableBlock {block} />
				{:else if block.kind === 'list'}
					<ListBlock {block} />
				{:else if block.kind === 'artifacts'}
					<ArtifactsBlock {block} onAction={onAction ? handleAction : null} />
				{:else if block.kind === 'sources'}
					<SourcesBlock {block} onAction={onAction ? handleAction : null} />
				{:else if block.kind === 'metrics'}
					<MetricsBlock {block} />
				{/if}
			{/each}
		</div>
		{#if response.actions && response.actions.length > 0}
			<div class="sr-response__actions">
				{#each response.actions as action}
					{@const present = actionPresentation(action)}
					{@const active = !!onAction && isActionRenderable(action)}
					<button
						type="button"
						class="sr-action"
					disabled={!active}
					title={
						`Action type "${present.description}" is currently unavailable`
					}
						on:click={() => {
							if (active) {
								void handleAction(action);
							}
						}}
					>
						{present.label}
					</button>
				{/each}
			</div>
		{/if}
	</article>
{/if}

<style>
	.sr-fallback {
		padding: 0.65rem;
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
		background: color-mix(in srgb, var(--card) 60%, transparent);
	}

	.sr-fallback__title,
	.sr-fallback__reason,
	.sr-fallback__plain {
		margin: 0;
		font-size: var(--text-sm);
	}

	.sr-fallback__title {
		font-weight: 650;
	}

	.sr-fallback__reason {
		color: var(--text-muted);
		margin-top: 0.35rem;
	}

	.sr-fallback__plain {
		margin-top: 0.25rem;
	}

	.sr-response {
		display: grid;
		gap: 0.5rem;
		padding: 0.7rem;
		border: 1px solid var(--border);
		border-radius: var(--radius-sm);
		border-left-width: 0.42rem;
	}

	.sr-response--neutral {
		border-left-color: var(--text-muted);
	}

	.sr-response--info {
		border-left-color: var(--info);
	}

	.sr-response--success {
		border-left-color: var(--success);
	}

	.sr-response--warning {
		border-left-color: var(--warning);
	}

	.sr-response--danger {
		border-left-color: var(--error);
	}

	.sr-response__title {
		margin: 0;
		font-size: var(--text-base);
		font-weight: 700;
	}

	.sr-response__summary {
		margin: 0;
		color: var(--text-muted);
		line-height: 1.4;
	}

	.sr-response__blocks {
		display: grid;
		gap: 0.5rem;
	}

	.sr-response__actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.45rem;
		padding-top: 0.15rem;
		border-top: 1px dashed var(--border);
	}

	.sr-action {
		font-size: var(--text-sm);
		border-radius: 0.45rem;
		padding: 0.3rem 0.6rem;
	}
</style>
