<script lang="ts">
	import type { AgentUpdate } from '$lib/types/agentUpdate';
	import Card from '../generative/Card.svelte';
	import Badge from '../generative/Badge.svelte';
	import OpenInStreamLink from '../OpenInStreamLink.svelte';

	export let event: AgentUpdate;
</script>

{#if event.kind === 'goal_completed'}
	<Card
		title="Goal completed"
		subtitle={`Goal ${event.goal_id}`}
		body={event.artifacts && event.artifacts.length > 0
			? `${event.artifacts.length} artifact${event.artifacts.length === 1 ? '' : 's'}`
			: undefined}
	>
		<Badge text="Completed" color="success" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{:else if event.kind === 'goal_failed'}
	<Card title="Goal failed" subtitle={`Goal ${event.goal_id}`} body={event.error}>
		<Badge text="Failed" color="error" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{:else if event.kind === 'goal_recovered'}
	<Card
		title="Goal recovered"
		subtitle={`Goal ${event.goal_id}`}
		body={event.recovery_strategy}
	>
		<Badge text="Recovered" color="info" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{/if}
