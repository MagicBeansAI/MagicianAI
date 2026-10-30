<script lang="ts">
    import { page } from '$app/stores';
    import ChatFilesPage from '$lib/magician/chat/components/ChatFilesPage.svelte';
</script>
<ChatFilesPage sessionId={$page.url.searchParams.get('session')} threadId={$page.params.name} />
