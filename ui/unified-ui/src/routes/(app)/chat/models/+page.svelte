<script lang="ts">
    import { page } from '$app/stores';
    import { taskStore } from '$lib/stores/taskStore';
    import ThreadSessionBar from '$lib/threads/ThreadSessionBar.svelte';
    import ChatWorkspacePage from '$lib/threads/ChatWorkspacePage.svelte';
    import ChatModelsPage from '$lib/magician/chat/components/ChatModelsPage.svelte';
    $: sessionId = $page.url.searchParams.get('session');
    $: threadId = $page.url.searchParams.get('thread') || 'general';
    $: query = new URLSearchParams({ ...(sessionId ? { session: sessionId } : {}), ...(threadId !== 'general' ? { thread: threadId } : {}) }).toString();
</script>
<ChatWorkspacePage {threadId}>
    <ThreadSessionBar slot="header" {threadId} {sessionId} activeTab="models" tasks={$taskStore.tasks} chatHref={`/chat${query ? `?${query}` : ''}`} filesHref={`/chat/files${query ? `?${query}` : ''}`} modelsHref={`/chat/models${query ? `?${query}` : ''}`} />
    <ChatModelsPage {sessionId} />
</ChatWorkspacePage>
