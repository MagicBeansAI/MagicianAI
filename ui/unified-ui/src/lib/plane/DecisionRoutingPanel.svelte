<script lang="ts">
 import { onMount } from 'svelte';
 import { hasHydratedScopeBearer } from '$lib/stores/scopeIdentityStore';
 import { decisionRouting, type RoutingSettings, type OperationRouting, type LocalityRoutes } from './decisionRouting';
 const modes = ['local', 'cloud'] as const;
 let settings: RoutingSettings | null = null;
 let selected = '';
 let draft: OperationRouting | null = null;
 let error = '';
 let saving = false;
 let loading = true;
 let saved = false;
 let alive = true;
 let timer: ReturnType<typeof setTimeout> | undefined;
 let pollCount = 0;
 $: selectedModels = draft?.routing ? [...new Set([draft.routing.local.primary, draft.routing.local.backup, draft.routing.cloud.primary, draft.routing.cloud.backup].filter((name): name is string => !!name))] : [];
 $: invalid = !draft?.routing || selectedModels.some(name => draft!.threshold_names.some(head => {
  const value = draft!.thresholds_by_model[name]?.[head];
  return typeof value !== 'number' || !Number.isFinite(value) || value < 0 || value > 1;
 })) || (['local', 'cloud'] as const).some(mode => draft?.routing?.[mode].primary === draft?.routing?.[mode].backup)
 || (!draft?.allow_remote_when_local && [draft?.routing?.local.primary, draft?.routing?.local.backup].some(name => settings?.models.find(m => m.name === name)?.remote));
 function selectOperation() {
  const operation = settings?.operations.find(o => o.name === selected);
  draft = operation ? JSON.parse(JSON.stringify(operation)) : null;
  saved = false;
 }
 function schedulePoll() {
  clearTimeout(timer);
  if (!settings?.pending || pollCount >= 60) return;
  timer = setTimeout(async () => {
   try {
    const next = await decisionRouting();
    if (!alive) return;
    if (next.revision !== settings?.revision) {
     error = 'Settings changed elsewhere. Reload before saving.';
     return;
    }
    settings = next;
    pollCount++;
    schedulePoll();
   } catch (e) { if (alive) error = String(e instanceof Error ? e.message : e); }
  }, 2000);
 }
 async function load() {
  loading = true; error = ''; clearTimeout(timer);
  try {
   if (!(await hasHydratedScopeBearer())) throw new Error('Sign in as the owner to manage decision routing.');
   const next = await decisionRouting();
   if (!alive) return;
   settings = next;
   if (!next.operations.some(o => o.name === selected)) selected = next.operations[0]?.name ?? '';
   selectOperation(); pollCount = 0; schedulePoll();
  } catch (e) { if (alive) error = e instanceof Error ? e.message : String(e); }
  finally { if (alive) loading = false; }
 }
 function setRoute(mode: 'local' | 'cloud', field: 'primary' | 'backup', event: Event) {
  if (!draft?.routing) return;
  const value = (event.target as HTMLSelectElement).value;
  const route = {...draft.routing[mode], [field]: field === 'backup' && !value ? null : value};
  if (field === 'primary' && route.backup === value) route.backup = null;
  draft = {...draft, routing: {...draft.routing, [mode]: route}};
  saved = false;
 }
 function setRemote(event: Event) {
  if (draft) draft = {...draft, allow_remote_when_local: (event.target as HTMLInputElement).checked};
  saved = false;
 }
 function setThreshold(model: string, head: string, event: Event) {
  if (!draft) return;
  const value = (event.target as HTMLInputElement).valueAsNumber;
  draft.thresholds_by_model[model] = {...draft.thresholds_by_model[model]};
  if (Number.isFinite(value)) draft.thresholds_by_model[model][head] = value;
  else delete draft.thresholds_by_model[model][head];
  draft = {...draft}; saved = false;
 }
 function startExplicitRouting() {
  if (!draft || !settings?.models.length) return;
  const local = settings.models.find(m => !m.remote)?.name ?? settings.models[0].name;
  draft.routing = { local: {primary: local, backup: null}, cloud: {primary: settings.models[0].name, backup: null} };
  draft = {...draft};
 }
 async function save() {
  if (!settings || !draft?.routing || invalid) return;
  saving = true; error = ''; saved = false; clearTimeout(timer);
  try {
   const next = await decisionRouting({revision: settings.revision, operation: draft.name,
    routing: draft.routing as LocalityRoutes, allow_remote_when_local: draft.allow_remote_when_local,
    thresholds_by_model: draft.thresholds_by_model});
   if (!alive) return;
   settings = next; selectOperation(); saved = true; pollCount = 0; schedulePoll();
  } catch (e) { if (alive) error = e instanceof Error ? e.message : String(e); }
  finally { if (alive) saving = false; }
 }
 onMount(() => { load(); return () => { alive = false; clearTimeout(timer); }; });
</script>

