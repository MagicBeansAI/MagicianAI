<script lang="ts">
	import { createEventDispatcher, onDestroy, onMount } from 'svelte';
	import { createGoalFolder, createGoalNote, goalKey, goalNoteName, type GoalWorkspace } from './goals';
	import { listPrograms, type ProgramSummary } from './programs';

	export let mode: 'goal' | 'folder';
	export let workspaces: GoalWorkspace[];
	const dispatch = createEventDispatcher<{ created: void; cancel: void }>();
	const controller = new AbortController();
	let workspace = workspaces[0]?.name ?? '';
	let newFolder = '', title = '', parentPath = '', target = '', description = '';
	let busy = false, error = '';
	let program = '', programs: ProgramSummary[] = [], programsLoading = false, programsError = '';
	$: selected = workspaces.find(space => space.name === workspace);
	$: parent = selected?.goals.find(goal => goal.path === parentPath);
	onDestroy(() => controller.abort());
	onMount(() => { if (mode === 'goal') void loadPrograms(); });

	async function loadPrograms() {
		programsLoading = true; programsError = '';
		try {
			const result = await listPrograms(controller.signal);
			if (!controller.signal.aborted) {
				programs = result;
				if (!programs.some(item => item.name === program)) program = '';
			}
		} catch (cause) {
			if (!controller.signal.aborted) programsError = cause instanceof Error ? cause.message : String(cause);
		} finally { if (!controller.signal.aborted) programsLoading = false; }
	}

	async function submit() {
		if (busy) return;
		error = '';
		try {
			const folder = goalNoteName(mode === 'folder' ? newFolder : workspace || newFolder);
			if (mode === 'goal') {
				const name = goalNoteName(title);
				if (selected?.goals.some(goal => goalKey(goal.name) === goalKey(name))) {
					throw new Error('A goal with this name already exists in this folder.');
				}
			}
			busy = true;
			if (mode === 'folder') await createGoalFolder(newFolder, controller.signal);
			else await createGoalNote({ workspace: folder, title, parent, target, description, program }, controller.signal);
			if (!controller.signal.aborted) dispatch('created');
		} catch (cause) {
			if (!controller.signal.aborted) error = cause instanceof Error ? cause.message : String(cause);
		} finally { busy = false; }
	}
</script>

<form aria-label={mode === 'goal' ? 'Create goal' : 'Create goal folder'} on:submit|preventDefault={submit}>
	<h2>{mode === 'goal' ? 'New goal' : 'New folder'}</h2>
	{#if mode === 'folder'}
		<label>Folder name <input bind:value={newFolder} required maxlength="160" disabled={busy} /></label>
		<p class="muted">A folder groups your goals.</p>
	{:else}
		{#if workspaces.length}
			<label>Folder <select bind:value={workspace} on:change={() => parentPath = ''} disabled={busy}>
				{#each workspaces as space}<option value={space.name}>{space.name}</option>{/each}
				<option value="">New folder…</option>
			</select></label>
		{/if}
		{#if !workspace}<label>Folder name <input bind:value={newFolder} required maxlength="160" disabled={busy} /></label>{/if}
		<label>Goal name <input bind:value={title} required maxlength="160" disabled={busy} /></label>
		{#if selected?.goals.length}
			<label>Parent goal <select bind:value={parentPath} disabled={busy}>
				<option value="">None — main goal</option>
				{#each selected.goals as goal}<option value={goal.path}>{goal.title}</option>{/each}
			</select></label>
		{/if}
		<label>Target date (optional) <input type="date" bind:value={target} disabled={busy} /></label>
		<label>Description (optional) <textarea bind:value={description} rows="3" disabled={busy}></textarea></label>
		<label>Playbook (optional)
			<select bind:value={program} disabled={busy || programsLoading || !!programsError}>
				<option value="">No playbook</option>
				{#each programs as item (item.name)}<option value={item.name}>{item.title} ({item.name})</option>{/each}
			</select>
		</label>
		{#if programsLoading}<p class="muted" role="status">Loading playbooks…</p>
		{:else if programsError}<p class="muted">Could not load playbooks. You can still create this goal without one. <button type="button" on:click={loadPrograms} disabled={busy}>Retry playbooks</button></p>
		{:else if !programs.length}<p class="muted">No playbooks are available in this workspace yet.</p>{/if}
		<p class="muted">A playbook gives ongoing direction. Linking one does not start work or change crew schedules.</p>
		<p class="muted">Created as a private, proposed goal owned by you.</p>
	{/if}
	{#if error}<p role="alert">{error}</p>{/if}
	<div class="actions">
		<button type="submit" disabled={busy}>{busy ? 'Creating…' : mode === 'goal' ? 'Create goal' : 'Create folder'}</button>
		<button type="button" on:click={() => dispatch('cancel')} disabled={busy}>Cancel</button>
	</div>
</form>

<style>
	form { display: grid; gap: .85rem; max-width: 38rem; margin: 1rem 0; padding: 1rem; border: 1px solid var(--border-soft); border-radius: var(--radius-lg, 12px); background: var(--bg-card); }
	h2, p { margin: 0; }
	h2 { font-size: 1.1rem; }
	label { display: grid; gap: .35rem; font-size: .9rem; }
	input, select, textarea { width: 100%; min-width: 0; box-sizing: border-box; padding: .6rem; color: var(--text-primary); background: var(--bg-input, var(--bg-card)); border: 1px solid var(--border-soft); border-radius: 6px; font: inherit; }
	textarea { resize: vertical; }
	.muted { color: var(--text-secondary); font-size: .85rem; }
	.actions { display: flex; flex-wrap: wrap; gap: .5rem; }
	button { padding: .55rem .85rem; border: 1px solid var(--border-soft); border-radius: 6px; background: var(--bg-card); color: var(--text-primary); cursor: pointer; }
	button[type=submit] { background: var(--accent-primary); color: var(--text-on-accent, white); }
	button:disabled { opacity: .5; cursor: default; }
</style>
