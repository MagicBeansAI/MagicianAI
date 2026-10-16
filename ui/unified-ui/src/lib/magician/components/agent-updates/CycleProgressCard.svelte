<script lang="ts">
	import type { AgentUpdate, CycleOutcome } from '$lib/types/agentUpdate';
	import Card from '../generative/Card.svelte';
	import Badge from '../generative/Badge.svelte';
	import OpenInStreamLink from '../OpenInStreamLink.svelte';

	export let event: AgentUpdate;

	function outcomeColor(outcome: CycleOutcome): 'success' | 'warning' | 'error' | 'info' {
		switch (outcome) {
			case 'succeeded':
				return 'success';
			case 'failed':
				return 'error';
			case 'paused':
				return 'warning';
			case 'partially_succeeded':
				return 'info';
		}
	}

	function outcomeLabel(outcome: CycleOutcome): string {
		switch (outcome) {
			case 'succeeded':
				return 'Succeeded';
			case 'failed':
				return 'Failed';
			case 'paused':
				return 'Paused';
			case 'partially_succeeded':
				return 'Partial';
		}
	}

	function formatDuration(ms: number): string {
		if (ms < 1000) return `${ms} ms`;
		const seconds = ms / 1000;
		if (seconds < 60) return `${seconds.toFixed(1)}s`;
		const minutes = seconds / 60;
		if (minutes < 60) return `${minutes.toFixed(1)} min`;
		return `${(minutes / 60).toFixed(1)} h`;
	}
</script>

{#if event.kind === 'cycle_started'}
	<Card
		title="Cycle started"
		subtitle={event.focus_area ? `Focus: ${event.focus_area}` : `Trigger: ${event.trigger}`}
	>
		<Badge text="Running" color="info" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{:else if event.kind === 'cycle_completed'}
	<Card title="Cycle completed" subtitle={formatDuration(event.duration_ms)}>
		<Badge text={outcomeLabel(event.outcome)} color={outcomeColor(event.outcome)} />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{:else if event.kind === 'cycle_failed'}
	<Card title="Cycle failed" subtitle={formatDuration(event.duration_ms)} body={event.error}>
		<Badge text="Failed" color="error" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{:else if event.kind === 'cycle_paused'}
	<Card title="Cycle paused" body={event.reason}>
		<Badge text="Paused" color="warning" />
		<OpenInStreamLink agentId={event.agent_id} timestampMs={event.ts} />
	</Card>
{/if}
