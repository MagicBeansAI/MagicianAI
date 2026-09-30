<script lang="ts">
  import { onDestroy, tick } from 'svelte';
  import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
  import { cleanupCutoff, createAppCleanupClient, type AppCleanupClient, type AppCleanupJob, type AppCleanupOptions } from './appDataCleanup';

  export let installationId: string;
  export let appName: string;
  export let initialEntity = '';
  export let createClient: (installationId: string) => AppCleanupClient = createAppCleanupClient;
  export let onChanged: () => void = () => {};

  let dialog: HTMLDialogElement;
  let client: AppCleanupClient | null = null;
  let request: AbortController | null = null;
  let openedScope = '';
  let openedInstallation = '';
  let options: AppCleanupOptions | null = null;
  let job: AppCleanupJob | null = null;
  let entity = initialEntity;
  let field = '';
  let cutoff = '';
  let busy = false;
  let driving = false;
  let pauseRequested = false;
  let confirmed = false;
  let error = '';
  let open = false;
  $: scopeKey = JSON.stringify([$scopeIdentityStore.principal,$scopeIdentityStore.workspace]);
  $: fields = options?.entities.find(entry => entry.entity === entity)?.timestamp_fields ?? [];
  $: if (fields.length && !fields.includes(field)) field = fields.includes('created_at') ? 'created_at' : fields[0];
  $: activeJob = job && ['running','paused','checkpointing'].includes(job.status);
  $: if (open && (scopeKey !== openedScope || installationId !== openedInstallation)) close();

  const label = (name: string) => name.replaceAll('_',' ').replace(/^./,letter => letter.toUpperCase());
  const bytes = (value: number) => value >= 1024 * 1024 ? `${(value / (1024 * 1024)).toFixed(1)} MB` : `${(value / 1024).toFixed(1)} KB`;
  function defaultDate() {
    const date = new Date(); date.setDate(date.getDate() - 90);
    return `${date.getFullYear()}-${String(date.getMonth()+1).padStart(2,'0')}-${String(date.getDate()).padStart(2,'0')}`;
  }

  function invalidatePreview() {
    if (job?.status === 'preview') job = null;
    confirmed = false;
  }

  async function show() {
    request?.abort(); request = new AbortController();
    const controller = request;
    openedScope = scopeKey; openedInstallation = installationId;
    client = createClient(installationId); options = null; job = null;
    error = ''; confirmed = false; busy = true; open = true;
    cutoff = defaultDate();
    await tick(); dialog.showModal();
    try {
      const loaded = await client.options(controller.signal);
      if (request !== controller || controller.signal.aborted) return;
      options = loaded; job = loaded.latest_job;
      entity = loaded.entities.some(entry => entry.entity === initialEntity) ? initialEntity : loaded.entities[0]?.entity ?? '';
      if (job && ['preview','running','paused','checkpointing'].includes(job.status)) {
        entity = job.selection.entity; field = job.selection.timestamp_field;
        const date = new Date(job.selection.before);
        cutoff = `${date.getFullYear()}-${String(date.getMonth()+1).padStart(2,'0')}-${String(date.getDate()).padStart(2,'0')}`;
      }
    } catch (cause) { if (!controller.signal.aborted) error = cause instanceof Error ? cause.message : 'Cleanup could not be opened.'; }
    finally { if (request === controller) busy = false; }
  }

  function close() {
    open = false; pauseRequested = true; driving = false; request?.abort(); request = null;
    busy = false; client = null; dialog?.close();
  }
  onDestroy(close);

  async function preview() {
    if (!client || !request || busy || driving) return;
    const controller = request; const owner = client;
    busy = true; error = ''; confirmed = false;
    try {
      const result = await owner.preview({entity,timestamp_field:field,before:cleanupCutoff(cutoff)},controller.signal);
      if (request === controller) job = result;
    } catch (cause) { if (!controller.signal.aborted) error = cause instanceof Error ? cause.message : 'Preview failed.'; }
    finally { if (request === controller) busy = false; }
  }

  async function run(operation: 'confirm' | 'resume') {
    if (!client || !request || !job || busy || driving || (operation === 'confirm' && !confirmed)) return;
    const controller = request; const owner = client;
    driving = true; pauseRequested = false; error = '';
    try {
      if (operation === 'confirm' || job.status === 'paused') {
        const result = await owner.control(job,operation,controller.signal);
        if (request !== controller) return;
        job = result;
      }
      while (request === controller && !controller.signal.aborted && job && ['running','checkpointing'].includes(job.status)) {
        if (pauseRequested) {
          if (job.status === 'running') job = await owner.control(job,'pause',controller.signal);
          break;
        }
        const result = await owner.advance(job,controller.signal);
        if (request !== controller) return;
        job = result;
        if (job.status === 'completed' || (job.status === 'cancelled' && job.deleted_records > 0)) onChanged();
        await tick();
      }
    } catch (cause) { if (!controller.signal.aborted) error = cause instanceof Error ? cause.message : 'Cleanup stopped. You can retry the saved progress.'; }
    finally { if (request === controller) driving = false; }
  }

  async function cancel() {
    if (!client || !request || !job || busy || driving) return;
    const controller = request; busy = true; error = '';
    try { const result = await client.control(job,'cancel',controller.signal); if (request === controller) job = result; }
    catch (cause) { if (!controller.signal.aborted) error = cause instanceof Error ? cause.message : 'Cleanup could not be cancelled.'; }
    finally { if (request === controller) busy = false; }
    if (request === controller && job?.status === 'checkpointing') await run('resume');
  }
