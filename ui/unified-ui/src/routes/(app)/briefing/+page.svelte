<script lang="ts">
	import { page } from '$app/stores';

	import PublishedScrollCanvas from '$lib/magician/components/PublishedScrollCanvas.svelte';

	$: routeTaskFilter = (($page.url.searchParams.get('task_id') || '').trim() || undefined);
	$: routeAgentFilter = (($page.url.searchParams.get('agent_id') || '').trim() || undefined);
</script>

<svelte:head>
	<title>Briefing · Magican</title>
</svelte:head>

<div class="briefing-page">
	<PublishedScrollCanvas
		routeTarget="/briefing"
		scopeTaskId={routeTaskFilter}
		scopeAgentId={routeAgentFilter}
		heading="Briefing canvas"
		subheading="Recurring briefings and delivery scrolls, rendered as a full dashboard canvas."
	/>
</div>

<style>
	.briefing-page {
		/* Share the app-wide content width (--app-content-max) for visual
		   consistency across primary pages. width: 100% ensures the column
		   stays at its max during loading too — was narrowing briefly in
		   v0.0.236-0.0.238 because the canvas's intrinsic content width
		   during skeleton state was less than the cap. */
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 0.85rem 1rem 1rem;
		box-sizing: border-box;
	}
</style>
