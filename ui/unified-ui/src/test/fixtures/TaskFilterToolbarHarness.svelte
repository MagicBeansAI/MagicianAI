<script lang="ts">
	import TaskFilterToolbar, {
		type TaskStatusChip
	} from '$lib/magician/components/TaskFilterToolbar.svelte';

	export let tags: string[] = [];
	export let counts: Record<string, number | null | undefined> = {};
	export let statusChips: TaskStatusChip[] = [];
	export let totalCount: number | null = null;

	let activeFilter = 'all';
	let activeTag: string | null = null;
</script>

<TaskFilterToolbar
	{activeFilter}
	{activeTag}
	{tags}
	{counts}
	{statusChips}
	{totalCount}
	on:filter={(event) => {
		activeFilter = event.detail.value;
		activeTag = null;
	}}
	on:tag={(event) => {
		activeTag = event.detail.value;
	}}
/>

<output data-testid="task-filter-state">{activeFilter}:{activeTag ?? ''}</output>
