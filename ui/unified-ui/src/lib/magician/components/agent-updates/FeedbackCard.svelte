<script lang="ts">
	import type { AgentUpdate } from '$lib/types/agentUpdate';
	import Card from '../generative/Card.svelte';
	import Badge from '../generative/Badge.svelte';
	import OpenInStreamLink from '../OpenInStreamLink.svelte';

	export let event: AgentUpdate;
</script>

{#if event.kind === 'feedback_generated'}
	<Card
		title="Feedback generated"
		subtitle={`${event.corrections} correction${event.corrections === 1 ? '' : 's'} · ${event.success_patterns} pattern${event.success_patterns === 1 ? '' : 's'}`}
	>
		<Badge text="Generated" color="info" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{/if}
