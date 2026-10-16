<script lang="ts">
	import ChatPanel from '$lib/magician/chat/ChatPanel.svelte';
	import { getThreadPageContext } from '$lib/threads/threadPageContext';

	// Chat tab of the thread workspace. The shared <ChatPanel/> (also used by
	// /chat) owns all chat chrome — ContextPill, FloatingComposer, sessions,
	// voice, and its own execution inspection — scoped to this thread via the
	// `threadId` prop. The thread shell lives in ../+layout.svelte.
	const threadPage = getThreadPageContext();
	$: threadName = $threadPage.threadName;
	$: threadDisplayName = $threadPage.threadDisplayName;
</script>

<div class="thread-chat-panel">
	<ChatPanel threadId={threadName} embedded={true} pageTitle={`#${threadDisplayName} · Magican`} />
</div>

<style>
	.thread-chat-panel {
		flex: 1 1 auto;
		min-height: 0;
		display: flex;
	}

	.thread-chat-panel :global(.chat-page--embedded) {
		flex: 1 1 auto;
		min-width: 0;
		min-height: 0;
	}
</style>
