<script lang="ts">
	import { page } from '$app/stores';
	import CrewDefinitionWorkspace from '$lib/magician/crew/CrewDefinitionWorkspace.svelte';
</script>

<CrewDefinitionWorkspace agentId={$page.params.id} />
