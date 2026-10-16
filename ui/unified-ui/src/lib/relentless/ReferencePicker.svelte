<script lang="ts">
  import { onDestroy } from 'svelte';
  import { findReferences, readReference } from './references';
  import type { NoteSearchResults, NoteSearchHit } from '$lib/notes/noteSearch';
  import type { SourceReference } from './workspace';
  export let references:SourceReference[] = [];
  export let scopeEpoch = 0;
  export let disabled = false;
  export let pending = false;
  let query = '', results:NoteSearchResults|null = null, error = '', busy = false;
  let epoch = -1, revision = 0, alive = true, abort:AbortController|null = null;
  $: pending = busy;
  $: if (epoch !== scopeEpoch) {epoch=scopeEpoch;revision++;abort?.abort();query='';results=null;error='';busy=false;}
  function begin() {abort?.abort();abort=new AbortController();revision++;error='';return {version:revision,epoch:scopeEpoch,signal:abort.signal};}
  function valid(token:{version:number;epoch:number}) {return alive && token.version === revision && token.epoch === scopeEpoch;}
  async function search() {
    if (disabled || !query.trim()) return;
    const token = begin(), original=query.trim();busy=true;
    try {const next=await findReferences(original,token.signal);if(valid(token)&&query.trim()===original)results=next;}
    catch(e) {if(valid(token))error=String(e);}
    finally {if(valid(token))busy=false;}
  }
  async function attach(hit:NoteSearchHit) {
    if(disabled || busy)return;
    const token=begin();busy=true;
    try {const next=await readReference(hit,token.signal);if(valid(token)&&!disabled){references=[...references.filter(ref=>ref.id!==next.id),next];results=null;query='';}}
    catch(e){if(valid(token))error=String(e);}
    finally{if(valid(token))busy=false;}
  }
  async function upload(event:Event) {
    if(disabled)return;
    const input=event.target as HTMLInputElement, file=input.files?.[0];if(!file)return;
    const token=begin();busy=true;
    try {
      // Files are text evidence here. Binary media uses the complete contract's
      // existing source-file admission instead of a lossy text conversion.
      if(file.size>1_048_576)throw new Error('Use a text reference up to 1 MB, or the complete contract for larger source files.');
      const content=new TextDecoder('utf-8',{fatal:true}).decode(await file.arrayBuffer());
      if(content.includes('\u0000'))throw new Error('Attach a UTF-8 text document. Binary files belong in the complete contract.');
      if(valid(token)&&!disabled)references=[...references,{id:`file:${crypto.randomUUID()}`,title:file.name,origin:`Uploaded file: ${file.name}`,content}];
    }catch(e){if(valid(token))error=String(e);}
    finally{input.value='';if(valid(token))busy=false;}
  }
  function invalidateSearch() {revision++;abort?.abort();results=null;error='';busy=false;}
  onDestroy(()=>{alive=false;revision++;abort?.abort();});
</script>

<div class="references">
  <div class="heading"><h3>References</h3><span>{references.length} attached</span></div>
  <p>Attach the request, fact sheet or supporting notes. Their contents are frozen when you review the brief.</p>
  <div class="search">
    <label class="search-label">Find a reference<input aria-label="Find a reference" bind:value={query} on:input={invalidateSearch} on:keydown={event=>{if(event.key==='Enter'){event.preventDefault();void search();}}} placeholder="Search notes and documents…" disabled={disabled||busy}/></label>
    <button type="button" on:click={search} disabled={disabled||busy||!query.trim()}>{busy?'Loading…':'Search notes'}</button>
    <label class="upload">Attach text file<input type="file" aria-label="Attach text file" on:change={upload} disabled={disabled||busy}/></label>
  </div>
  {#if error}<p role="alert" class="error">{error}</p>{/if}
  {#if results}
    <ul class="matches" aria-label="Reference search results">
      {#each results.hits as hit (hit.source_ref)}<li><button type="button" disabled={disabled||busy} on:click={()=>attach(hit)}><strong>{hit.title}</strong><small>{hit.relative_path}</small><span>{hit.matches[0]?.text??'Attach full document'}</span></button></li>{/each}
    </ul>
    {#if !results.hits.length}<p>No references matched in the current notes provider.</p>{/if}
    {#if results.more_available||results.scan_truncated}<p>More documents may be available. Narrow your search.</p>{/if}
  {/if}
  {#each references as ref (ref.id)}
    <div class="attached"><details><summary>{ref.title}</summary><small>{ref.origin}</small><pre>{ref.content}</pre></details><button type="button" aria-label={`Remove reference ${ref.title}`} disabled={disabled||busy} on:click={()=>references=references.filter(item=>item.id!==ref.id)}>Remove</button></div>
  {/each}
</div>

<style>
  .references{padding:1rem;border:1px solid var(--border-soft);border-radius:var(--radius-md,12px);background:var(--bg-soft)}
  .heading,.search,.attached{display:flex;align-items:center;gap:.75rem}.heading{justify-content:space-between}h3{margin:0;font-size:.95rem}p,small,.heading span{color:var(--text-secondary);font-size:.8rem}p{margin:.5rem 0 .85rem}.search{flex-wrap:wrap;align-items:flex-end}.search-label{flex:1;min-width:12rem;display:flex;flex-direction:column;gap:.3rem;font-size:.8rem}input{width:100%;padding:.6rem .7rem;border:1px solid var(--input-border,var(--border-soft));border-radius:8px;background:var(--input-bg,var(--bg-card));color:var(--text-primary);font:inherit}button,.upload{padding:.6rem .75rem;border:1px solid var(--border-soft);border-radius:8px;background:var(--bg-card);color:var(--text-primary);font:inherit;font-size:.8rem;cursor:pointer}button:disabled,.upload:has(input:disabled){opacity:.5;cursor:default}.upload{position:relative;overflow:hidden}.upload input{position:absolute;inset:0;opacity:0;cursor:pointer}.upload:focus-within,button:focus-visible,input:focus-visible{outline:2px solid var(--accent-primary);outline-offset:2px}.matches{padding:0;list-style:none;max-height:18rem;overflow:auto}.matches button{width:100%;text-align:left;display:grid;gap:.25rem;margin:.2rem 0}.matches span{font-size:.8rem;color:var(--text-secondary)}.attached{justify-content:space-between;padding-top:.6rem}.attached details{min-width:0;flex:1}summary{cursor:pointer;font-size:.85rem;overflow-wrap:anywhere}pre{max-height:14rem;overflow:auto;white-space:pre-wrap;overflow-wrap:anywhere;font-size:.8rem}.error{color:var(--status-failed)}
</style>
