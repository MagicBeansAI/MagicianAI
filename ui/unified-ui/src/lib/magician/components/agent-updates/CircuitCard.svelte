<script lang="ts">
	import type { AgentUpdate } from '$lib/types/agentUpdate';
	import Card from '../generative/Card.svelte';
	import Badge from '../generative/Badge.svelte';
	import OpenInStreamLink from '../OpenInStreamLink.svelte';

	export let event: AgentUpdate;

	function formatTime(ms: number | undefined): string {
		if (ms === undefined) return '';
		try {
			return new Date(ms).toLocaleString();
		} catch {
			return String(ms);
		}
	}
</script>

{#if event.kind === 'circuit_opened'}
	<Card
		title="Circuit opened"
		subtitle={event.scope}
		body={event.next_retry_at !== undefined
			? `${event.reason} · retry ${formatTime(event.next_retry_at)}`
			: event.reason}
	>
		<Badge text="Open" color="warning" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{:else if event.kind === 'circuit_recovered'}
	<Card
		title="Circuit recovered"
		subtitle={event.scope}
		body={`Recovered ${formatTime(event.recovered_at)}`}
	>
		<Badge text="Recovered" color="success" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{/if}
