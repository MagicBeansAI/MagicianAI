<script lang="ts">
	import ChatContentBlocks from '$lib/magician/components/chat/ChatContentBlocks.svelte';
	import { getMessageContentBlocks, type ChatMessage } from '$lib/stores/chatStore';

	export let message: ChatMessage;
	// Rendered timestamp — formatTimestamp stays in ChatPanel (the
	// user/assistant bubbles there share it), so the parent passes the
	// formatted string.
	export let timeLabel: string;
</script>

<div class="chat chat-end">
	<div class="chat-header">
		You
		<time class="chat-msg-time">{timeLabel}</time>
	</div>
	<div class="chat-bubble chat-bubble-neutral chat-bubble-attachment">
		<ChatContentBlocks
			sessionId={message.session_id}
			blocks={getMessageContentBlocks(message.content)}
		/>
	</div>
</div>

<style>
	/* Duplicate of ChatPanel's scoped .chat-msg-time — Svelte scoping
	   doesn't cross the component boundary and the parent rule still
	   serves the user/assistant bubbles there. */
	.chat-msg-time {
		font-weight: 400;
		opacity: 0.7;
		font-size: var(--text-2xs);
	}

	.chat-bubble-attachment {
		display: block;
	}

	.chat-bubble-attachment :global(.chat-rich-blocks) {
		margin-top: 0;
	}
</style>
