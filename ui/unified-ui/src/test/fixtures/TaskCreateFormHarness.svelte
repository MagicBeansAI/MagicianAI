<script lang="ts">
	import TaskCreateForm from '$lib/magician/tasks/TaskCreateForm.svelte';
	import type { TaskCreateSubmitValues } from '$lib/magician/tasks/types';

	export let disabled = false;

	let submitted: TaskCreateSubmitValues | null = null;
	let clearCount = 0;
</script>

<TaskCreateForm
	{disabled}
	agentOptions={[
		{ agent_id: 'agent-1', name: 'Ada' },
		{ agent_id: 'agent-2', name: 'Grace' }
	]}
	threadOptions={[
		{ id: 'general', name: 'general' },
		{ id: 'product', name: 'product' }
	]}
	scheduleTimezoneOptions={[
		{ value: 'UTC', label: 'UTC' },
		{ value: 'Asia/Kolkata', label: 'Asia/Kolkata' }
	]}
	defaultTimezone="UTC"
	on:submit={(event) => (submitted = event.detail.values)}
	on:clear={() => (clearCount += 1)}
/>

<output data-testid="task-create-submit">{submitted ? JSON.stringify(submitted) : ''}</output>
<output data-testid="task-create-clears">{clearCount}</output>
