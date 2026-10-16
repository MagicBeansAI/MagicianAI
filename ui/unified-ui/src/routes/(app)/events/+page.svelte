<script lang="ts">
	/**
	 * /events — unified live event stream (full density).
	 *
	 * Thin wrapper around <EventStreamCard /> with no scope filter, so the
	 * server-side cross-scope backfill kicks in (24h history across the
	 * scope's task tree). The same component is used in compact mode inside
	 * ExecutionPanel for execution-scoped tails.
	 *
	 * Reads `?execution_id`, `?task_id`, and `?t=<ms>` query params so that
	 * the Phase 4 "Open in stream" deep-links from feed/step/attention
	 * surfaces land scoped + time-anchored.
	 *
	 * See `docs/archive/plans/2026-05-10-execution-panel-canvas-redesign.md`.
	 */
	import { page } from '$app/stores';
	import EventStreamCard from '$lib/realtime/EventStreamCard.svelte';

	$: query = $page.url.searchParams;
	$: executionIdParam = query.get('execution_id');
	$: taskIdParam = query.get('task_id');
	$: agentIdParam = query.get('agent_id');
	$: anchorParam = parseAnchor(query.get('t'));

	function parseAnchor(raw: string | null): number | null {
		if (!raw) return null;
		const n = Number(raw);
		return Number.isFinite(n) && n > 0 ? n : null;
	}
</script>

<svelte:head>
	<title>Events · Magican</title>
</svelte:head>

<div class="events-page">
	<EventStreamCard
		title="Event stream"
		density="full"
		maxHeight="calc(100vh - 18rem)"
		executionId={executionIdParam}
		taskId={taskIdParam}
		defaultAgentId={agentIdParam}
		anchorTimestampMs={anchorParam}
	/>
</div>

<style>
	.events-page {
		display: flex;
		flex-direction: column;
		min-height: calc(100vh - 48px - var(--attention-bar-offset, 0px));
		padding: 1rem 1.25rem;
	}
</style>
