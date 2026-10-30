<script lang="ts">
	import ChatHistoryRail from '$lib/shell/ChatHistoryRail.svelte';
	import { chatHistoryExpanded } from '$lib/shell/shellState';

	export let threadId = 'general';
</script>

<div class="chat-workspace-page">
	<ChatHistoryRail
		bind:expanded={$chatHistoryExpanded}
		threadFilter={threadId === 'general' ? null : threadId}
	/>
	<div class="chat-workspace-column">
		<slot name="header" />
		<slot />
	</div>
</div>

<style>
	.chat-workspace-page {
		display: flex;
		flex: 1;
		width: 100%;
		height: 100%;
		min-width: 0;
		min-height: 0;
		overflow: hidden;
	}

	.chat-workspace-column {
		display: flex;
		flex: 1;
		flex-direction: column;
		min-width: 0;
		min-height: 0;
		max-width: var(--chat-col);
		margin-inline: auto;
		overflow: hidden;
	}
</style>
