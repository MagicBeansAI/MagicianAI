<script lang="ts">
	import ChatMarkdown from '$lib/magician/components/chat/ChatMarkdown.svelte';
	import { getMessageText, type ChatMessage } from '$lib/stores/chatStore';

	export let message: ChatMessage;
</script>

<div class="chat-system-message">
	<div class="chat-system-text">
		<ChatMarkdown
			content={getMessageText(message.content)}
			sessionId={message.session_id}
		/>
	</div>
</div>

<style>
	.chat-system-message {
		display: flex;
		justify-content: center;
		padding: 0.5rem 0;
	}

	.chat-system-text {
		display: inline-block;
		font-size: var(--text-2xs);
		line-height: 1.45;
		color: var(--text-muted, #8f9799);
		background: var(--bg-soft, rgba(0, 0, 0, 0.03));
		padding: 0.45rem 0.8rem;
		border-radius: 0.9rem;
		border: 1px solid color-mix(in srgb, var(--text-primary, #111827) 8%, transparent);
		max-width: min(100%, 34rem);
		min-width: 0;
		text-align: left;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		word-break: break-word;
	}
</style>