</script>

<button type="button" class="cleanup-launch" on:click={() => void show()}>Delete older data…</button>
<dialog bind:this={dialog} on:close={close} on:cancel={close} aria-label={`Delete older data from ${appName}`}>
  {#if open}
    <header><div><p class="eyebrow">App data</p><h2>Delete older data</h2><p>{appName} · {$scopeIdentityStore.workspace}</p></div><button type="button" aria-label="Close data cleanup" on:click={close}>×</button></header>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if !options}<p role="status">{busy ? 'Loading cleanup options…' : 'Close and reopen to try again.'}</p>
    {:else if options.entities.length === 0}<p>This app has no dated record types available for cleanup.</p>
    {:else}
      {#if !activeJob}
        <form on:submit|preventDefault={() => void preview()}>
          <div class="fields">
            <label>Record type<select bind:value={entity} on:change={invalidatePreview} disabled={busy || driving}>{#each options.entities as entry}<option value={entry.entity}>{label(entry.entity)}</option>{/each}</select></label>
            <label>Date field<select bind:value={field} on:change={invalidatePreview} disabled={busy || driving}>{#each fields as name}<option value={name}>{label(name)}</option>{/each}</select></label>
            <label>Before date<input type="date" bind:value={cutoff} on:input={invalidatePreview} required disabled={busy || driving} /></label>
          </div>
          <p class="hint">Before midnight on this date, in your timezone. Records on or after the cutoff are kept.</p>
          <button type="submit" disabled={busy || driving || !entity || !field || !cutoff}>{busy ? 'Preparing preview…' : 'Preview cleanup'}</button>
        </form>
      {/if}
      {#if job && job.status !== 'cancelled'}
        <section class="preview" aria-label="Cleanup preview and progress">
          <p><strong>{job.matching_records.toLocaleString()} records</strong> matched {label(job.selection.timestamp_field).toLowerCase()} before <strong>{new Date(job.selection.before).toLocaleString()}</strong>.</p>
          <p>{bytes(job.matching_payload_bytes)} of current record data. Stored history and indexes for deleted records are removed too.</p>
          <p>New records and records changed since this preview are kept. Records needed by other data are kept and reported.</p>
          <p class="hint">This removes this app’s stored data. Data imported from another source may return when the app syncs again.</p>
          {#if job.status === 'preview'}
            <p class="hint">Preview valid until {new Date(job.expires_at).toLocaleTimeString()}.</p>
            <label class="confirm"><input type="checkbox" bind:checked={confirmed} disabled={busy || driving} />I understand this permanently deletes the selected older app data.</label>
            <div class="actions"><button type="button" class="danger" disabled={busy || driving || !confirmed || job.matching_records === 0 || Date.now() >= Date.parse(job.expires_at)} on:click={() => void run('confirm')}>Delete selected older data</button><button type="button" disabled={busy || driving} on:click={() => void cancel()}>Discard preview</button></div>
          {:else}
            <div role="status" aria-live="polite">
              <p><strong>{job.deleted_records.toLocaleString()} deleted</strong> · {job.remaining_records.toLocaleString()} remaining</p>
              {#if job.kept_changed_records}<p>{job.kept_changed_records.toLocaleString()} kept because they changed or were already removed.</p>{/if}
              {#if job.kept_referenced_records}<p>{job.kept_referenced_records.toLocaleString()} kept because of references. You can review linked data and run a fresh preview later.</p>{/if}
              {#if job.status === 'completed'}<p>Cleanup complete. Freed database pages can be reused. <a href="/storage">Open Storage to reclaim file space</a>.</p>
              {:else if job.status === 'checkpointing'}<p>Finishing database cleanup…</p>
              {:else}<p>{driving ? 'Deleting in batches…' : 'Progress saved. Continue when ready.'}</p>{/if}
            </div>
            {#if activeJob}<div class="actions">
              {#if driving}<button type="button" disabled={pauseRequested} on:click={() => pauseRequested = true}>{pauseRequested ? 'Pausing…' : 'Pause cleanup'}</button>
              {:else}<button type="button" on:click={() => void run('resume')}>Continue cleanup</button>{#if job.status !== 'checkpointing'}<button type="button" on:click={() => void cancel()}>Stop remaining cleanup</button>{/if}{/if}
            </div><p class="hint">Closing this dialog stops after the current batch. Reopen it to continue from saved progress.</p>{/if}
          {/if}
        </section>
      {:else if job?.status === 'cancelled'}<p role="status">Cleanup stopped. {job.deleted_records.toLocaleString()} records were already deleted; remaining records were kept.</p>{/if}
    {/if}
  {/if}
</dialog>

<style>
  .cleanup-launch { font-size: .8rem; }
  dialog { width: min(640px, calc(100vw - 32px)); max-height: calc(100vh - 40px); overflow: auto; color: var(--text-primary); background: var(--bg-elevated, var(--bg-canvas, #fff)); border: 1px solid var(--border-soft); border-radius: 16px; padding: 24px; box-shadow: 0 24px 80px #0004; }
  dialog::backdrop { background: #1118; }
  header { display: flex; justify-content: space-between; align-items: flex-start; gap: 16px; }
  header h2 { margin: 0; font-size: 1.4rem; }
  .eyebrow { text-transform: uppercase; letter-spacing: .08em; font-size: .7rem; }
  p { margin: 10px 0; line-height: 1.5; }
  .fields { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; margin-top: 20px; }
  label { display: flex; flex-direction: column; gap: 6px; font-size: .85rem; }
  input,select,button { color: inherit; font: inherit; }
  input,select { border: 1px solid var(--border-soft); border-radius: 7px; padding: 8px; background: transparent; }
  button { border: 1px solid var(--border-soft); border-radius: 8px; background: transparent; padding: 8px 12px; cursor: pointer; }
  button:disabled { cursor: default; opacity: .5; }
  .hint { font-size: .8rem; color: var(--text-secondary); }
  .preview { border-top: 1px solid var(--border-soft); margin-top: 20px; padding-top: 12px; }
  .confirm { flex-direction: row; align-items: flex-start; margin: 16px 0; }
  .confirm input { margin-top: 3px; }
  .actions { display: flex; flex-wrap: wrap; gap: 8px; }
  .danger { color: var(--color-error, #b02e36); border-color: currentColor; }
  .error { color: var(--color-error, #b02e36); background: #b02e3610; padding: 10px; border-radius: 8px; }
  a { text-decoration: underline; }
  @media (max-width: 520px) { .fields { grid-template-columns: 1fr; } dialog { padding: 18px; } }
</style>
