<script lang="ts">
	import type { AgentUpdate } from '$lib/types/agentUpdate';
	import Card from '../generative/Card.svelte';
	import Badge from '../generative/Badge.svelte';
	import OpenInStreamLink from '../OpenInStreamLink.svelte';

	export let event: AgentUpdate;

	$: stats = event.kind === 'memory_report'
		? `${event.episodes_processed} episode${event.episodes_processed === 1 ? '' : 's'} · ${event.corrections_applied} correction${event.corrections_applied === 1 ? '' : 's'}`
		: '';
</script>

{#if event.kind === 'memory_report'}
	<Card title="Memory report" subtitle={stats} body={event.summary}>
		<Badge text="Processed" color="info" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{/if}
