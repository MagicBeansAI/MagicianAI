<script lang="ts">
	// Fills LiveTaskCard's `activity` slot the way ChatPanel does, echoing the
	// slot props so tests can see which task/run the host would stream.
	// `slotText` overrides that echo; '' renders an empty slot (a live task
	// whose activity has produced nothing yet).
	import type { ChatMessage, ChatRenderTaskExecutionGroup } from '$lib/stores/chatStore';
	import LiveTaskCard from '$lib/magician/chat/next/LiveTaskCard.svelte';

	export let message: ChatMessage;
	export let taskExecutionGroups: ChatRenderTaskExecutionGroup[] = [];
	export let slotText: string | undefined = undefined;
	export let onCancel: ((taskId: string) => void | Promise<void>) | undefined = undefined;

	const noop = () => {};
</script>

<LiveTaskCard
	{message}
	{taskExecutionGroups}
	{onCancel}
	onToggleRun={noop}
	onOpenTask={noop}
	onInspectFromIds={noop}
	onWatchLive={noop}
	onStopWatching={noop}
>
	<svelte:fragment slot="activity" let:taskId let:executionId>
		<div data-testid="activity-slot">{slotText ?? `${taskId}:${executionId ?? ''}`}</div>
	</svelte:fragment>
</LiveTaskCard>
