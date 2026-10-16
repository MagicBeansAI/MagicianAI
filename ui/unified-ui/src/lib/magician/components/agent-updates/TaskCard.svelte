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

{#if event.kind === 'task_created'}
	<Card title={event.title} subtitle={`Task ${event.task_id}`}>
		<Badge text="New" color="info" />
		<OpenInStreamLink
			agentId={event.agent_id}
			taskId={event.task_id}
			timestampMs={event.ts}
		/>
	</Card>
{:else if event.kind === 'task_updated'}
	<Card
		title="Task updated"
		subtitle={`Task ${event.task_id}`}
		body={formatChanges(event.changed_fields)}
	>
		<Badge text="Updated" color="info" />
		<OpenInStreamLink
			agentId={event.agent_id}
			taskId={event.task_id}
			timestampMs={event.ts}
		/>
	</Card>
{:else if event.kind === 'task_completed'}
	<Card
		title="Task completed"
		subtitle={`Task ${event.task_id}`}
		body={event.artifacts && event.artifacts.length > 0
			? `${event.artifacts.length} artifact${event.artifacts.length === 1 ? '' : 's'}`
			: undefined}
	>
		<Badge text="Completed" color="success" />
		<OpenInStreamLink
			agentId={event.agent_id}
			taskId={event.task_id}
			timestampMs={event.ts}
		/>
	</Card>
{:else if event.kind === 'task_failed'}
	<Card title="Task failed" subtitle={`Task ${event.task_id}`} body={event.error}>
		<Badge text="Failed" color="error" />
		<OpenInStreamLink
			agentId={event.agent_id}
			taskId={event.task_id}
			timestampMs={event.ts}
		/>
	</Card>
{/if}
