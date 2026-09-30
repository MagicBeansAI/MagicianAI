<script lang="ts">
	import { page } from '$app/stores';
	import AppSurfacePage from '$lib/apps/AppSurfacePage.svelte';

	$: installationId = $page.params.installation ?? '';
	$: surfacePath = $page.params.surface ?? '';
</script>

<AppSurfacePage {installationId} {surfacePath} />
