<script lang="ts">
	import type { AgentUpdate } from '$lib/types/agentUpdate';
	import Card from '../generative/Card.svelte';
	import Badge from '../generative/Badge.svelte';
	import OpenInStreamLink from '../OpenInStreamLink.svelte';

	export let event: AgentUpdate;

	function formatChanges(fields: string[]): string {
		if (fields.length === 0) return '';
		if (fields.length <= 3) return fields.join(', ');
		return `${fields.slice(0, 3).join(', ')} +${fields.length - 3}`;
	}
</script>

{#if event.kind === 'agent_created'}
	<Card title={`Agent created: ${event.name}`} subtitle={`kind: ${event.agent_kind}`}>
		<Badge text="Created" color="success" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{:else if event.kind === 'agent_updated'}
	<Card
		title="Agent updated"
		subtitle={`Agent ${event.agent_id}`}
		body={formatChanges(event.changed_fields)}
	>
		<Badge text="Updated" color="info" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{:else if event.kind === 'agent_deleted'}
	<Card title="Agent deleted" subtitle={`Agent ${event.agent_id}`}>
		<Badge text="Deleted" color="error" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{:else if event.kind === 'agent_paused'}
	<Card
		title="Agent paused"
		subtitle={`Agent ${event.agent_id}`}
		body={event.reason}
	>
		<Badge text="Paused" color="warning" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{:else if event.kind === 'agent_resumed'}
	<Card title="Agent resumed" subtitle={`Agent ${event.agent_id}`}>
		<Badge text="Resumed" color="success" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{/if}
