<script lang="ts">
	import type { StructuredTextBlockV1 } from '../types';

	export let block: StructuredTextBlockV1;
</script>

<section class="sr-block sr-block--text" data-block-kind="text">
	{#if block.title}
		<h4 class="sr-block__title">{block.title}</h4>
	{/if}
	<p class="sr-block__text">{block.text}</p>
</section>

<style>
	.sr-block {
		display: block;
	}

	.sr-block__title {
		font-size: var(--text-sm);
		font-weight: 650;
		margin: 0 0 0.35rem;
	}

	.sr-block__text {
		margin: 0;
		line-height: 1.45;
		white-space: pre-wrap;
		word-wrap: break-word;
	}
</style>
