<script lang="ts">
	import { onMount, createEventDispatcher } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { fetchCodingModelRows } from '$lib/llm/codingModelSelection';
	import ModelSelectionControl from '$lib/llm/ModelSelectionControl.svelte';
	import CodingEnginePicker from '$lib/shell/CodingEnginePicker.svelte';
	import { selectionForRoute, routeForOperationSelection, thinkingLabel, type ModelChoice } from '$lib/llm/modelCatalog';
	import { fetchRoutingOverview } from '$lib/stores/modelRoutingStore';
	import {
		isAutoCodingChoiceId,
		type CodingProfile
	} from '$lib/stores/codingProfileStore';
	import {
		fetchAgentDefinitionRecord,
		patchAgentDefinition,
		type AgentDefinitionRecord
	} from '$lib/magician/crew/definitionApi';
	import {
		MODEL_PIN_LANES,
		buildModelPinPatch,
		countModelPins,
		directPinLabel,
		isDirectPin,
		modelPinStateFromDefinition,
		modelPinStatesEqual,
		type ModelPinProfileOption,
		type ModelPinState
	} from './agentModelPins';

	export let agentId: string;

	const dispatch = createEventDispatcher<{ saved: { record: AgentDefinitionRecord } }>();

	let mounted = false;
	let loading = false;
	let saving = false;
	let error: string | null = null;
	let saveError: string | null = null;
	let savedNotice = false;
	let etag = '';
	let loadedFor = '';
	let profiles: ModelPinProfileOption[] = [];
	let codingProfiles: CodingProfile[] = [];
	const globalCoding: CodingProfile = { id: '', label: 'Follow global coding', supports_user_image_inputs: false, is_default: false };
	let models: ModelChoice[] = [];
	let saved: ModelPinState = modelPinStateFromDefinition(null);
	let draft: ModelPinState = modelPinStateFromDefinition(null);

	$: dirty = !modelPinStatesEqual(saved, draft);
	$: pinCount = countModelPins(saved);
	$: if (mounted && agentId.trim() && agentId.trim() !== loadedFor) {
		void load();
	}

	function clone(state: ModelPinState): ModelPinState {
		return {
			lanes: { ...state.lanes },
			coding_profile: state.coding_profile,
			operations: { ...state.operations }
		};
	}

	async function fetchCodingProfiles(): Promise<CodingProfile[]> {
		try {
			const { rows } = await fetchCodingModelRows();
			return rows.filter(
				(profile) => !isAutoCodingChoiceId(profile.id) && profile.selectable !== false
			);
		} catch {
			return [];
		}
	}

	async function load(): Promise<void> {
		const normalized = agentId.trim();
		loadedFor = normalized;
		loading = true;
		error = null;
		saveError = null;
		try {
			const [record, overview, coding] = await Promise.all([
				fetchAgentDefinitionRecord(normalized),
				fetchRoutingOverview(),
				fetchCodingProfiles()
			]);
			if (normalized !== agentId.trim()) return;
			if (!record) throw new Error('Agent definition not found');
			etag = record.etag;
			profiles = (overview.profiles ?? []) as ModelPinProfileOption[];
			codingProfiles = coding;
			models = overview.models ?? [];
			saved = modelPinStateFromDefinition(record.definition);
			draft = clone(saved);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Model pins are unavailable';
		} finally {
			loading = false;
		}
	}

	async function save(): Promise<void> {
		if (!dirty || saving) return;
		saving = true;
		saveError = null;
		savedNotice = false;
		try {
			const record = await patchAgentDefinition(agentId, buildModelPinPatch(saved, draft), etag);
			etag = record.etag;
			saved = modelPinStateFromDefinition(record.definition);
			draft = clone(saved);
			savedNotice = true;
			dispatch('saved', { record });
		} catch (cause) {
			saveError = cause instanceof Error ? cause.message : 'Saving model pins failed';
		} finally {
			saving = false;
		}
	}

	function clearAll(): void {
		draft = {
			lanes: Object.fromEntries(MODEL_PIN_LANES.map(({ lane }) => [lane, ''])) as ModelPinState['lanes'],
			coding_profile: '',
			operations: { ...draft.operations }
		};
	}

	function savedModelLabel(route: string): string {
		if (!route || selectionForRoute(models, profiles, route)) return '';
		const profile = profiles.find(profile => profile.name === route);
		return profile ? `${profile.model} · ${profile.provider}` : isDirectPin(route) ? directPinLabel(route) : 'Unavailable model';
	}

	function isKnownCoding(id: string): boolean {
		return !id || codingProfiles.some((profile) => profile.id === id);
	}

	onMount(() => {
		mounted = true;
		return () => {
			mounted = false;
		};
	});
