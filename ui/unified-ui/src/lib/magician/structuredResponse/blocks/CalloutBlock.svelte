<script lang="ts">
	import type { StructuredCalloutBlockV1 } from '../types';

	export let block: StructuredCalloutBlockV1;
</script>

<section class={`sr-callout sr-callout--${block.tone}`} data-block-kind="callout">
	{#if block.title}
		<div class="sr-callout__title" id={`sr-callout-${block.title.slice(0, 20).replace(/\W+/g, '-').toLowerCase()}`}>
			{block.title}
		</div>
	{/if}
	<div class="sr-callout__text">{block.text}</div>
</section>

<style>
	.sr-callout {
		border: 1px solid var(--border);
		border-left-width: 0.4rem;
		border-radius: var(--radius-sm);
		padding: 0.55rem 0.65rem;
	}

	.sr-callout--info {
		background: rgba(59, 130, 246, 0.08);
		border-left-color: rgb(59, 130, 246);
	}

	.sr-callout--success {
		background: rgba(16, 185, 129, 0.08);
		border-left-color: rgb(16, 185, 129);
	}

	.sr-callout--warning {
		background: rgba(245, 158, 11, 0.1);
		border-left-color: rgb(245, 158, 11);
	}

	.sr-callout--danger {
		background: rgba(239, 68, 68, 0.09);
		border-left-color: rgb(239, 68, 68);
	}

	.sr-callout__title {
		font-weight: 650;
		font-size: var(--text-sm);
		margin: 0 0 0.25rem;
	}

	.sr-callout__text {
		font-size: var(--text-sm);
		line-height: 1.4;
	}
</style>
