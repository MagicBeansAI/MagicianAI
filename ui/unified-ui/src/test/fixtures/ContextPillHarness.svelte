<script lang="ts">
	import ContextPill from '$lib/shell/ContextPill.svelte';

	export let inline = false;
	export let threadId: string | null = 'general';
	export let sessionTitle: string | null = 'Launch notes';
	export let updatedAt: number | null = null;
	export let isReadOnly = false;
	export let canClear = true;
	export let canBuild = true;

	let lastEvent = '';
	let openHistoryCount = 0;
</script>

<ContextPill
	{inline}
	{threadId}
	{sessionTitle}
	{updatedAt}
	{isReadOnly}
	{canClear}
	{canBuild}
	on:open-history={() => {
		openHistoryCount += 1;
		lastEvent = 'open-history';
	}}
	on:new-session={() => (lastEvent = 'new-session')}
	on:build={() => (lastEvent = 'build')}
	on:clear={() => (lastEvent = 'clear')}
	on:archive={() => (lastEvent = 'archive')}
	on:delete={() => (lastEvent = 'delete')}
	on:return-to-active={() => (lastEvent = 'return-to-active')}
/>

<output data-testid="ctx-event">{lastEvent}</output>
<output data-testid="ctx-open-history-count">{openHistoryCount}</output>
