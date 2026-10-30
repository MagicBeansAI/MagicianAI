<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import { get } from 'svelte/store';
	import { scopeIdentityStore, getCurrentScopeCredentialRevision } from '$lib/stores/scopeIdentityStore';
	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import ProgramProgress from './ProgramProgress.svelte';
	import WorkHelp from './WorkHelp.svelte';
	import { WORK_HELP } from './help';
	import { listProgramMembers, readProgramMember, referencesProgram, type ProgramMember } from './programContext';
	import Markdown from '$lib/magician/components/generative/Markdown.svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { listPrograms, readProgram, restoreProgram, saveProgram, ProgramChangedError, type ProgramSummary, type ProgramDocument } from './programs';

	export let compact = false;
	let documents: ProgramSummary[] = [], document: ProgramDocument | null = null;
	let members: ProgramMember[] = [], membersLoading = false, membersError = '';
	$: responsibleMembers = selectedName ? members.filter(member => referencesProgram(member,selectedName)) : members;
	let loading = true, detailLoading = false, busy = false, error = '', detailError = '';
	// Owner edit: the draft is bound to the document (name + hash) it was opened from.
	let editing = false, draft = '', editName = '', editBase = '', saveError = '', conflict = false;
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
		stopEditing();
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
		stopEditing();
		if (!name) return;
		try { const result = await readProgram(name, controller.signal); if (current(g, scope) && sequence === detailSequence && name === selectedName) { document = result; documentScope = scope; } }
		catch (e) { if (current(g, scope) && sequence === detailSequence) detailError = e instanceof Error ? e.message : String(e); }
		finally { if (current(g, scope) && sequence === detailSequence) detailLoading = false; }
	}

	function stopEditing() {
		editing = false; draft = ''; editName = ''; editBase = ''; saveError = ''; conflict = false;
	}

	function startEdit() {
		if (!document || busy || documentScope !== programScopeKey()) return;
		editing = true; draft = document.content; editName = document.name; editBase = document.sha256; saveError = ''; conflict = false;
	}

	async function confirmDiscard(): Promise<boolean> {
		if (!editing || !document || draft === document.content) return true;
		return requestConfirmation({ title: 'Discard changes', message: `Discard your unsaved changes to ${document.title}?`, confirmLabel: 'Discard changes' });
	}

	async function cancelEdit() {
		if (busy) return;
		const g = generation, scope = programScopeKey();
		if (!(await confirmDiscard()) || !current(g, scope)) return;
		stopEditing();
	}

	async function reloadLatest() {
		if (busy) return;
		const name = selectedName, g = generation, scope = programScopeKey();
		if (!(await confirmDiscard()) || !current(g, scope) || name !== selectedName) return;
		await loadDetail(name);
	}

	async function save() {
		if (!document || !editing || busy) return;
		const name = editName, content = draft, base = editBase, g = generation, scope = programScopeKey();
		if (documentScope !== scope || name !== selectedName || !content.trim()) return;
		busy = true; saveError = ''; conflict = false;
		try {
			await saveProgram(name, content, base, controller.signal);
			if (current(g, scope) && name === selectedName) await loadDetail(name);
		} catch (e) {
			if (!current(g, scope) || name !== selectedName) return;
			if (e instanceof ProgramChangedError) {
				conflict = true;
				saveError = `${e.message} Your edits are still in the editor; copy anything you need, then reload the latest version.`;
			} else saveError = e instanceof Error ? e.message : String(e);
		} finally { if (mounted && g === generation) busy = false; }
	}

	async function restore(snapshot: string) {
		if (!document || busy || editing) return;
		const name = document.name, g = generation, scope = programScopeKey();
		if (documentScope !== scope) return;
		const confirmed = await requestConfirmation({ title: 'Restore playbook version', message: `Restore ${document.title} from ${snapshot}? The current document will be saved as a history snapshot.`, confirmLabel: 'Restore version' });
		if (!confirmed || !current(g, scope) || name !== selectedName) return;
		busy = true; detailError = '';
		try { await restoreProgram(name, snapshot, controller.signal); if (current(g, scope) && name === selectedName) await loadDetail(name); }
		catch (e) { if (current(g, scope) && name === selectedName) detailError = e instanceof Error ? e.message : String(e); }
		finally { if (mounted && g === generation) busy = false; }
	}

	onMount(() => { mounted = true; });
	onDestroy(() => { mounted = false; ++generation; controller.abort(); });
