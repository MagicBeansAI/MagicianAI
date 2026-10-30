<script lang="ts">
	/**
	 * An ask's supporting text (its `hint`). A short single line stays a plain
	 * line, as before: a re-ask's reason or an example answer. Anything longer is
	 * what the ask is about (a proposed goal tree, a plan, the notes about to be
	 * written), so it renders as Markdown under a "Details" toggle the reader can
	 * open before answering.
	 */
	import Markdown from '$lib/magician/components/generative/Markdown.svelte';
	import { isDetailedHint } from './hitlDetails';

	export let text: string | null | undefined = '';
	/** Start opened, for surfaces with room to show it (the focused prompt). */
	export let open = false;

	$: value = typeof text === 'string' ? text.trim() : '';
	$: detailed = isDetailedHint(value);
</script>

{#if value && detailed}
	<details class="hitl-details" {open} data-testid="hitl-details">
		<summary>Details</summary>
		<div class="hitl-details__body">
			<Markdown content={value} />
		</div>
	</details>
{:else if value}
	<p class="hitl-hint">{value}</p>
{/if}

<style>
	.hitl-hint {
		margin: 0;
		font-size: 0.78rem;
		line-height: 1.45;
		color: var(--text-secondary);
		white-space: pre-wrap;
	}

	.hitl-details {
		font-size: 0.8rem;
		line-height: 1.45;
		color: var(--text-primary);
	}

	.hitl-details summary {
		cursor: pointer;
		color: var(--text-secondary);
		font-size: 0.78rem;
		user-select: none;
	}

	.hitl-details__body {
		margin-top: 0.4rem;
		max-height: 22rem;
		overflow-y: auto;
		padding-right: 0.25rem;
	}
</style>
