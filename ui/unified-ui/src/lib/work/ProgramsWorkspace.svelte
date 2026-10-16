<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import { get } from 'svelte/store';
	import { scopeIdentityStore, getCurrentScopeCredentialRevision } from '$lib/stores/scopeIdentityStore';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import ProgramProgress from './ProgramProgress.svelte';
	import { listProgramMembers, readProgramMember, referencesProgram, type ProgramMember } from './programContext';
	import Markdown from '$lib/magician/components/generative/Markdown.svelte';
	import { listPrograms, readProgram, restoreProgram, type ProgramSummary, type ProgramDocument } from './programs';

	export let compact = false;
	let documents: ProgramSummary[] = [], document: ProgramDocument | null = null;
	let members: ProgramMember[] = [], membersLoading = false, membersError = '';
	$: responsibleMembers = selectedName ? members.filter(member => referencesProgram(member,selectedName)) : members;
	let loading = true, detailLoading = false, busy = false, error = '', detailError = '';
	let mounted = false, generation = 0, detailSequence = 0;
	let controller = new AbortController();
	let currentScope = '', currentSelection = '', documentScope = '';
	$: scopeKey = JSON.stringify([$scopeIdentityStore.principal, $scopeIdentityStore.workspace, getCurrentScopeCredentialRevision()]);
	$: selectedName = compact ? '' : $page.url.searchParams.get('program') || '';
	$: if (mounted && scopeKey !== currentScope) { currentScope = scopeKey; void reload(); }
	$: if (mounted && selectedName !== currentSelection) { currentSelection = selectedName; void loadDetail(selectedName); }

	function programScopeKey(): string {
		const scope = get(scopeIdentityStore);
		return JSON.stringify([scope.principal, scope.workspace, getCurrentScopeCredentialRevision()]);
	}
	function current(g: number, scope: string): boolean {
		return mounted && g === generation && scope === programScopeKey();
	}

	async function reload() {
		controller.abort(); controller = new AbortController();
		const g = ++generation, scope = programScopeKey();
		documents = []; document = null; documentScope = ''; error = ''; detailError = ''; loading = true; busy = false;
		members = []; membersError = '';
		currentSelection = selectedName;
		void loadDetail(selectedName);
		if (!compact) void reloadMembers(g,scope,controller.signal);
		try { const result = await listPrograms(controller.signal); if (current(g, scope)) documents = result; }
		catch (e) { if (current(g, scope)) error = e instanceof Error ? e.message : String(e); }
		finally { if (current(g, scope)) loading = false; }
	}

 async function reloadMembers(g: number, scope: string, signal: AbortSignal) {
  membersLoading = true;
  try {
   const result = await listProgramMembers(signal);
   if (!current(g,scope)) return;
   members = result;
   let next = 0;
   await Promise.all(Array.from({length:Math.min(4,result.length)},async () => {
    while (current(g,scope) && next < result.length) {
     const index = next++, member = await readProgramMember(result[index],signal);
     if (current(g,scope)) members = members.map(existing => existing.id === member.id ? member : existing);
    }
   }));
  } catch (error) { if (current(g,scope)) membersError = error instanceof Error ? error.message : String(error); }
  finally { if (current(g,scope)) membersLoading = false; }
 }

	async function loadDetail(name: string) {
		const sequence = ++detailSequence, g = generation, scope = programScopeKey();
		document = null; documentScope = ''; detailError = ''; detailLoading = !!name;
		if (!name) return;
		try { const result = await readProgram(name, controller.signal); if (current(g, scope) && sequence === detailSequence && name === selectedName) { document = result; documentScope = scope; } }
		catch (e) { if (current(g, scope) && sequence === detailSequence) detailError = e instanceof Error ? e.message : String(e); }
		finally { if (current(g, scope) && sequence === detailSequence) detailLoading = false; }
	}

	async function restore(snapshot: string) {
		if (!document || busy) return;
		const name = document.name, g = generation, scope = programScopeKey();
		if (documentScope !== scope) return;
		const confirmed = await requestConfirmation({ title: 'Restore program version', message: `Restore ${document.title} from ${snapshot}? The current document will be saved as a history snapshot.`, confirmLabel: 'Restore version' });
		if (!confirmed || !current(g, scope) || name !== selectedName) return;
		busy = true; detailError = '';
		try { await restoreProgram(name, snapshot, controller.signal); if (current(g, scope) && name === selectedName) await loadDetail(name); }
		catch (e) { if (current(g, scope) && name === selectedName) detailError = e instanceof Error ? e.message : String(e); }
		finally { if (mounted && g === generation) busy = false; }
	}

	onMount(() => { mounted = true; });
	onDestroy(() => { mounted = false; ++generation; controller.abort(); });
</script>

