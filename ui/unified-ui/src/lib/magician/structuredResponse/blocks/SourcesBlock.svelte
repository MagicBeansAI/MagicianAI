<script lang="ts">
	import type { StructuredResponseActionV1, StructuredSourcesBlockV1 } from '../types';

	export let block: StructuredSourcesBlockV1;
	export let onAction: ((action: StructuredResponseActionV1) => void | Promise<void>) | null = null;
</script>

<section class="sr-block sr-block--sources" data-block-kind="sources">
	{#if block.title}<h4 class="sr-block__title">{block.title}</h4>{/if}
	<ul class="sr-sources">
		{#each block.items as source}
			<li>
				<button
					type="button"
					class="sr-link"
					disabled={!onAction}
					on:click={() => onAction && void onAction({ kind: 'open_url', label: source.label, url: source.href })}
				>
					{source.label}
				</button>
			</li>
		{/each}
	</ul>
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

	.sr-sources {
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.sr-link {
		padding: 0;
		border: 0;
		background: transparent;
		color: var(--link);
		text-underline-offset: 2px;
		word-break: break-word;
		cursor: pointer;
	}

	.sr-link:disabled {
		color: inherit;
		cursor: default;
	}

	.sr-sources li {
		padding: 0.2rem 0;
		line-height: 1.35;
	}
</style>
