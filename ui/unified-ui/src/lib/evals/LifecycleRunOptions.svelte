<script lang="ts">
	import { onMount } from 'svelte';
	import { fetchRoutingOverview, type RoutingProfile } from '$lib/stores/modelRoutingStore';
	import type { EvalRunOptions } from './api';

	export let value: EvalRunOptions = { profiles: [], repeats: 3, partition: 'all' };
	export let valid = true;
	let profiles: RoutingProfile[] = [];
	let loading = true;
	let error = '';
	$: valid = (value.profiles?.length ?? 0) <= 3;
	onMount(() => {
		let active = true;
		fetchRoutingOverview().then((overview) => {
			if (active) profiles = overview.profiles.filter(p => p.selectable !== false && p.class !== 'harness');
		}).catch(() => {
			if (active) error = 'Profile list unavailable. You can still run the configured routes.';
		}).finally(() => { if (active) loading = false; });
		return () => { active = false; };
	});
</script>

<fieldset class="lifecycle-options">
	<legend>Lifecycle run</legend>
	<label>Fixture partition
		<select bind:value={value.partition}>
			<option value="all">All scenarios</option>
			<option value="development">Development</option>
			<option value="validation">Validation</option>
		</select>
	</label>
	<label>Repeats
		<select bind:value={value.repeats}>
			<option value={1}>1</option><option value={2}>2</option><option value={3}>3</option>
		</select>
	</label>
	<label>Profiles to compare (optional, up to 3)
		<select multiple size="5" bind:value={value.profiles} disabled={loading}>
			{#each profiles as profile}
				<option value={profile.name}>{profile.name} — {profile.provider} / {profile.model}</option>
			{/each}
		</select>
	</label>
	<button type="button" on:click={() => value = { ...value, profiles: [] }}>Use configured routes</button>
	<p>With no selection, use the saved operation routes. Selected profiles each run the same fixtures in isolation; live model settings stay unchanged. Comparing profiles can also change their provider and generation settings.</p>
	<p>Model call estimates are in each report. The Evals ledger total is unavailable for this separate evaluator process.</p>
	{#if error}<p role="status">{error}</p>{/if}
	{#if !valid}<p role="alert">Select at most three profiles.</p>{/if}
</fieldset>

<style>
	.lifecycle-options { display: grid; gap: .75rem; margin: 1rem 0; padding: 1rem; border: 1px solid var(--border-color, #ccd3dc); border-radius: .5rem; }
	label { display: grid; gap: .3rem; }
	select, button { font: inherit; color: inherit; background: var(--bg-secondary, transparent); padding: .4rem; border: 1px solid var(--border-color, #ccd3dc); border-radius: .3rem; }
	button { justify-self: start; }
	p { margin: 0; font-size: .875rem; }
</style>
