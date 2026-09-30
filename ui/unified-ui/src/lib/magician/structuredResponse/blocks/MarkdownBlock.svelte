<script lang="ts">
	import ChatMarkdown from '$lib/magician/components/chat/ChatMarkdown.svelte';
	import type { StructuredMarkdownBlockV1 } from '../types';

	export let block: StructuredMarkdownBlockV1;
	export let sessionId: string | null = null;
</script>

<section class="sr-block sr-block--markdown" data-block-kind="markdown">
	{#if block.title}
		<h4 class="sr-block__title" id={`sr-markdown-${block.kind}-${block.text.slice(0, 12).replace(/\W+/g, '-').toLowerCase()}`}>
			{block.title}
		</h4>
	{/if}
	<div class="sr-block__content">
		<ChatMarkdown content={block.text} sessionId={sessionId} />
	</div>
</section>

<style>
	.sr-block {
		display: block;
	}

	.sr-block__title {
		font-size: var(--text-sm);
		font-weight: 650;
		margin: 0 0 0.5rem;
	}

	.sr-block__content :global(p) {
		margin: 0;
		line-height: 1.45;
	}
</style>