<section class="routing" aria-label="Decision model routing">
 <h3>Decision models and operation mappings</h3>
 <p>Choose a primary model and optional backup for each operation. Local and cloud mappings follow the request’s processing mode. These settings apply to all clients.</p>
 {#if error}<p role="alert">{error}</p>{/if}
 {#if loading}<p>Loading decision models…</p>{/if}
 <button type="button" onclick={load} disabled={saving || loading}>Reload decision routing</button>
 {#if settings}
  {#if !settings.enabled}<p>The Decision Engine is disabled. Saved mappings will apply when it is enabled.</p>{/if}
  <p role="status">{settings.pending ? 'Saved configuration is waiting to become active. Local model loading may take time.' : saved ? 'Decision routing configuration applied.' : 'Showing saved decision routing.'}</p>
  <label>Decision operation
   <select bind:value={selected} onchange={selectOperation} disabled={saving || loading}>
    {#each settings.operations as op}<option value={op.name}>{op.name.replaceAll('_', ' ')}</option>{/each}
   </select>
  </label>
  {#if draft}
   <p class="active">Active local route: {settings.operations.find(o => o.name === selected)?.active_local.join(' → ') || 'Unavailable'}<br />Active cloud route: {settings.operations.find(o => o.name === selected)?.active_cloud.join(' → ') || 'Unavailable'}</p>
   {#if !settings.pending && settings.enabled && settings.operations.some(o => o.name === selected && (o.active_local.length < o.configured_local.length || o.active_cloud.length < o.configured_cloud.length))}
    <p role="status">Some selected models are unavailable or excluded by locality policy. Check the active routes above, model installation, and provider credentials.</p>
   {/if}
   {#if !draft.routing}
    <p>This operation uses a legacy tier: {draft.configured_cloud.join(' → ')}. Its full route is preserved until you replace it.</p>
    <button type="button" onclick={startExplicitRouting}>Set primary and backup</button>
   {:else}
    <form onsubmit={(e) => { e.preventDefault(); save(); }}>
     <fieldset disabled={saving || loading}>
      <div class="routes">
       {#each modes as mode}
        <div class="route">
         <h4>{mode === 'local' ? 'Local processing' : 'Cloud processing'}</h4>
         <label>{mode === 'local' ? 'Local' : 'Cloud'} primary
          <select value={draft.routing[mode].primary} onchange={(e) => setRoute(mode, 'primary', e)}>
           {#each settings.models as model}<option value={model.name}>{model.name} ({model.remote ? 'remote' : 'local'})</option>{/each}
          </select>
         </label>
         <label>{mode === 'local' ? 'Local' : 'Cloud'} backup
          <select value={draft.routing[mode].backup ?? ''} onchange={(e) => setRoute(mode, 'backup', e)}>
           <option value="">None</option>
           {#each settings.models as model}<option value={model.name} disabled={model.name === draft.routing[mode].primary}>{model.name} ({model.remote ? 'remote' : 'local'})</option>{/each}
          </select>
         </label>
        </div>
       {/each}
      </div>
      <label class="opt-in"><input type="checkbox" checked={draft.allow_remote_when_local} onchange={setRemote} /> Allow this operation to send content to remote models in local mode</label>
      <details>
       <summary>Model confidence thresholds{invalid ? ' — review required' : ''}</summary>
       <p>Each model needs its own thresholds. A backup runs when the primary fails or is uncertain; an uncertain final answer is deferred or returned to the operation’s existing fallback.</p>
       {#each selectedModels as model}
        <h4>{model} ({settings.models.find(m => m.name === model)?.model})</h4>
        {#each draft.threshold_names as head}
         <label>{model}: {head}
          <input type="number" min="0" max="1" step="0.01" required value={draft.thresholds_by_model[model]?.[head] ?? ''} oninput={(e) => setThreshold(model, head, e)} />
         </label>
        {/each}
       {/each}
      </details>
      {#if invalid}<p>Choose different primary and backup models, provide each model’s thresholds, and explicitly allow any remote model in the local mapping.</p>{/if}
      <button type="submit" disabled={invalid || saving || settings.pending}>{saving ? 'Saving…' : 'Save operation mapping'}</button>
     </fieldset>
    </form>
   {/if}
  {/if}
 {/if}
</section>
<style>
 .routing { margin-block: 1.25rem; border-top: 1px solid var(--border-color, #8884); padding-top: 1rem; }
 .routes { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 220px), 1fr)); gap: 1rem; }
 label { display: flex; flex-direction: column; gap: .35rem; margin-block: .6rem; }
 select, input[type=number] { width: 100%; max-width: 100%; min-width: 0; padding: .5rem; border: 1px solid #8886; border-radius: .4rem; background: var(--bg-secondary, transparent); }
 .opt-in { flex-direction: row; align-items: flex-start; }
 fieldset { padding: 0; border: 0; min-width: 0; }
 p { font-size: .9rem; margin-block: .6rem; overflow-wrap: anywhere; }
 button { margin-block: .5rem; padding: .5rem .8rem; border: 1px solid #8886; border-radius: .4rem; }
 button:disabled { opacity: .5; }
 details { margin-block: 1rem; }
</style>
