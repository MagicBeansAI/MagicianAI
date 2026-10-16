<script lang="ts">
	import type { StructuredKeyValuesBlockV1 } from '../types';

	export let block: StructuredKeyValuesBlockV1;
</script>

<section class="sr-block sr-block--kv" data-block-kind="key_values">
	{#if block.title}<h4 class="sr-block__title">{block.title}</h4>{/if}
	<ul class="sr-kv-list">
		{#each block.items as item}
			<li>
				<span class="sr-kv-label">{item.label}</span>
				<span class="sr-kv-separator">:</span>
				<span class="sr-kv-value">{item.value}</span>
				{#if item.hint}
					<span class="sr-kv-hint">({item.hint})</span>
				{/if}
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
		margin: 0 0 0.4rem;
	}

	.sr-kv-list {
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.sr-kv-list li {
		display: grid;
		grid-template-columns: minmax(120px, 180px) auto 1fr;
		gap: 0.2rem 0.4rem;
		padding: 0.25rem 0;
		line-height: 1.35;
		font-size: var(--text-sm);
	}

	.sr-kv-label {
		font-weight: 600;
		color: var(--text-muted);
	}

	.sr-kv-separator {
		color: var(--text);
	}

	.sr-kv-value {
		font-weight: 500;
	}

	.sr-kv-hint {
		color: var(--text-muted);
		font-size: var(--text-xs);
	}

	@media (max-width: 720px) {
		.sr-kv-list li {
			grid-template-columns: 1fr;
		}
	}
</style>
