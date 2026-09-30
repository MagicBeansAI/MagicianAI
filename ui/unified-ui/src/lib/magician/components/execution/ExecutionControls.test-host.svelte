<script lang="ts">
	import ExecutionControls from './ExecutionControls.svelte';

	export let executionId: string;

	let lastAction = '';
</script>

<ExecutionControls
	{executionId}
	on:changed={(event) => {
		lastAction = event.detail.action;
	}}
/>

<output data-testid="changed-action">{lastAction}</output>
