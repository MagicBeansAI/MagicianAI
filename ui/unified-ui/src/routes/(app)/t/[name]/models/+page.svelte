<script lang="ts">
    import { page } from '$app/stores';
    import ChatModelsPage from '$lib/magician/chat/components/ChatModelsPage.svelte';
</script>
<ChatModelsPage sessionId={$page.url.searchParams.get('session')} />
