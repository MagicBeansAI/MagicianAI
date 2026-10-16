<script lang="ts">
	import StorageGovernancePanel from '$lib/storage/StorageGovernancePanel.svelte';
	import { fetchStorageActivation } from '$lib/storage/storageGovernance';
</script>

<svelte:head>
	<title>Storage · Magican</title>
</svelte:head>

<StorageGovernancePanel loadActivation={fetchStorageActivation} />
