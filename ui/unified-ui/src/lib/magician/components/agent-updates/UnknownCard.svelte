<script lang="ts">
	import type { AgentUpdate } from '$lib/types/agentUpdate';
	import Card from '../generative/Card.svelte';
	import Badge from '../generative/Badge.svelte';

	export let event: AgentUpdate;

	$: payload = serialize(event);

	function serialize(value: AgentUpdate): string {
		try {
			return JSON.stringify(value, null, 2);
		} catch {
			return '[unserializable]';
		}
	}
</script>

<Card title={`Update: ${event.kind}`} subtitle="No registered card for this kind">
	<pre class="muij-unknown-payload">{payload}</pre>
	<Badge text="Unhandled" color="default" />
</Card>

<style>
	.muij-unknown-payload {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		white-space: pre-wrap;
		word-break: break-word;
		background: var(--bg-soft);
		padding: var(--space-sm, 8px);
		border-radius: var(--radius-sm, 4px);
		max-height: 200px;
		overflow: auto;
		margin: var(--space-xs, 4px) 0;
	}
</style>