</script>

<section class="model-pins" aria-label="Model pins">
	<header class="pins-header">
		<div class="pins-heading">
			<div class="pins-icon" aria-hidden="true"><Icon name="sparkle" size={16} /></div>
			<div>
				<h3>Models</h3>
				<p>
					{#if pinCount === 0}
						Follows global routing — no model is pinned for this agent.
					{:else}
						{pinCount} {pinCount === 1 ? 'pin overrides' : 'pins override'} global routing for this agent.
					{/if}
				</p>
			</div>
		</div>
		<div class="pins-controls">
			<button class="ghost-button" type="button" on:click={load} disabled={loading || saving} aria-label="Reload model pins" title="Reload">
				<Icon name="rotate-ccw" size={14} />
			</button>
		</div>
	</header>

	{#if loading && !etag}
		<div class="pins-loading" role="status">Loading models…</div>
	{:else if error}
		<div class="pins-error" role="status">
			<Icon name="alert" size={15} />
			<div><strong>Model pins unavailable</strong><span>{error}</span></div>
		</div>
	{:else}
		<div class="pin-rows">
			{#each MODEL_PIN_LANES as spec (spec.lane)}
				<div class="pin-row">
					<span class="pin-label">
						<strong>{spec.label}</strong>
						<small>{spec.hint}</small>
					</span>
						<div>
							<ModelSelectionControl {models} label={spec.label} savedLabel={savedModelLabel(draft.lanes[spec.lane])} allowUnassigned disabled={saving}
								selection={selectionForRoute(models, profiles, draft.lanes[spec.lane])}
								on:change={(event) => { const route = routeForOperationSelection(models, event.detail); if (route) draft.lanes[spec.lane] = route; }}
								on:clear={() => draft.lanes[spec.lane] = ''} />
							{#if draft.lanes[spec.lane] && !selectionForRoute(models, profiles, draft.lanes[spec.lane])}
								<small>{isDirectPin(draft.lanes[spec.lane]) ? directPinLabel(draft.lanes[spec.lane]) : 'Saved model is unavailable'}</small>
							{/if}
						</div>
				</div>
			{/each}
			<div class="pin-row">
				<span class="pin-label">
					<strong>Coding engine model</strong>
					<small>The model <code>run_coding_task</code> writes code with when the call names none.</small>
				</span>
				{#if codingProfiles.some(profile => profile.model_choice)}
					<CodingEnginePicker profiles={[globalCoding, ...codingProfiles]} selectedProfileId={draft.coding_profile}
						selectedProfile={codingProfiles.find(profile => profile.id === draft.coding_profile) ?? (draft.coding_profile ? { ...globalCoding, id: draft.coding_profile, label: 'Unavailable coding model · saved' } : globalCoding)}
						disabled={saving} ariaLabel="Coding engine model" on:select={event => draft.coding_profile = event.detail.id} />
				{:else}
				<select bind:value={draft.coding_profile} disabled={saving} aria-label="Coding engine model">
					<option value="">Default — global coding profile</option>
					{#if !isKnownCoding(draft.coding_profile)}
						<option value={draft.coding_profile}>{draft.coding_profile} (not in config)</option>
					{/if}
					{#each codingProfiles as profile (profile.id)}
						<option value={profile.id}>{profile.label || profile.id}</option>
					{/each}
				</select>
				{/if}
			</div>
		</div>

		{#if Object.keys(draft.operations).length > 0}
			<div class="operation-pins">
				<small>Per-operation pins (edit in Settings (YAML))</small>
				<ul>
					{#each Object.entries(draft.operations) as [operation, profile] (operation)}
						{@const selection = selectionForRoute(models, profiles, profile)}
						<li><code>{operation}</code> → {selection ? `${models.find(model => model.id === selection.model_id)?.label} · ${thinkingLabel(selection.thinking)}` : profile}</li>
					{/each}
				</ul>
			</div>
		{/if}

		{#if saveError}
			<div class="pins-error" role="alert">
				<Icon name="alert" size={15} />
				<div><strong>Not saved</strong><span>{saveError}</span></div>
			</div>
		{/if}

		<footer class="pins-footer">
			<button class="ghost-button text" type="button" on:click={clearAll} disabled={saving || countModelPins(draft) === Object.keys(draft.operations).length}>
				Clear pins
			</button>
			<div class="footer-actions">
				{#if savedNotice && !dirty}<span class="saved-notice">Saved</span>{/if}
				<button class="ghost-button text" type="button" on:click={() => (draft = clone(saved))} disabled={!dirty || saving}>
					Reset
				</button>
				<button class="primary-button" type="button" on:click={save} disabled={!dirty || saving}>
					{saving ? 'Saving…' : 'Save pins'}
				</button>
			</div>
		</footer>
	{/if}
</section>

<style>
	.model-pins {
		--pins-border: var(--component-card-border, var(--border-soft));
		--pins-surface: var(--bg-elevated, var(--bg-card));
		border: 1px solid var(--pins-border);
		border-radius: var(--radius-md, 16px);
		background: var(--component-card-bg, var(--pins-surface));
		box-shadow: var(--component-card-shadow, var(--shadow-sm));
		padding: 1rem;
		width: 100%;
		max-width: 100%;
		box-sizing: border-box;
		min-width: 0;
		color: var(--text-primary);
		container-type: inline-size;
	}
	.pins-header, .pins-heading, .pins-controls, .pins-footer, .footer-actions { display: flex; align-items: center; }
	.pins-header { justify-content: space-between; gap: 1rem; flex-wrap: wrap; }
	.pins-heading { gap: .7rem; min-width: 0; }
	.pins-heading h3 { margin: 0; font-size: .96rem; letter-spacing: -.01em; }
	.pins-heading p { margin: .18rem 0 0; font-size: .76rem; color: var(--text-secondary); }
	.pins-icon { display: grid; place-items: center; width: 30px; height: 30px; flex: 0 0 auto; border-radius: 9px; color: var(--accent-primary); background: var(--accent-primary-soft); }
	.pin-rows { display: grid; gap: .55rem; margin-top: .85rem; }
	.pin-row { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1.1fr); gap: .75rem; align-items: center; border-top: 1px solid var(--pins-border); padding-top: .55rem; }
	.pin-label { display: grid; gap: .15rem; min-width: 0; }
	.pin-label strong { font-size: .8rem; }
	.pin-label small { font-size: .68rem; color: var(--text-secondary); overflow-wrap: anywhere; }
	.pin-label code, .operation-pins code { font-size: .68rem; }
	select { width: 100%; min-width: 0; min-height: 32px; border: 1px solid var(--input-border, var(--pins-border)); border-radius: 8px; background: var(--input-bg, var(--pins-surface)); color: inherit; padding: 0 1.8rem 0 .6rem; font: 500 .74rem/1 var(--font-primary); text-overflow: ellipsis; }
	select:focus-visible, button:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 2px; }
	.operation-pins { margin-top: .7rem; border-top: 1px solid var(--pins-border); padding-top: .55rem; font-size: .7rem; color: var(--text-secondary); }
	.operation-pins ul { margin: .3rem 0 0; padding-left: 1rem; }
	.pins-footer { justify-content: space-between; gap: .75rem; margin-top: .85rem; border-top: 1px solid var(--pins-border); padding-top: .7rem; }
	.footer-actions { gap: .5rem; }
	.saved-notice { font-size: .7rem; color: var(--color-success, var(--accent-secondary)); }
	.ghost-button { display: grid; place-items: center; min-width: 30px; height: 30px; border: 1px solid var(--button-secondary-border, var(--pins-border)); border-radius: 8px; color: var(--button-secondary-color, var(--text-secondary)); background: var(--button-secondary-bg, var(--pins-surface)); cursor: pointer; }
	.ghost-button.text { padding: 0 .7rem; font: 600 .72rem/1 var(--font-primary); }
	.primary-button { height: 30px; padding: 0 .85rem; border: 0; border-radius: 8px; color: var(--button-primary-color, #fff); background: var(--button-primary-bg, var(--accent-primary)); font: 650 .72rem/1 var(--font-primary); cursor: pointer; }
	button:disabled { opacity: .5; cursor: default; }
	.pins-loading { min-height: 50px; display: flex; align-items: center; font-size: .73rem; color: var(--text-secondary); }
	.pins-error { display: flex; align-items: flex-start; gap: .55rem; margin-top: .8rem; border: 1px solid color-mix(in srgb, var(--color-error) 32%, transparent); border-radius: 10px; padding: .65rem; color: var(--color-error); background: var(--color-error-soft); }
	.pins-error strong, .pins-error span { display: block; }
	.pins-error strong { font-size: .74rem; }
	.pins-error span { margin-top: .12rem; font-size: .68rem; color: var(--text-secondary); overflow-wrap: anywhere; }
	@container (max-width: 520px) {
		.pin-row { grid-template-columns: 1fr; gap: .35rem; }
		.pins-footer { flex-direction: column; align-items: stretch; }
		.footer-actions { justify-content: flex-end; }
	}
	@media (max-width: 640px) {
		.pin-row { grid-template-columns: 1fr; gap: .35rem; }
		.pins-footer { flex-direction: column; align-items: stretch; }
		.footer-actions { justify-content: flex-end; }
	}
</style>