</script>

<section class:compact aria-label="Playbooks" class="programs">
	{#if compact}
		<header class="compact-header">
			<div class="compact-title-group">
				<span class="compact-icon" aria-hidden="true">🧭</span>
				<div>
					<h3 class="compact-title">Playbooks</h3>
					<span class="compact-subtitle">Durable operational direction</span>
				</div>
			</div>
			<div class="compact-header-actions">
				<WorkHelp topic={WORK_HELP.programs} />
				<a href="/work?view=programs" class="compact-view-all">All playbooks <span class="compact-counter">{documents.length}</span> →</a>
			</div>
		</header>
		{#if error}<div role="alert" class="compact-alert">{error} <button on:click={reload} class="compact-retry-btn">Retry</button></div>
		{:else if loading}<p role="status" class="compact-loading">Loading playbooks…</p>
		{:else if !documents.length}
			<div class="compact-empty">
				<p class="muted compact-empty-text">No playbook documents in this workspace.</p>
				<a href="/chat" class="compact-action-link">Ask Magican in chat to write one →</a>
			</div>
		{:else}
			<div class="program-list compact-card-list">
				{#each documents.slice(0, 4) as item (item.name)}
					<a href={`/work?view=programs&program=${encodeURIComponent(item.name)}`} class="compact-item-card" aria-current={selectedName === item.name ? 'page' : undefined}>
						<strong class="compact-item-title">{item.title}</strong>
						<div class="compact-item-tags">
							<span class="kind">Playbook</span>
							<span class="muted compact-item-link">{item.name} →</span>
						</div>
					</a>
				{/each}
			</div>
		{/if}
	{:else}
		<!-- Top Command Masthead -->
		<header class="programs-masthead">
			<div class="programs-masthead__identity">
				<div class="programs-masthead__badge-row">
					<span class="masthead-badge">Crew playbooks</span>
					<span class="masthead-count">{documents.length} playbook{documents.length === 1 ? '' : 's'}</span>
				</div>
				<h2>Playbooks <WorkHelp topic={WORK_HELP.programs} /></h2>
				<p>{WORK_HELP.programs.description}</p>
			</div>
			<div class="programs-masthead__actions">
				<button type="button" class="action-btn" on:click={reload} disabled={loading} title="Reload playbook documents">
					<Icon name="rotate-ccw" size={13} />
					<span>{loading ? 'Refreshing...' : 'Refresh'}</span>
				</button>
			</div>
		</header>
		{#if error}<div role="alert">{error} <button on:click={reload}>Retry</button></div>
		{:else if loading}<p role="status">Loading playbooks…</p>
		{:else if !documents.length}<p class="muted">No playbook documents in this workspace. <a href="/chat">Ask Magican in chat</a> to write one for a crew member.</p>
		{:else}<div class="program-list">
			{#each documents as item (item.name)}
				<a href={`/work?view=programs&program=${encodeURIComponent(item.name)}`} aria-current={selectedName === item.name ? 'page' : undefined}><span class="kind">Playbook</span><strong>{item.title}</strong><span class="muted">{item.name} →</span></a>
			{/each}
		</div>{/if}
	{/if}
	{#if !compact && selectedName}
		<article class="program-detail">
			<button on:click={() => goto('/work?view=programs')}>← Playbooks</button>
			{#if detailError}<p role="alert">{detailError}</p><button on:click={() => loadDetail(selectedName)}>Retry</button>{/if}
			{#if detailLoading}<p role="status">Loading playbook…</p>{:else if document}
				<div class="detail-title"><div><h2>{document.title}</h2><p class="muted">{document.name}</p></div>{#if !editing}<button on:click={startEdit} disabled={busy}>Edit</button>{/if}</div>
				{#if editing}
					<label class="editor-label" for="program-editor">Markdown for {document.name}</label>
					<textarea id="program-editor" bind:value={draft} spellcheck="false" disabled={busy}></textarea>
					{#if saveError}<p role="alert">{saveError}</p>{#if conflict}<button on:click={reloadLatest} disabled={busy}>Reload latest version</button>{/if}{/if}
					<div class="editor-actions"><button class="primary" on:click={save} disabled={busy || conflict || draft === document.content || !draft.trim()}>{busy ? 'Saving…' : 'Save'}</button><button on:click={cancelEdit} disabled={busy}>Cancel</button></div>
					<p class="muted">Saving keeps the current version in Document history, so you can restore it.</p>
				{:else}
					<Markdown content={document.content} />
				{/if}
				<details><summary>Document history · {document.history.length}</summary>
					{#each document.history as version (version.snapshot)}<div class="version"><div><strong>{version.snapshot}</strong><p>{version.modified}</p></div><button disabled={busy || editing} on:click={() => restore(version.snapshot)}>Restore version</button></div>{/each}
					{#if !document.history.length}<p>No saved versions.</p>{/if}
				</details>
				<div class="links"><a href="/work?view=crew">Crew and playbook progress →</a><a href="/work?view=operations">Team routines and diagnostics →</a></div>
			{/if}
		</article>
	{/if}
 {#if !compact}
  <section class="responsible" aria-label="Responsible crew and playbook progress">
   <h2>{selectedName ? 'Responsible crew and progress' : 'Runtime direction and progress'}</h2>
   {#if membersError}<p role="alert">{membersError}</p><button on:click={reload}>Retry progress</button>{/if}
   {#if membersLoading && !members.length}<p role="status">Loading responsible crew…</p>{/if}
   {#if responsibleMembers.length}<div class="progress-list">{#each responsibleMembers as member (member.id)}<ProgramProgress {member} program={selectedName} />{/each}</div>
   {:else if !membersLoading && !membersError}<p class="muted">{selectedName ? 'No crew member is configured to use this document.' : 'No playbook-enabled crew in this workspace.'}</p>{/if}
  </section>
 {/if}
</section>

<style>
	.responsible { margin-top: var(--space-lg); }
	.progress-list { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 300px), 1fr)); gap: var(--space-md); }
	.programs {
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		box-sizing: border-box;
		padding: var(--space-md) 1.45rem;
		color: var(--text-primary);
		min-width: 0;
	}
	.programs.compact {
		width: auto;
		max-width: none;
		margin: 0;
		padding: var(--space-sm) var(--space-md);
		background: linear-gradient(
			145deg,
			color-mix(in srgb, var(--accent-primary) 6%, var(--bg-card)) 0%,
			color-mix(in srgb, var(--bg-soft, var(--bg-card)) 30%, var(--bg-card)) 60%,
			var(--bg-card) 100%
		);
		border: 1px solid color-mix(in srgb, var(--accent-primary) 20%, var(--border-soft));
		border-radius: var(--radius-lg, 12px);
		box-shadow: 0 4px 16px color-mix(in srgb, var(--accent-primary) 4%, rgba(0, 0, 0, 0.03));
		display: flex;
		flex-direction: column;
		gap: var(--space-xs);
		box-sizing: border-box;
		min-width: 0;
		transition: border-color 0.18s ease, box-shadow 0.18s ease;
	}
	.programs.compact:hover {
		border-color: color-mix(in srgb, var(--accent-primary) 40%, var(--border-soft));
		box-shadow: 0 6px 20px color-mix(in srgb, var(--accent-primary) 8%, rgba(0, 0, 0, 0.05));
	}
	.compact-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-sm);
		padding-bottom: var(--space-2xs, 4px);
		border-bottom: 1px solid color-mix(in srgb, var(--accent-primary) 14%, var(--border-soft));
	}
	.compact-title-group {
		display: flex;
		align-items: center;
		gap: var(--space-xs);
		min-width: 0;
	}
	.compact-icon {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 26px;
		height: 26px;
		border-radius: var(--radius-md, 6px);
		background: linear-gradient(
			135deg,
			color-mix(in srgb, var(--accent-primary) 18%, var(--bg-card)) 0%,
			var(--bg-card) 100%
		);
		border: 1px solid color-mix(in srgb, var(--accent-primary) 30%, var(--border-soft));
		font-size: 13px;
		flex-shrink: 0;
	}
	.compact-title {
		font-family: var(--font-display);
		font-size: var(--text-sm, 0.85rem);
		font-weight: 700;
		margin: 0;
		color: var(--text-primary);
		line-height: 1.2;
	}
	.compact-subtitle {
		font-size: var(--text-2xs, 0.72rem);
		color: var(--text-muted);
		line-height: 1.2;
		display: block;
	}
	.compact-header-actions {
		display: flex;
		align-items: center;
		gap: 6px;
		flex-shrink: 0;
	}
	.compact-view-all {
		display: inline-flex;
		align-items: center;
		gap: 5px;
		font-size: var(--text-xs, 0.78rem);
		font-weight: 600;
		color: var(--text-secondary);
		text-decoration: none;
		padding: 4px 8px;
		border-radius: var(--radius-md, 8px);
		background: linear-gradient(
			180deg,
			var(--bg-card) 0%,
			color-mix(in srgb, var(--bg-card) 92%, var(--bg-soft, var(--bg-base))) 100%
		);
		border: 1px solid var(--border-soft);
		white-space: nowrap;
		transition: all 0.15s ease;
		flex-shrink: 0;
	}
	.compact-view-all:hover {
		color: var(--text-primary);
		background: linear-gradient(
			135deg,
			color-mix(in srgb, var(--accent-primary) 14%, var(--bg-card)) 0%,
			var(--bg-card) 100%
		);
		border-color: color-mix(in srgb, var(--accent-primary) 40%, var(--border-soft));
	}
	.compact-counter {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		padding: 1px 5px;
		border-radius: var(--radius-sm, 4px);
		background: var(--bg-card);
		color: var(--text-primary);
		font-size: var(--text-2xs, 0.72rem);
		font-weight: 700;
		border: 1px solid var(--border-soft);
		line-height: 1;
	}
	.compact-card-list {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 130px), 1fr));
		gap: 6px;
		align-items: stretch;
	}
	.compact-item-card {
		display: flex;
		flex-direction: column;
		justify-content: space-between;
		gap: 6px;
		padding: 9px 12px;
		background: linear-gradient(
			180deg,
			var(--bg-card) 0%,
			color-mix(in srgb, var(--bg-card) 88%, var(--bg-soft, var(--bg-base))) 100%
		);
		border: 1px solid color-mix(in srgb, var(--border-soft) 85%, transparent);
		border-radius: var(--radius-md, 8px);
		text-decoration: none;
		color: var(--text-primary);
		min-width: 0;
		min-height: 68px;
		overflow-wrap: anywhere;
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.02);
		transition: transform 0.12s ease, border-color 0.12s ease, box-shadow 0.12s ease, background 0.12s ease;
	}
	.compact-item-card:hover {
		background: linear-gradient(
			180deg,
			color-mix(in srgb, var(--accent-primary) 5%, var(--bg-card)) 0%,
			var(--bg-card) 100%
		);
		border-color: color-mix(in srgb, var(--accent-primary) 45%, var(--border-soft));
		transform: translateY(-1px);
		box-shadow: 0 4px 14px color-mix(in srgb, var(--accent-primary) 8%, rgba(0, 0, 0, 0.05));
	}
	.compact-item-card:focus-visible {
		outline: 2px solid var(--accent-primary);
		outline-offset: 2px;
	}
	.compact-item-title {
		font-size: var(--text-sm, 0.85rem);
		font-weight: 600;
		color: var(--text-primary);
		line-height: 1.3;
		margin: 0;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		-webkit-box-orient: vertical;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.compact-item-tags {
		display: flex;
		align-items: center;
		justify-content: space-between;
		flex-wrap: wrap;
		gap: 4px;
		margin-top: auto;
		padding-top: 2px;
	}
	.compact-item-link {
		color: var(--text-secondary);
		font-weight: 500;
		font-size: var(--text-2xs, 0.72rem);
	}
	.compact-empty {
		padding: var(--space-md) var(--space-sm);
		text-align: center;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 6px;
		color: var(--text-secondary);
		font-size: var(--text-xs);
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-md, 8px);
		background: var(--bg-surface, var(--bg-base));
	}
	.compact-empty p { margin: 0; }
	.compact-action-link {
		color: var(--text-primary);
		font-weight: 600;
		text-decoration: none;
	}
	.compact-action-link:hover {
		text-decoration: underline;
	}
	.compact-loading, .compact-alert {
		padding: var(--space-sm);
		font-size: var(--text-xs);
		border-radius: var(--radius-md, 8px);
		display: flex;
		align-items: center;
		gap: var(--space-sm);
	}
	.compact-loading {
		color: var(--text-muted);
	}
	.compact-alert {
		color: var(--color-error);
		background: var(--color-error-soft);
		border: 1px solid rgba(255,107,107,0.2);
	}
	.compact-retry-btn {
		min-height: 26px;
		padding: 2px 8px;
		font-size: var(--text-2xs);
		border-radius: var(--radius-sm, 6px);
		cursor: pointer;
	}
	.programs-masthead {
		align-items: flex-start;
		background:
			radial-gradient(ellipse at top right, color-mix(in srgb, var(--accent-primary) 12%, transparent), transparent 60%),
			linear-gradient(180deg, color-mix(in srgb, var(--bg-card) 96%, var(--bg-soft) 4%), var(--bg-card));
		border: 1px solid var(--border-soft);
		border-radius: 12px;
		display: flex;
		flex-wrap: wrap;
		gap: 1.25rem;
		justify-content: space-between;
		padding: 1.25rem 1.5rem;
		margin-bottom: var(--space-md);
		box-sizing: border-box;
	}

	.programs-masthead__identity {
		max-width: min(48rem, 100%);
		min-width: 0;
	}

	.programs-masthead__badge-row {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.6rem;
		margin-bottom: 0.4rem;
	}

	.masthead-badge {
		align-items: center;
		background: color-mix(in srgb, var(--accent-primary) 10%, transparent);
		border: 1px solid color-mix(in srgb, var(--accent-primary) 28%, var(--border-soft));
		border-radius: var(--radius-chip, 6px);
		color: var(--accent-primary);
		display: inline-flex;
		font-size: 0.72rem;
		font-weight: 700;
		letter-spacing: 0.04em;
		padding: 0.2rem 0.55rem;
		text-transform: uppercase;
	}

	.masthead-count {
		color: var(--text-muted);
		font-size: 0.76rem;
		font-weight: 500;
	}

	.programs-masthead h2 {
		font-family: var(--font-display, var(--font-primary));
		font-size: 1.85rem;
		font-weight: 700;
		letter-spacing: -0.02em;
		line-height: 1.15;
		margin: 0.15rem 0 0.4rem;
		color: var(--text-primary);
	}

	.programs-masthead p {
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.45;
		margin: 0;
	}

	.programs-masthead__actions {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.45rem;
		align-self: flex-start;
	}

	.programs-masthead__actions .action-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		min-height: 1.85rem;
		padding: 0.32rem 0.75rem;
		font-size: 0.78rem;
		font-weight: 600;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-chip, 6px);
		color: var(--text-secondary);
		background: var(--bg-card);
		cursor: pointer;
		box-sizing: border-box;
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.02);
		transition: all 0.15s ease;
	}

	.programs-masthead__actions .action-btn:hover:not(:disabled) {
		color: var(--text-primary);
		border-color: color-mix(in srgb, var(--accent-primary) 35%, var(--border-soft));
		background: var(--bg-soft);
		box-shadow: 0 2px 6px color-mix(in srgb, var(--accent-primary) 8%, rgba(0, 0, 0, 0.04));
	}

	.programs-masthead__actions .action-btn:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.version { display: flex; align-items: center; justify-content: space-between; gap: var(--space-sm); flex-wrap: wrap; }
	h2 { font-family: var(--font-display); margin: 0 0 var(--space-sm); } p { line-height: 1.5; }
	.muted, .version p { color: var(--text-secondary); font-size: var(--text-sm); }
	.program-list { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 240px), 1fr)); gap: var(--space-sm); }
	.program-list a {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		padding: var(--space-md);
		color: var(--text-primary);
		background: linear-gradient(
			180deg,
			var(--bg-card) 0%,
			color-mix(in srgb, var(--bg-card) 94%, var(--bg-soft, var(--bg-base))) 100%
		);
		border: 1px solid color-mix(in srgb, var(--border-soft) 85%, transparent);
		border-radius: var(--radius-lg, 12px);
		text-decoration: none;
		min-width: 0;
		overflow-wrap: anywhere;
		box-shadow: 0 1px 3px rgba(0, 0, 0, 0.02);
		transition: border-color 0.15s ease, box-shadow 0.15s ease, transform 0.15s ease;
	}
	.program-list a:hover {
		border-color: color-mix(in srgb, var(--accent-primary) 40%, var(--border-soft));
		box-shadow: 0 4px 14px color-mix(in srgb, var(--accent-primary) 8%, rgba(0, 0, 0, 0.05));
		transform: translateY(-1px);
	}
	.program-list a[aria-current] {
		border-color: var(--accent-primary);
		background: linear-gradient(
			135deg,
			color-mix(in srgb, var(--accent-primary) 10%, var(--bg-card)) 0%,
			var(--bg-card) 100%
		);
		box-shadow: 0 2px 8px color-mix(in srgb, var(--accent-primary) 10%, transparent);
	}
	.kind { font-size: var(--text-xs); color: var(--text-secondary); }
	.program-detail {
		margin-top: var(--space-md);
		padding: var(--space-md);
		background: linear-gradient(
			145deg,
			color-mix(in srgb, var(--accent-primary) 5%, var(--bg-card)) 0%,
			color-mix(in srgb, var(--bg-soft, var(--bg-card)) 20%, var(--bg-card)) 50%,
			var(--bg-card) 100%
		);
		border: 1px solid color-mix(in srgb, var(--accent-primary) 22%, var(--border-soft));
		border-radius: var(--radius-lg, 12px);
		box-shadow: 0 2px 8px color-mix(in srgb, var(--accent-primary) 4%, rgba(0, 0, 0, 0.03));
		overflow-wrap: anywhere;
	}
	.program-detail h2 { margin-top: var(--space-md); } details { margin-top: var(--space-md); } summary { padding: var(--space-sm) 0; cursor: pointer; }
	.detail-title, .editor-actions { display: flex; align-items: center; justify-content: space-between; gap: var(--space-sm); flex-wrap: wrap; }
	.editor-actions { justify-content: flex-start; margin-top: var(--space-sm); }
	.editor-label { display: block; margin: var(--space-sm) 0; color: var(--text-secondary); font-size: var(--text-sm); }
	textarea { box-sizing: border-box; width: 100%; min-height: 24rem; padding: var(--space-sm); border: 1px solid var(--border-soft); border-radius: var(--radius-md); color: var(--text-primary); background: var(--bg-primary, var(--bg-card)); font-family: var(--font-mono, ui-monospace, monospace); font-size: var(--text-sm); line-height: 1.5; resize: vertical; }
	textarea:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 2px; }
	button.primary { border-color: var(--accent-primary); }
	.version { padding: var(--space-sm) 0; border-bottom: 1px solid var(--border-soft); } .version strong { overflow-wrap: anywhere; }
	.links { display: flex; flex-wrap: wrap; gap: var(--space-md); margin-top: var(--space-md); } .links a { color: var(--text-primary); }
	button { min-height: 32px; padding: 0.28rem 0.65rem; border: 1px solid var(--border-soft); border-radius: var(--radius-chip, 6px); color: var(--text-primary); background: var(--bg-card); font: inherit; font-size: 0.8rem; cursor: pointer; transition: all 0.15s ease; } button:disabled { opacity: .6; cursor: default; }
	a:focus-visible, button:focus-visible, summary:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 3px; }
</style>
