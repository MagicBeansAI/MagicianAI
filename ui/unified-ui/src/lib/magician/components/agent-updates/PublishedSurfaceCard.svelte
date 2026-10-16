<script lang="ts">
	import type { AgentUpdate } from '$lib/types/agentUpdate';
	import Card from '../generative/Card.svelte';
	import Badge from '../generative/Badge.svelte';
	import OpenInStreamLink from '../OpenInStreamLink.svelte';

	export let event: AgentUpdate;
</script>

{#if event.kind === 'published_surface_changed'}
	<Card
		title="Surface published"
		subtitle={`ID: ${event.surface_id}`}
		body={event.url}
	>
		{#if event.url}
			<a class="muij-link" href={event.url}>Open</a>
		{/if}
		<Badge text="Published" color="success" />
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
