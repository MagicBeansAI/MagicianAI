<script lang="ts">
	import { onMount } from 'svelte';
	import { fetchRoutingOverview, type RoutingProfile } from '$lib/stores/modelRoutingStore';
	import ModelSelectionControl from '$lib/llm/ModelSelectionControl.svelte';
	import { routeForSelection, selectionForRoute, type ModelChoice, type ModelSelection } from '$lib/llm/modelCatalog';
	import type { EvalRunOptions } from './api';

	export let value: EvalRunOptions = { profiles: [], repeats: 3, partition: 'all' };
	export let valid = true;
	let profiles: RoutingProfile[] = [];
	let models: ModelChoice[] = [];
	let loading = true;
	let error = '';
	$: comparisons = value.profiles ?? [];
	$: valid = comparisons.length <= 3 && new Set(comparisons).size === comparisons.length
		&& comparisons.every(route => {
			const selection = selectionForRoute(models, profiles, route);
			return selection && Boolean(routeForSelection(models, selection))
				&& models.find(model => model.id === selection.model_id)?.installed !== false;
		});
	function choose(index: number, selection: ModelSelection) {
		const route = routeForSelection(models, selection);
		if (!route) return;
		value = { ...value, profiles: comparisons.map((current, position) => position === index ? route : current) };
	}
	onMount(() => {
		let active = true;
		fetchRoutingOverview().then((overview) => {
			if (!active) return;
			profiles = overview.profiles.filter(p => p.selectable !== false && p.class !== 'harness');
			const routes = new Set(profiles.map(profile => profile.name));
			models = (overview.models ?? []).filter(model => !model.provider.startsWith('harness-'))
				.map(model => ({ ...model, operation_choices: undefined,
					choices: Object.fromEntries(Object.entries(model.operation_choices ?? model.choices).filter(([, route]) => routes.has(route))) }))
				.filter(model => Boolean(model.choices.auto));
			if (!models.length) error = 'Model choices unavailable. You can still run the configured routes.';
		}).catch(() => {
			if (active) error = 'Model choices unavailable. You can still run the configured routes.';
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
	<p class="comparison-label">Models to compare (optional, up to 3)</p>
	{#each comparisons as route, index (index)}
		<div class="comparison" role="group" aria-label={`Model comparison ${index + 1}`}>
			<ModelSelectionControl {models} label={`Comparison ${index + 1}`}
				selection={selectionForRoute(models, profiles, route)} disabled={loading || !models.length}
				savedLabel={route ? 'Unavailable model' : ''} on:change={event => choose(index, event.detail)} />
			<button type="button" aria-label={`Remove comparison ${index + 1}`}
				on:click={() => value = { ...value, profiles: comparisons.filter((_, position) => position !== index) }}>Remove</button>
		</div>
	{/each}
	<button type="button" disabled={loading || !models.length || comparisons.length >= 3 || comparisons.includes('')}
		on:click={() => value = { ...value, profiles: [...comparisons, ''] }}>Add model</button>
	<button type="button" on:click={() => value = { ...value, profiles: [] }}>Use configured routes</button>
	<p>With no selection, use the saved operation routes. Choose a model and thinking level for each comparison. Each runs the same fixtures in isolation without changing live model settings.</p>
	<p>Model call estimates are in each report. The Evals ledger total is unavailable for this separate evaluator process.</p>
	{#if error}<p role="status">{error}</p>{/if}
	{#if !valid}<p role="alert">Choose up to three available model and thinking combinations. Each comparison must be different.</p>{/if}
</fieldset>

<style>
	.lifecycle-options { display: grid; gap: .75rem; margin: 1rem 0; padding: 1rem; border: 1px solid var(--border-color, #ccd3dc); border-radius: .5rem; }
	label { display: grid; gap: .3rem; }
	select, button { font: inherit; color: inherit; background: var(--bg-secondary, transparent); padding: .4rem; border: 1px solid var(--border-color, #ccd3dc); border-radius: .3rem; }
	button { justify-self: start; }
	.comparison { display: grid; gap: .6rem; padding: .75rem; border: 1px solid var(--border-color); border-radius: .5rem; }
	.comparison-label { font-weight: 600; }
	p { margin: 0; font-size: .875rem; }
</style>
