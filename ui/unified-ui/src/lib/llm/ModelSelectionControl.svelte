<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import ChoiceDropdown from '$lib/shared/components/ChoiceDropdown.svelte';
	import ThinkingControl from './ThinkingControl.svelte';
	import { contextLabel, modelProviderLabel, offeredModels, selectModel, type ModelChoice, type ModelSelection } from './modelCatalog';

	export let models: ModelChoice[] = [];
	export let selection: ModelSelection | null = null;
	export let disabled = false;
	export let allowUnassigned = false;
	export let label = 'Model';
	export let savedLabel = '';
	export let inline = false;
	const dispatch = createEventDispatcher<{ change: ModelSelection; clear: void }>();
	$: selected = models.find(model => model.id === selection?.model_id);
	$: options = [
		...(allowUnassigned ? [{ id: '', label: 'Follow global routing' }] : []),
		...offeredModels(models, selection?.model_id).map(model => ({ id: model.id, label: model.label, group: modelProviderLabel(model.provider),
			disabled: model.installed === false || model.availability?.state === 'unavailable',
			note: model.installed === false ? 'not installed' : model.availability?.state === 'unavailable' ? 'no longer offered' : undefined,
			title: model.model, search: `${model.model} ${model.provider}` }))
	];
</script>

<div class="model-selection">
	<ChoiceDropdown {label} {options} {disabled} {inline} value={selection?.model_id ?? null}
		placeholder={savedLabel ? `${savedLabel} · saved` : allowUnassigned ? 'Follow global routing' : 'Choose a model'}
		on:change={event => event.detail ? dispatch('change', selectModel(models, event.detail, selection)) : dispatch('clear')} />
	{#if selected}
		<div class="model-facts">
			<span>{modelProviderLabel(selected.provider)}</span>{#if selected.limits}<span>{contextLabel(selected.limits.context_window_tokens)} context</span>{/if}
			{#if selected.capabilities.vision}<span>Images</span>{/if}
			{#if selected.provider === 'ollama'}<span>Local</span>{/if}
		</div>
		<ThinkingControl control={selected.thinking} value={selection?.thinking ?? 'auto'} {disabled}
			on:change={event => dispatch('change', { model_id: selected!.id, thinking: event.detail })} />
	{/if}
</div>

<style>
	.model-selection { display: grid; gap: .55rem; min-width: 0; width: 100%; }
	.model-facts { display: flex; flex-wrap: wrap; gap: .35rem; font-size: .65rem; color: var(--text-muted, #8f9799); }
	.model-facts span + span::before { content: '·'; margin-right: .35rem; }
</style>
