<script lang="ts">
	import { createEventDispatcher, tick } from 'svelte';
	import ChoiceDropdown from '$lib/shared/components/ChoiceDropdown.svelte';
	import ComposerModelChip from './ComposerModelChip.svelte';
	import ModelSelectionControl from '$lib/llm/ModelSelectionControl.svelte';
	import { thinkingLabel, type ModelSelection } from '$lib/llm/modelCatalog';
	import { codingEngineLabel, codingEngineNote, codingGatewaySummary, type CodingProfile } from '$lib/stores/codingProfileStore';

	export let profiles: CodingProfile[] = [];
	export let blockedProfiles: CodingProfile[] = [];
	export let selectedProfile: CodingProfile | null = null;
	export let disabled = false;
	export let compact = false;
	export let ariaLabel = 'Coding model and engine';
	const dispatch = createEventDispatcher<{ select: { id: string } }>();
	let open = false;
	let root: HTMLDivElement;
	let trigger: HTMLButtonElement;
	let panel: HTMLDivElement;
	const engineKey = (profile: CodingProfile): string => profile.engine ?? profile.id;
	$: current = selectedProfile ?? profiles.find(profile => profile.is_default) ?? profiles[0];
	$: selectedEngine = current ? engineKey(current) : 'pi';
	$: models = profiles.filter(profile => engineKey(profile) === selectedEngine).flatMap(profile => profile.model_choice ? [profile.model_choice] : []);
	$: selection = current?.model_selection ?? null;
	$: model = models.find(model => model.id === selection?.model_id);
	$: allProfiles = [...profiles, ...blockedProfiles];
	$: engineKeys = [...new Set(allProfiles.map(engineKey))];
	$: engineOptions = engineKeys.map(id => {
		const rows = allProfiles.filter(profile => engineKey(profile) === id);
		const available = rows.find(profile => profile.selectable !== false);
		return { id, label: rows[0].engine ? codingEngineLabel(id) : rows[0].label,
			disabled: !available, note: !available ? rows[0].reason ?? 'Unavailable' : undefined,
			title: codingEngineNote(rows[0].engine, rows[0].credentials) ?? rows[0].description ?? undefined };
	});
	$: engineModels = allProfiles.filter(profile => engineKey(profile) === selectedEngine);
	$: modelOptions = engineModels.map(profile => ({ id: profile.id,
		label: profile.model && !['default', 'codex', 'claude', 'agy'].includes(profile.model) ? profile.model : engineModels.length === 1 ? 'Default' : profile.label,
		disabled: profile.selectable === false, note: profile.selectable === false ? profile.reason ?? 'Unavailable' : undefined }));
	$: label = model ? `${codingEngineLabel(current?.engine)} · ${model.label} · ${thinkingLabel(selection?.thinking ?? 'auto')}` : current?.label ?? 'Choose engine';
	$: triggerEngine = current?.engine ? codingEngineLabel(current.engine) : current?.label ?? 'Choose engine';
	$: triggerModel = model?.label ?? current?.model ?? (current?.label !== triggerEngine ? current?.label : null);
	function selectModel(value: ModelSelection): void {
		const id = models.find(model => model.id === value.model_id)?.choices[value.thinking];
		if (id) dispatch('select', { id });
	}
	function selectEngine(engine: string): void {
		const rows = profiles.filter(profile => engineKey(profile) === engine && profile.selectable !== false);
		const next = rows.find(profile => profile.is_default) ?? rows.find(profile => profile.model_selection?.thinking === 'auto') ?? rows[0];
		if (next) dispatch('select', { id: next.id });
	}
	async function toggle(): Promise<void> {
		open = !open;
		if (open) { await tick(); panel?.querySelector<HTMLButtonElement>('button')?.focus(); }
	}
	function outside(event: PointerEvent): void {
		if (open && !root?.contains(event.target as Node)) open = false;
	}
	function onKeydown(event: KeyboardEvent): void {
		if (open && event.key === 'Escape') { event.preventDefault(); open = false; trigger?.focus(); }
	}
</script>

<svelte:window on:pointerdown={outside} on:keydown={onKeydown} />
<div class="coding-model-picker" class:compact bind:this={root}>
	<ComposerModelChip engine={triggerEngine} model={triggerModel} modelIdentifier={model?.model ?? current?.model ?? null} feedback={model?.feedback ?? null} thinking={model && model.thinking.mode !== 'none' ? thinkingLabel(selection?.thinking ?? 'auto') : null}
		expanded={open} {disabled} {ariaLabel} title={label}
		bind:triggerElement={trigger} on:click={toggle} />
	{#if open}
		<div class="panel" role="dialog" aria-label="Coding model and engine" bind:this={panel}>
			<ChoiceDropdown label="Engine" value={selectedEngine} options={engineOptions} {disabled} on:change={event => selectEngine(event.detail)} />
			{#if models.length > 0}
				<ModelSelectionControl {models} {selection} {disabled} label="Model" on:change={event => selectModel(event.detail)} />
			{:else if current?.engine}
				<ChoiceDropdown label="Model" value={current.id} options={modelOptions} {disabled} on:change={event => dispatch('select', { id: event.detail })} />
			{:else if current?.description}
				<p>{current.description}</p>
			{/if}
			{#if current?.gateway}<p>{codingGatewaySummary(current.gateway)}</p>{/if}
		</div>
	{/if}
</div>

<style>
	.coding-model-picker { position: relative; min-width: 0; max-width: min(24rem, 60vw); }
	.panel { font-family: var(--font-primary); font-size: .76rem; display: grid; gap: .6rem; position: absolute; bottom: calc(100% + .5rem); right: 0; box-sizing: border-box; width: min(352px, calc(100vw - 2rem)); max-height: min(34rem, 75vh); overflow: auto; padding: .7rem; border: 1px solid var(--border-soft, #8884); border-radius: .65rem; background: var(--bg-elevated, var(--vibe-surface, #fff)); color: var(--text-primary, #222); box-shadow: 0 12px 40px #0002; z-index: 100; }
	p { margin: 0; font-size: .68rem; line-height: 1.4; color: var(--text-secondary, #666); }
</style>
