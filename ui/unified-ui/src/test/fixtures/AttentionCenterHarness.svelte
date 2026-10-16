<script lang="ts">
	import { onMount } from 'svelte';
	import AttentionCenter from '$lib/attention/AttentionCenter.svelte';
	import { openAttentionCenter, openAttentionItem } from '$lib/attention/centerState';
	import AttentionPromptModal from '$lib/magician/components/AttentionPromptModal.svelte';
	import { mountEscapeHandler } from '$lib/shell/overlayCoordinator';

	export let exactItemId = '';

	onMount(() => mountEscapeHandler());
</script>

<button type="button" data-attention-trigger on:click={openAttentionCenter}>Open Attention</button>
{#if exactItemId}
	<button type="button" on:click={() => openAttentionItem(exactItemId)}>Open exact item</button>
{/if}

<AttentionCenter />
<AttentionPromptModal />
