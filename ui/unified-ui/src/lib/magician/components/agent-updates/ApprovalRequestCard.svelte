<script lang="ts">
	import type { AgentUpdate } from '$lib/types/agentUpdate';
	import Card from '../generative/Card.svelte';
	import Badge from '../generative/Badge.svelte';
	import OpenInStreamLink from '../OpenInStreamLink.svelte';

	export let event: AgentUpdate;

	$: paramsPreview =
		event.kind === 'approval_requested' && event.params !== undefined && event.params !== null
			? previewJson(event.params)
			: '';

	function previewJson(value: unknown): string {
		try {
			const full = JSON.stringify(value);
			return full.length > 120 ? `${full.slice(0, 117)}…` : full;
		} catch {
			return '[unserializable]';
		}
	}
</script>

{#if event.kind === 'approval_requested'}
	{@const subtitle =
		event.tool && event.action
			? `${event.tool} · ${event.action}`
			: event.tool
				? event.tool
				: event.pending_action_count !== undefined
					? `${event.pending_action_count} action${event.pending_action_count === 1 ? '' : 's'} pending`
					: `Request ${event.approval_id}`}
	<Card title="Approval requested" {subtitle} body={paramsPreview}>
		<Badge text="Pending" color="warning" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{:else if event.kind === 'approval_resolved'}
	<Card
		title="Approval resolved"
		subtitle={`By ${event.resolver}`}
	>
		<Badge
			text={event.decision === 'approve' ? 'Approved' : 'Rejected'}
			color={event.decision === 'approve' ? 'success' : 'error'}
		/>
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{:else if event.kind === 'approval_expired'}
	<Card
		title="Approval expired"
		subtitle={`Request ${event.approval_id}`}
	>
		<Badge text="Expired" color="default" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{/if}
