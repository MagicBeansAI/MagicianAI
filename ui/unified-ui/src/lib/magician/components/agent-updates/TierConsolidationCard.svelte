<script lang="ts">
	import type { AgentUpdate } from '$lib/types/agentUpdate';
	import Card from '../generative/Card.svelte';
	import Badge from '../generative/Badge.svelte';
	import OpenInStreamLink from '../OpenInStreamLink.svelte';

	export let event: AgentUpdate;
</script>

{#if event.kind === 'tier_consolidated'}
	<Card
		title={`${event.source} → ${event.target}`}
		subtitle={`Rule: ${event.rule}`}
		body={`${event.records_written} record${event.records_written === 1 ? '' : 's'} written`}
	>
		<Badge text="Consolidated" color="info" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{/if}
