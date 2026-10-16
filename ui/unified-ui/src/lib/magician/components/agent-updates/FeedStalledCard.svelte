<script lang="ts">
	import type { AgentUpdate } from '$lib/types/agentUpdate';
	import Card from '../generative/Card.svelte';
	import Badge from '../generative/Badge.svelte';
	import OpenInStreamLink from '../OpenInStreamLink.svelte';

	export let event: AgentUpdate;

	function formatSilent(ms: number): string {
		if (ms < 60_000) return `${Math.round(ms / 1000)}s`;
		if (ms < 3_600_000) return `${Math.round(ms / 60_000)}m`;
		return `${(ms / 3_600_000).toFixed(1)}h`;
	}
</script>

{#if event.kind === 'feed_stalled'}
	<Card
		title="Feed stalled"
		subtitle={event.cycle_id ? `Cycle ${event.cycle_id}` : 'Agent silent'}
		body={`No events for ${formatSilent(event.silent_for_ms)} — agent may have died silently.`}
	>
		<Badge text="Stalled" color="warning" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{/if}
