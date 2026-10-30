<script lang="ts">
 import { onMount, onDestroy } from 'svelte';
 import { scopeIdentityStore, scopedRequestHeaders, getCurrentScopeCredentialRevision } from '$lib/stores/scopeIdentityStore';
 import { timedFetch } from '$lib/shared/fetch';
 import ServerPager from '$lib/shared/components/ServerPager.svelte';
 type Row = { id:string; title?:string; identity?:string; state:string; at:string; provider?:string; audience?:{kind:string;id:string}; slots?:unknown[]; held?:unknown; reply_agent_id?:string };
 let expanded = false;
 let kind: 'scheduling'|'delivery' = 'scheduling';
 let rows: Row[] = [];
 let offset = 0;
 let total = 0;
 const limit = 20;
 let loading = false;
 let error = '';
 let detail = '';
 let generation = 0;
 let scopeKey = '';
 let scopeReady = false;
 let unsubscribe = () => {};
 async function load() {
   if (!scopeReady) { loading = true; return; }
   const ticket = ++generation;
   loading = true; error = ''; detail = '';
   try {
     const response = await timedFetch(`/api/magician/v2/work/follow-through?kind=${kind}&offset=${offset}&limit=${limit}`, {headers:scopedRequestHeaders()});
     if (!response.ok) throw new Error(`Could not load follow-through (${response.status}).`);
     const result = await response.json();
     if (ticket !== generation) return;
     rows = result.items; total = result.total;
   } catch (cause) { if (ticket === generation) { rows = []; error = cause instanceof Error ? cause.message : 'Could not load follow-through.'; } }
   finally { if (ticket === generation) loading = false; }
 }
 function selectKind(value:'scheduling'|'delivery') { kind = value; offset = 0; void load(); }
 async function inspect(row:Row) {
   const ticket = ++generation;
   error = ''; detail = '';
   const path = kind === 'delivery' ? `/delivery/receipts?act_ref=${encodeURIComponent(row.id)}` : `/work/negotiations/${encodeURIComponent(row.id)}?audience_kind=${encodeURIComponent(row.audience?.kind ?? '')}&audience_id=${encodeURIComponent(row.audience?.id ?? '')}`;
   try {
     const response = await timedFetch('/api/magician/v2'+path, {headers:scopedRequestHeaders()});
     if (!response.ok) throw new Error(`Could not read this record (${response.status}).`);
     const result = await response.json();
     if (ticket !== generation) return;
     // Keep private raw provider bytes out of this compact view. The existing
     // authenticated API retains the complete intake audit for inspection.
     detail = JSON.stringify(kind === 'delivery' ? {knowledge:result.knowledge,dispatch_recorded:result.dispatch_recorded,observations:result.observations,attempt_count:result.attempt_count} : result, null, 2);
   } catch(cause) { if (ticket === generation) error = cause instanceof Error ? cause.message : 'Could not read this record.'; }
 }
 onMount(() => { unsubscribe = scopeIdentityStore.subscribe(scope => {
   scopeReady = scope.isResolved;
   const key = `${scope.principal}/${scope.workspace}/${getCurrentScopeCredentialRevision()}/${scope.isResolved}`;
   if (key !== scopeKey) { scopeKey = key; ++generation; rows = []; total = 0; offset = 0; detail = ''; error = ''; loading = false; if (expanded && scope.isResolved) void load(); }
 }); });
 onDestroy(() => { ++generation; unsubscribe(); });
</script>

<section class="follow-through" aria-label="Scheduling and delivery follow-through">
 <button class="panel-toggle" type="button" aria-expanded={expanded} on:click={() => { expanded = !expanded; if(expanded) void load(); else { ++generation; loading = false; } }}>Scheduling and delivery <span>{expanded ? '−' : '+'}</span></button>
 {#if expanded}
  <div class="controls"><div role="group" aria-label="Follow-through type"><button type="button" aria-pressed={kind === 'scheduling'} on:click={() => selectKind('scheduling')}>Scheduling</button><button type="button" aria-pressed={kind === 'delivery'} on:click={() => selectKind('delivery')}>Delivery</button></div><button type="button" disabled={loading} on:click={() => void load()}>Refresh</button></div>
  <p>Reply work and owner decisions use the existing Work and Attention lists.</p>
  {#if error}<p role="alert">{error}</p>{:else if loading}<p role="status">Loading…</p>{:else if rows.length === 0}<p>No {kind} records in this workspace.</p>{:else}
   <div class="records">{#each rows as row (row.id)}<button class="record" type="button" on:click={() => void inspect(row)}><span><strong>{row.title || row.identity || row.id}</strong><small>{row.title ? row.identity : row.provider || 'Provider unknown'}</small></span><span class="state">{row.state.replaceAll('_',' ')}</span></button>{/each}</div>
   <ServerPager currentPage={Math.floor(offset/limit)+1} pageCount={Math.max(1,Math.ceil(total/limit))} startItem={offset+1} endItem={Math.min(offset+limit,total)} totalItems={total} {loading} on:pagechange={(event) => { offset = (event.detail.page-1)*limit; void load(); }} />
  {/if}
  {#if detail}<div class="detail"><button type="button" on:click={() => detail = ''}>Close details</button><pre>{detail}</pre></div>{/if}
 {/if}
</section>

<style>
 .follow-through { border:1px solid var(--border-soft); border-radius:12px; background:linear-gradient(180deg, var(--bg-card) 0%, var(--bg-soft) 100%); padding:var(--space-md); margin-bottom:0; color:var(--text-primary); box-shadow:0 1px 3px rgba(0,0,0,0.04); }
 button { color:var(--text-primary); background:var(--bg-card); border:1px solid var(--border-soft); border-radius:var(--radius-chip, 6px); padding:0.3rem 0.65rem; cursor:pointer; min-height:28px; font-size:var(--text-xs); box-sizing:border-box; transition:all 0.15s ease; }
 button:hover:not(:disabled) { border-color:var(--accent-primary); }
 button:focus-visible { outline:2px solid var(--accent-primary); outline-offset:2px; }
 button[aria-pressed=true] { background:color-mix(in srgb, var(--accent-primary) 15%, transparent); border-color:var(--accent-primary); color:var(--accent-primary); font-weight:600; }
 .panel-toggle { display:flex; align-items:center; justify-content:space-between; width:100%; border:0; background:transparent; font-weight:600; font-size:var(--text-sm); text-align:left; padding:0; min-height:auto; }
 .panel-toggle:hover { color:var(--accent-primary); }
 .controls { display:flex; justify-content:space-between; gap:var(--space-sm); margin-top:var(--space-sm); } .controls div { display:flex; gap:var(--space-xs); }
 p,small { color:var(--text-secondary); font-size:var(--text-xs); } p[role=alert] { color:var(--text-primary); }
 .records { display:grid; gap:var(--space-xs); } .record { display:flex; justify-content:space-between; gap:var(--space-sm); text-align:left; } .record > span:first-child { min-width:0; } strong,small { display:block; overflow-wrap:anywhere; } .state { font-size:var(--text-xs); white-space:nowrap; }
 .detail { margin-top:var(--space-md); } pre { overflow:auto; white-space:pre-wrap; overflow-wrap:anywhere; max-height:360px; font-size:var(--text-xs); }
</style>
