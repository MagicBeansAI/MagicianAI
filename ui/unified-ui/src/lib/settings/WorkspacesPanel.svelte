<script lang="ts">
	import { onMount } from 'svelte';
	import {
		createWorkspace,
		deleteWorkspace,
		isValidWorkspaceSlug,
		listWorkspaces,
		suggestWorkspaceSlug,
		updateWorkspace,
		WorkspaceRequestError,
		type WorkspaceCard
	} from '$lib/stores/workspacesStore';
	import { showSuccess } from '$lib/shared/stores/notifications';

	interface Props {
		/** The workspace this session is bound to; it cannot delete itself. */
		currentWorkspace: string;
		/** Called after any change, so the page can refresh its switcher. */
		onChange?: () => void | Promise<void>;
	}

	let { currentWorkspace, onChange }: Props = $props();

	let workspaces = $state<WorkspaceCard[]>([]);
	let loading = $state(false);
	let listError = $state<string | null>(null);

	// Create
	let creating = $state(false);
	let newName = $state('');
	let newSlug = $state('');
	let slugEdited = $state(false);
	let newDescription = $state('');
	let createBusy = $state(false);
	let createError = $state<string | null>(null);

	// Rename — one row at a time
	let editingId = $state<string | null>(null);
	let editName = $state('');
	let editDescription = $state('');
	let editBusy = $state(false);
	let editError = $state<string | null>(null);

	// Delete — one row at a time. `confirm` asks once; `purge` is reached only
	// when the server says the workspace still holds data, and needs the id
	// typed out before anything is removed.
	let deletingId = $state<string | null>(null);
	let deleteStep = $state<'confirm' | 'purge'>('confirm');
	let typedConfirmation = $state('');
	let deleteBusy = $state(false);
	let deleteError = $state<string | null>(null);

	const slugToCreate = $derived(slugEdited ? newSlug : suggestWorkspaceSlug(newName));
	const slugValid = $derived(isValidWorkspaceSlug(slugToCreate));
	const slugTaken = $derived(workspaces.some((w) => w.id === slugToCreate));
	const canCreate = $derived(
		!createBusy && newName.trim().length > 0 && slugValid && !slugTaken
	);

	onMount(() => {
		void refresh();
	});

	function messageOf(error: unknown, fallback: string): string {
		return error instanceof Error ? error.message : fallback;
	}

	async function refresh(): Promise<void> {
		loading = true;
		listError = null;
		try {
			workspaces = await listWorkspaces();
		} catch (error) {
			listError = messageOf(error, 'Could not load workspaces.');
		} finally {
			loading = false;
		}
	}

	async function changed(): Promise<void> {
		await refresh();
		await onChange?.();
	}

	function startCreate(): void {
		creating = true;
		newName = '';
		newSlug = '';
		slugEdited = false;
		newDescription = '';
		createError = null;
	}

	function editSlug(value: string): void {
		slugEdited = true;
		newSlug = value;
	}

	async function submitCreate(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (!canCreate) return;
		createBusy = true;
		createError = null;
		const slug = slugToCreate;
		try {
			await createWorkspace({ slug, display_name: newName, description: newDescription });
			creating = false;
			showSuccess(`Created ${newName.trim()}`);
			await changed();
		} catch (error) {
			createError =
				error instanceof WorkspaceRequestError && error.code === 'workspace_pending_purge'
					? `“${slug}” belonged to a workspace deleted with its data. The name is free again after Magician restarts — or pick another.`
					: messageOf(error, 'Could not create the workspace.');
		} finally {
			createBusy = false;
		}
	}

	function startEdit(workspace: WorkspaceCard): void {
		cancelDelete();
		editingId = workspace.id;
		editName = workspace.display_name;
		editDescription = workspace.description ?? '';
		editError = null;
	}

	function cancelEdit(): void {
		editingId = null;
		editError = null;
	}

	async function submitEdit(event: SubmitEvent, workspace: WorkspaceCard): Promise<void> {
		event.preventDefault();
		if (!editName.trim()) {
			editError = 'A name is required.';
			return;
		}
		editBusy = true;
		editError = null;
		try {
			await updateWorkspace(workspace.id, {
				display_name: editName,
				description: editDescription.trim() ? editDescription : null
			});
			editingId = null;
			showSuccess(`Saved ${editName.trim()}`);
			await changed();
		} catch (error) {
			editError = messageOf(error, 'Could not save the workspace.');
		} finally {
			editBusy = false;
		}
	}

	function deleteBlockedReason(workspace: WorkspaceCard): string | null {
		if (workspace.is_default) return 'The default workspace cannot be deleted.';
		if (workspace.id === currentWorkspace) {
			return 'You are working in this workspace. Switch to another one to delete it.';
		}
		return null;
	}

	function startDelete(workspace: WorkspaceCard): void {
		cancelEdit();
		deletingId = workspace.id;
		deleteStep = 'confirm';
		typedConfirmation = '';
		deleteError = null;
	}

	function cancelDelete(): void {
		deletingId = null;
		deleteStep = 'confirm';
		typedConfirmation = '';
		deleteError = null;
	}

	async function confirmDelete(workspace: WorkspaceCard): Promise<void> {
		deleteBusy = true;
		deleteError = null;
		try {
			if (deleteStep === 'purge') {
				if (typedConfirmation.trim() !== workspace.id) return;
				await deleteWorkspace(workspace.id, { purge: true });
				showSuccess(
					`Deleted ${workspace.display_name}. Its files are removed the next time Magician starts.`
				);
			} else {
				await deleteWorkspace(workspace.id);
				showSuccess(`Deleted ${workspace.display_name}`);
			}
			cancelDelete();
			await changed();
		} catch (error) {
			if (
				deleteStep === 'confirm' &&
				error instanceof WorkspaceRequestError &&
				error.code === 'workspace_has_live_state'
			) {
				// Not an error to the person: the workspace simply has data, and
				// removing that deserves its own, explicit confirmation.
				deleteStep = 'purge';
				typedConfirmation = '';
			} else {
				deleteError = messageOf(error, 'Could not delete the workspace.');
			}
		} finally {
			deleteBusy = false;
		}
	}

	function counts(workspace: WorkspaceCard): string {
		const parts = [
			`${workspace.agent_count} agent${workspace.agent_count === 1 ? '' : 's'}`,
			`${workspace.active_task_count} active task${workspace.active_task_count === 1 ? '' : 's'}`
		];
		const last = relativeTime(workspace.last_activity);
		if (last) parts.push(`last active ${last}`);
		return parts.join(' · ');
	}

	function relativeTime(iso: string | null | undefined): string | null {
		if (!iso) return null;
		const then = Date.parse(iso);
		if (Number.isNaN(then)) return null;
		const minutes = Math.round((Date.now() - then) / 60_000);
		if (minutes < 1) return 'just now';
		if (minutes < 60) return `${minutes} min ago`;
		const hours = Math.round(minutes / 60);
		if (hours < 48) return `${hours} h ago`;
		return `${Math.round(hours / 24)} days ago`;
	}
