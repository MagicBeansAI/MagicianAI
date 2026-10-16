<script lang="ts">
	import { manifestoInline, type ManifestoBlock } from './manifesto';

	export let blocks: readonly ManifestoBlock[];
</script>

{#each blocks as block, i (i)}
	{#if block.kind === 'prose'}
		<p class="prose">
			{#each manifestoInline(block.text) as run, j (`${i}-${j}`)}
				{#if run.strong}<strong>{run.text}</strong>{:else}{run.text}{/if}
			{/each}
		</p>
	{:else}
		<div class="verse">
			{#each block.lines as line, k (`${i}-v-${k}`)}
				<p>
					{#each manifestoInline(line) as run, j (`${i}-v-${k}-${j}`)}
						{#if run.strong}<strong>{run.text}</strong>{:else}{run.text}{/if}
					{/each}
				</p>
			{/each}
		</div>
	{/if}
{/each}

<style>
	.prose,
	.verse {
		margin: 0 0 1.5rem;
		font-size: clamp(1.04rem, 1.75vw, 1.14rem);
		line-height: 1.68;
	}

	.verse p {
		margin: 0;
		font-size: inherit;
		line-height: 1.45;
	}

	strong {
		font-weight: 650;
	}
</style>