<section class:compact aria-label="Programs" class="programs">
	{#if !compact}<div class="heading"><div><h2>Programs</h2><p>Durable direction for your crew. Read the direction, current progress and evidence, then open the responsible crew member’s controls.</p></div><button on:click={reload} disabled={loading}>Refresh</button></div>{/if}
	{#if error}<div role="alert">{error} <button on:click={reload}>Retry</button></div>
	{:else if loading}<p role="status">Loading programs…</p>
	{:else if !documents.length}<p class="muted">No program documents in this workspace.</p>
	{:else}<div class="program-list">
		{#each compact ? documents.slice(0, 3) : documents as item (item.name)}
			<a href={`/work?view=programs&program=${encodeURIComponent(item.name)}`} aria-current={selectedName === item.name ? 'page' : undefined}><span class="kind">Program</span><strong>{item.title}</strong><span class="muted">{item.name} →</span></a>
		{/each}
		{#if compact && documents.length > 3}<a href="/work?view=programs"><strong>All {documents.length} programs →</strong></a>{/if}
	</div>{/if}
	{#if !compact && selectedName}
		<article class="program-detail">
			<button on:click={() => goto('/work?view=programs')}>← Programs</button>
			{#if detailError}<p role="alert">{detailError}</p><button on:click={() => loadDetail(selectedName)}>Retry</button>{/if}
			{#if detailLoading}<p role="status">Loading program…</p>{:else if document}
				<h2>{document.title}</h2><p class="muted">{document.name}</p>
				<Markdown content={document.content} />
				<details><summary>Document history · {document.history.length}</summary>
					{#each document.history as version (version.snapshot)}<div class="version"><div><strong>{version.snapshot}</strong><p>{version.modified}</p></div><button disabled={busy} on:click={() => restore(version.snapshot)}>Restore version</button></div>{/each}
					{#if !document.history.length}<p>No saved versions.</p>{/if}
				</details>
				<div class="links"><a href="/work?view=crew">Crew and program progress →</a><a href="/work?view=operations">Company loop and diagnostics →</a></div>
			{/if}
		</article>
	{/if}
 {#if !compact}
  <section class="responsible" aria-label="Responsible crew and program progress">
   <h2>{selectedName ? 'Responsible crew and progress' : 'Runtime direction and progress'}</h2>
   {#if membersError}<p role="alert">{membersError}</p><button on:click={reload}>Retry progress</button>{/if}
   {#if membersLoading && !members.length}<p role="status">Loading responsible crew…</p>{/if}
   {#if responsibleMembers.length}<div class="progress-list">{#each responsibleMembers as member (member.id)}<ProgramProgress {member} program={selectedName} />{/each}</div>
   {:else if !membersLoading && !membersError}<p class="muted">{selectedName ? 'No crew member is configured to use this document.' : 'No program-enabled crew in this workspace.'}</p>{/if}
  </section>
 {/if}
</section>

<style>
	.responsible {margin-top:var(--space-lg);}
	.progress-list {display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,300px),1fr));gap:var(--space-md);}
	.programs { padding: var(--space-md); color: var(--text-primary); min-width: 0; }
	.compact { padding: 0; }
	.heading,.version { display:flex; align-items:center; justify-content:space-between; gap:var(--space-sm); flex-wrap:wrap; }
	h2 { font-family:var(--font-display); margin:0 0 var(--space-sm); } p { line-height:1.5; }
	.heading p,.muted,.version p { color:var(--text-secondary); font-size:var(--text-sm); }
	.program-list { display:grid; grid-template-columns:repeat(auto-fit,minmax(min(100%,240px),1fr)); gap:var(--space-sm); }
	.program-list a { display:flex; flex-direction:column; gap:var(--space-sm); padding:var(--space-md); color:var(--text-primary); background:var(--bg-card); border:1px solid var(--border-soft); border-radius:var(--radius-lg); text-decoration:none; min-width:0; overflow-wrap:anywhere; }
	.program-list a:hover,.program-list a[aria-current] { border-color:var(--accent-primary); }
	.kind { font-size:var(--text-xs); color:var(--text-secondary); }
	.program-detail { margin-top:var(--space-md); padding:var(--space-md); background:var(--bg-card); border:1px solid var(--border-soft); border-radius:var(--radius-lg); overflow-wrap:anywhere; }
	.program-detail h2 { margin-top:var(--space-md); } details { margin-top:var(--space-md); } summary { padding:var(--space-sm) 0; cursor:pointer; }
	.version { padding:var(--space-sm) 0; border-bottom:1px solid var(--border-soft); } .version strong { overflow-wrap:anywhere; }
	.links { display:flex; flex-wrap:wrap; gap:var(--space-md); margin-top:var(--space-md); } .links a { color:var(--text-primary); }
	button { min-height:44px; padding:var(--space-sm) var(--space-sm); border:1px solid var(--border-soft); border-radius:var(--radius-md); color:var(--text-primary); background:var(--bg-card); font:inherit; cursor:pointer; } button:disabled { opacity:.6; cursor:default; }
	a:focus-visible,button:focus-visible,summary:focus-visible { outline:2px solid var(--accent-primary); outline-offset:3px; }
</style>