</script>

<section class="card" aria-labelledby="workspaces-title">
	<div class="header">
		<div>
			<p class="overline">Workspaces</p>
			<h2 id="workspaces-title">Your workspaces</h2>
			<p>
				Each workspace keeps its own agents, tasks, memory and files. Deleting one that still holds
				data removes access at once and its files the next time Magician starts.
			</p>
		</div>
		<div class="actions">
			<button type="button" disabled={loading} onclick={() => void refresh()}>
				{loading ? 'Refreshing…' : 'Refresh'}
			</button>
			{#if !creating}
				<button class="primary" type="button" onclick={startCreate}>New workspace</button>
			{/if}
		</div>
	</div>

	{#if creating}
		<form class="create" onsubmit={submitCreate} aria-label="New workspace">
			<label>
				Name
				<input bind:value={newName} placeholder="Research" autocomplete="off" required />
			</label>
			<label>
				ID
				<input
					value={slugToCreate}
					oninput={(event) => editSlug((event.currentTarget as HTMLInputElement).value)}
					autocomplete="off"
					spellcheck="false"
					aria-describedby="workspace-slug-hint"
				/>
			</label>
			<p id="workspace-slug-hint" class="hint" class:invalid={!!slugToCreate && (!slugValid || slugTaken)}>
				{#if slugTaken}
					A workspace with this ID already exists.
				{:else if slugToCreate && !slugValid}
					Use 1–32 lowercase letters, digits, “-” or “_”.
				{:else}
					Used in paths and can’t be changed later.
				{/if}
			</p>
			<label class="wide">
				Description <span class="optional">(optional)</span>
				<input bind:value={newDescription} autocomplete="off" />
			</label>
			{#if createError}<p class="alert" role="alert">{createError}</p>{/if}
			<div class="actions">
				<button type="button" disabled={createBusy} onclick={() => (creating = false)}>Cancel</button>
				<button class="primary" type="submit" disabled={!canCreate}>
					{createBusy ? 'Creating…' : 'Create workspace'}
				</button>
			</div>
		</form>
	{/if}

	{#if listError}
		<p class="alert" role="alert">{listError}</p>
	{:else if loading && workspaces.length === 0}
		<p class="empty">Loading workspaces…</p>
	{/if}

	<ul class="list">
		{#each workspaces as workspace (workspace.id)}
			{@const blocked = deleteBlockedReason(workspace)}
			<li class="row" class:current={workspace.id === currentWorkspace}>
				{#if editingId === workspace.id}
					<form class="edit" onsubmit={(event) => submitEdit(event, workspace)} aria-label={`Edit ${workspace.display_name}`}>
						<label>
							Name
							<input bind:value={editName} autocomplete="off" required />
						</label>
						<label class="wide">
							Description <span class="optional">(optional)</span>
							<input bind:value={editDescription} autocomplete="off" />
						</label>
						{#if editError}<p class="alert" role="alert">{editError}</p>{/if}
						<div class="actions">
							<button type="button" disabled={editBusy} onclick={cancelEdit}>Cancel</button>
							<button class="primary" type="submit" disabled={editBusy}>
								{editBusy ? 'Saving…' : 'Save'}
							</button>
						</div>
					</form>
				{:else}
					<div class="summary">
						<div class="title">
							<strong>{workspace.display_name}</strong>
							{#if workspace.is_default}<span class="chip">Default</span>{/if}
							{#if workspace.id === currentWorkspace}<span class="chip accent">Current</span>{/if}
						</div>
						<code class="id">{workspace.id}</code>
						{#if workspace.description}<p class="description">{workspace.description}</p>{/if}
						<p class="meta">{counts(workspace)}</p>
					</div>
					<div class="actions">
						<button type="button" onclick={() => startEdit(workspace)} aria-label={`Rename ${workspace.display_name}`}>
							Rename
						</button>
						<button
							class="danger"
							type="button"
							disabled={!!blocked || deletingId === workspace.id}
							title={blocked ?? undefined}
							aria-label={`Delete ${workspace.display_name}`}
							onclick={() => startDelete(workspace)}
						>
							Delete
						</button>
					</div>
				{/if}

				{#if deletingId === workspace.id}
					<div class="confirm" role="group" aria-label={`Delete ${workspace.display_name}`}>
						{#if deleteStep === 'confirm'}
							<p>Delete <strong>{workspace.display_name}</strong>?</p>
						{:else}
							<p>
								<strong>{workspace.display_name}</strong> still holds data
								({counts(workspace)}). Deleting it with its data can’t be undone: access ends now,
								and its agents, tasks, memory and files are removed the next time Magician starts.
							</p>
							{#if workspace.active_task_count > 0}
								<p class="alert warn" role="alert">
									{workspace.active_task_count} task{workspace.active_task_count === 1 ? ' is' : 's are'} still
									active in this workspace.
								</p>
							{/if}
							<label>
								Type <code>{workspace.id}</code> to confirm
								<input
									bind:value={typedConfirmation}
									autocomplete="off"
									spellcheck="false"
									aria-label={`Type ${workspace.id} to confirm`}
								/>
							</label>
						{/if}
						{#if deleteError}<p class="alert" role="alert">{deleteError}</p>{/if}
						<div class="actions">
							<button type="button" disabled={deleteBusy} onclick={cancelDelete}>Cancel</button>
							<button
								class="danger solid"
								type="button"
								disabled={deleteBusy ||
									(deleteStep === 'purge' && typedConfirmation.trim() !== workspace.id)}
								onclick={() => void confirmDelete(workspace)}
							>
								{#if deleteBusy}
									Deleting…
								{:else if deleteStep === 'purge'}
									Delete with data
								{:else}
									Delete
								{/if}
							</button>
						</div>
					</div>
				{/if}
			</li>
		{/each}
	</ul>
</section>

<style>
	.card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		display: flex;
		flex-direction: column;
		gap: 1rem;
		padding: 1rem;
	}
	.header {
		display: flex;
		flex-wrap: wrap;
		gap: 1rem;
		justify-content: space-between;
	}
	.header > div:first-child {
		flex: 1 1 22rem;
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}
	.overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		margin: 0;
		text-transform: uppercase;
	}
	h2,
	p {
		margin: 0;
	}
	h2 {
		color: var(--text-primary);
		font-size: 1.05rem;
	}
	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		align-items: flex-start;
	}
	button {
		background: var(--bg-secondary);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		cursor: pointer;
		padding: 0.45rem 0.85rem;
	}
	button.primary {
		background: var(--accent);
		border-color: var(--accent);
		color: var(--text-on-accent, #fff);
	}
	button.danger {
		border-color: var(--danger, #c0392b);
		color: var(--danger, #c0392b);
	}
	button.danger.solid {
		background: var(--danger, #c0392b);
		color: #fff;
	}
	button:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}
	.list {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		list-style: none;
		margin: 0;
		padding: 0;
	}
	.row {
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem;
		justify-content: space-between;
		padding: 0.75rem;
	}
	.row.current {
		border-color: var(--accent);
	}
	.summary {
		display: flex;
		flex: 1 1 18rem;
		flex-direction: column;
		gap: 0.2rem;
		min-width: 0;
	}
	.title {
		align-items: center;
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
	}
	.id {
		color: var(--text-secondary);
		font-size: 0.8rem;
	}
	.description {
		color: var(--text-primary);
		font-size: 0.9rem;
	}
	.meta,
	.empty,
	.hint,
	.optional {
		color: var(--text-secondary);
		font-size: 0.8rem;
	}
	.hint.invalid {
		color: var(--danger, #c0392b);
	}
	.chip {
		border: 1px solid var(--border-soft);
		border-radius: 999px;
		font-size: 0.7rem;
		padding: 0.05rem 0.5rem;
	}
	.chip.accent {
		border-color: var(--accent);
		color: var(--accent);
	}
	.create,
	.edit {
		display: grid;
		gap: 0.6rem;
		grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr));
		width: 100%;
	}
	.create .wide,
	.edit .wide,
	.create .hint,
	.create .alert,
	.edit .alert,
	.create .actions,
	.edit .actions {
		grid-column: 1 / -1;
	}
	label {
		color: var(--text-secondary);
		display: flex;
		flex-direction: column;
		font-size: 0.8rem;
		gap: 0.25rem;
	}
	input {
		background: var(--bg-secondary);
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		font-size: 0.9rem;
		padding: 0.45rem 0.6rem;
	}
	.confirm {
		background: var(--bg-secondary);
		border-left: 3px solid var(--danger, #c0392b);
		border-radius: 4px;
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
		padding: 0.75rem;
		width: 100%;
	}
	.alert {
		background: var(--bg-secondary);
		border-left: 3px solid var(--danger, #c0392b);
		padding: 0.5rem 0.75rem;
	}
	.alert.warn {
		border-left-color: var(--warning, #d68910);
	}
	code {
		font-size: 0.85em;
	}
</style>
