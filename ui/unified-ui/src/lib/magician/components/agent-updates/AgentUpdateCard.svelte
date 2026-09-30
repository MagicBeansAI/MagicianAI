<script lang="ts">
	import type { AgentUpdate } from '$lib/types/agentUpdate';
	import { getCardForKind } from './registry';

	export let event: AgentUpdate;

	$: component = getCardForKind(event.kind);
</script>

<svelte:component this={component} {event} />
