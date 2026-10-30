<script lang="ts">
	import LifecycleRunOptions from '$lib/evals/LifecycleRunOptions.svelte';
	import type { EvalRunOptions } from '$lib/evals/api';
	export let initial: EvalRunOptions = { profiles: [], repeats: 3, partition: 'all' };
	let value = initial;
	let valid = true;
</script>

<LifecycleRunOptions bind:value bind:valid />
<pre data-testid="run-options">{JSON.stringify({ value, valid })}</pre>
