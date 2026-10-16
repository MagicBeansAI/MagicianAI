<script lang="ts" context="module">
	let gameSectionSequence = 0;

	function nextGameSectionId(): string {
		gameSectionSequence += 1;
		return `game-section-${gameSectionSequence}`;
	}
</script>

<script lang="ts">
	import '../game-chrome.css';

	export let title: string;
	export let description = '';
	export let level: 2 | 3 | 4 = 3;
	export let compact = false;
	export let className = '';

	const titleId = nextGameSectionId();
	$: headingTag = `h${level}` as 'h2' | 'h3' | 'h4';
</script>

<section
	{...$$restProps}
	class={`game-ui-section ${className}`.trim()}
	data-compact={compact}
	aria-labelledby={titleId}
>
	<header class="game-ui-section__header">
		<div>
			<svelte:element this={headingTag} id={titleId} class="game-ui-section__title">{title}</svelte:element>
			{#if description}<p class="game-ui-section__description">{description}</p>{/if}
		</div>
		{#if $$slots.actions}<div class="game-ui-section__actions"><slot name="actions" /></div>{/if}
	</header>
	<slot />
</section>
