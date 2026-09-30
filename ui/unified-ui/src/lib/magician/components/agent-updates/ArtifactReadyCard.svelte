<script lang="ts">
	import type { AgentUpdate } from '$lib/types/agentUpdate';
	import Card from '../generative/Card.svelte';
	import Badge from '../generative/Badge.svelte';
	import OpenInStreamLink from '../OpenInStreamLink.svelte';

	export let event: AgentUpdate;
</script>

{#if event.kind === 'artifact_created'}
	<Card
		title={event.artifact.label ?? 'Artifact ready'}
		subtitle={event.artifact.kind}
		body={event.artifact.surface_url ?? event.artifact.artifact_id}
	>
		{#if event.artifact.surface_url}
			<a class="muij-link" href={event.artifact.surface_url}>Open</a>
		{/if}
		<Badge text="Ready" color="success" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{:else if event.kind === 'artifact_create_failed'}
	<Card title="Artifact failed" subtitle={event.attempted_kind} body={event.error}>
		<Badge text="Failed" color="error" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{/if}

<style>
	.muij-link {
		color: var(--color-info, #3b82f6);
		text-decoration: underline;
		margin-right: var(--space-sm, 8px);
	}
</style>
