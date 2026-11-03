<script lang="ts">
    import { page } from '$app/stores';
    import ChatFilesPage from '$lib/magician/chat/components/ChatFilesPage.svelte';
    import { getThreadPageContext } from '$lib/threads/threadPageContext';

    // The route param is a slug or alias; the shell resolves it to the id.
    const threadPage = getThreadPageContext();
</script>
<ChatFilesPage sessionId={$page.url.searchParams.get('session')} threadId={$threadPage.threadName} />
