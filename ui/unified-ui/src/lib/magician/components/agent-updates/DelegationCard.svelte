<script lang="ts">
	import type { AgentUpdate } from '$lib/types/agentUpdate';
	import Card from '../generative/Card.svelte';
	import Badge from '../generative/Badge.svelte';
	import OpenInStreamLink from '../OpenInStreamLink.svelte';

	export let event: AgentUpdate;

	function outcomeColor(outcome: string): 'success' | 'warning' | 'error' | 'info' | 'default' {
		const normalized = outcome.toLowerCase();
		if (normalized === 'completed' || normalized === 'success' || normalized === 'succeeded')
			return 'success';
		if (normalized === 'failed' || normalized === 'error' || normalized === 'rejected')
			return 'error';
		if (normalized === 'cancelled' || normalized === 'timeout') return 'warning';
		return 'info';
	}
</script>

{#if event.kind === 'delegation_issued'}
	<Card
		title={`${event.from_agent} → ${event.to_agent}`}
		subtitle={`Task ${event.task_id}`}
	>
		<Badge text="Issued" color="info" />
		<OpenInStreamLink
			agentId={event.agent_id}
			taskId={event.task_id}
			timestampMs={event.ts}
		/>
	</Card>
{:else if event.kind === 'delegation_resolved'}
	<Card
		title={`${event.from_agent} → ${event.to_agent}`}
		subtitle={`Task ${event.task_id}`}
		body={event.outcome}
	>
		<Badge text={event.outcome} color={outcomeColor(event.outcome)} />
		<OpenInStreamLink
			agentId={event.agent_id}
			taskId={event.task_id}
			timestampMs={event.ts}
		/>
	</Card>
{/if}
