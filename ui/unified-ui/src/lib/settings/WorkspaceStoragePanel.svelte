<script lang="ts">
	import { onMount } from 'svelte';

	import { requestConfirmation } from '$lib/stores/confirmationStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';

	type WorkspaceStorageProvider = 'local_file' | 'silverbullet_space';

	interface WorkspaceStorageEnvelope {
		settings_path: string;
		settings: {
			provider: string;
			silverbullet?: { space_path?: string | null };
		};
		resolved: {
			provider: string;
			local_root: string;
			silverbullet_space_path: string;
			active_runtime_root: string;
		};
		warnings: string[];
	}

	let envelope = $state<WorkspaceStorageEnvelope | null>(null);
	let provider = $state<WorkspaceStorageProvider>('local_file');
	let spacePath = $state('');
	let error = $state<string | null>(null);
	let loading = $state(false);
	let saving = $state(false);

	const savedProvider = $derived(normalizeProvider(envelope?.settings.provider));
	const savedSpacePath = $derived(envelope?.settings.silverbullet?.space_path ?? '');
	const dirty = $derived(
		envelope !== null && (provider !== savedProvider || spacePath.trim() !== savedSpacePath)
	);

	onMount(() => {
		void refresh(false);
	});

	function normalizeProvider(value: string | null | undefined): WorkspaceStorageProvider {
		return value === 'silverbullet' || value === 'silverbullet_space'
			? 'silverbullet_space'
			: 'local_file';
	}

	function applyEnvelope(next: WorkspaceStorageEnvelope): void {
		envelope = next;
		provider = normalizeProvider(next.settings.provider);
		spacePath = next.settings.silverbullet?.space_path ?? '';
	}

	async function readError(response: Response): Promise<string> {
		try {
			const payload = (await response.json()) as { message?: unknown; error?: unknown };
			if (typeof payload.message === 'string' && payload.message.trim()) return payload.message;
			if (typeof payload.error === 'string' && payload.error.trim()) return payload.error;
		} catch {
			// The status line is enough when the body is not JSON.
		}
		return `Request failed (${response.status})`;
	}

	async function refresh(showToast: boolean): Promise<void> {
		loading = true;
		error = null;
		try {
			const response = await fetch('/api/magician/v2/workspace-storage/settings');
			if (!response.ok) throw new Error(await readError(response));
			applyEnvelope((await response.json()) as WorkspaceStorageEnvelope);
			if (showToast) showSuccess('Workspace storage loaded');
		} catch (caught) {
			const message = caught instanceof Error ? caught.message : 'Failed to load workspace storage';
			error = message;
			if (showToast) showError(message);
		} finally {
			loading = false;
		}
	}

	async function save(): Promise<void> {
		if (!envelope || !dirty || saving) return;
		const nextProvider = provider;
		const nextSpacePath = spacePath.trim();
		if (nextProvider !== savedProvider) {
			const movingToSpace = nextProvider === 'silverbullet_space';
			const confirmed = await requestConfirmation({
				title: movingToSpace ? 'Store runtime state in the notes folder?' : 'Store runtime state locally?',
				message: movingToSpace
					? 'After Magician restarts, tasks, chat, memory, and artifacts are read and written under the notes folder. Existing files stay where they are and are not copied.'
					: 'After Magician restarts, tasks, chat, memory, and artifacts come from the local backend data root again. The notes folder is left in place.',
				confirmLabel: 'Save',
				cancelLabel: 'Cancel'
			});
			if (!confirmed) return;
		}
		if (saving) return;

		saving = true;
		error = null;
		try {
			const response = await fetch('/api/magician/v2/workspace-storage/settings', {
				method: 'PUT',
				headers: { 'content-type': 'application/json' },
				body: JSON.stringify({
					provider: nextProvider,
					silverbullet: {
						space_path: nextSpacePath || null
					}
				})
			});
			if (!response.ok) throw new Error(await readError(response));
			applyEnvelope((await response.json()) as WorkspaceStorageEnvelope);
			showSuccess('Workspace storage saved. Restart Magician to apply it.');
		} catch (caught) {
			const message = caught instanceof Error ? caught.message : 'Failed to save workspace storage';
			error = message;
			showError(message);
		} finally {
			saving = false;
		}
	}
</script>

<section class="card" aria-labelledby="workspace-storage-title">
	<div class="header">
		<div>
			<p class="overline">Canonical runtime</p>
			<h2 id="workspace-storage-title">Workspace storage</h2>
			<p>
				Choose where this workspace's tasks, chat, execution history, memory, and artifacts live.
				Notes stay on the notes provider. A saved change applies after Magician restarts, and existing data is not moved.
			</p>
		</div>
		<button type="button" disabled={loading || saving} onclick={() => void refresh(true)}>
			{loading ? 'Refreshing…' : 'Refresh'}
		</button>
	</div>

	{#if error}
		<p class="alert error" role="alert">{error}</p>
	{/if}

	<fieldset class="field" disabled={loading || saving || !envelope}>
		<legend>Runtime provider</legend>
		<label>
			<input
				type="radio"
				name="workspace-storage-provider"
				value="local_file"
				checked={provider === 'local_file'}
				onchange={() => (provider = 'local_file')}
			/>
			Local backend data
		</label>
		<label>
			<input
				type="radio"
				name="workspace-storage-provider"
				value="silverbullet_space"
				checked={provider === 'silverbullet_space'}
				onchange={() => (provider = 'silverbullet_space')}
			/>
			Notes folder
		</label>
		<small>Local keeps canonical files under the backend data root.</small>
	</fieldset>

	<div class="field">
		<label for="workspace-storage-space">Notes folder path</label>
		<input
			id="workspace-storage-space"
			bind:value={spacePath}
			disabled={loading || saving || !envelope}
			placeholder="Blank uses the runtime root"
			spellcheck="false"
		/>
		<small>Used when runtime state is stored in the notes folder. Leave blank to use the runtime root.</small>
	</div>

	{#if envelope}
		<dl>
			<div>
				<dt>Settings file</dt>
				<dd>{envelope.settings_path}</dd>
			</div>
			<div>
				<dt>Active root</dt>
				<dd>{envelope.resolved.active_runtime_root}</dd>
			</div>
			<div>
				<dt>Local root</dt>
				<dd>{envelope.resolved.local_root}</dd>
			</div>
			<div>
				<dt>Notes folder</dt>
				<dd>{envelope.resolved.silverbullet_space_path}</dd>
			</div>
		</dl>
		{#each envelope.warnings ?? [] as warning (warning)}
			<p class="alert warning" role="status">{warning}</p>
		{/each}
	{:else if loading}
		<p class="empty">Loading workspace storage…</p>
	{/if}

	<div class="actions">
		<button class="primary" type="button" disabled={!dirty || loading || saving} onclick={() => void save()}>
			{saving ? 'Saving…' : 'Save workspace storage'}
		</button>
	</div>
</section>

<style>
	.card {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		color: var(--text-primary);
		display: grid;
		gap: 0.9rem;
		padding: 1rem;
	}

	.header {
		align-items: flex-start;
		display: flex;
		gap: 1rem;
		justify-content: space-between;
	}

	.overline {
		color: var(--text-secondary);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0;
		margin: 0;
		text-transform: uppercase;
	}

	h2,
	p,
	small {
		letter-spacing: 0;
		margin: 0;
	}

	h2 {
		font-size: 1.05rem;
		font-weight: 600;
		line-height: 1.25;
		margin: 0.2rem 0 0.35rem;
	}

	p,
	small,
	dd {
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.45;
	}

	small {
		font-size: var(--text-2xs, 0.72rem);
	}

	.field {
		display: grid;
		gap: 0.35rem;
	}

	fieldset {
		border: 0;
		margin: 0;
		padding: 0;
	}

	legend,
	.field > label {
		font-size: 0.78rem;
		font-weight: 650;
	}

	fieldset label {
		align-items: center;
		display: flex;
		font-size: 0.92rem;
		font-weight: 500;
		gap: 0.45rem;
	}

	input,
	button {
		font: inherit;
	}

	input:not([type='radio']) {
		background: var(--input-bg, var(--bg-soft));
		border: 1px solid var(--input-border, var(--border-soft));
		border-radius: 6px;
		color: var(--text-primary);
		font-size: 0.92rem;
		min-height: 2.25rem;
		padding: 0.4rem 0.65rem;
	}

	dl {
		display: grid;
		gap: 0.55rem;
		margin: 0;
	}

	dt {
		color: var(--text-secondary);
		font-size: 0.72rem;
		font-weight: 700;
	}

	dd {
		margin: 0.1rem 0 0;
		overflow-wrap: anywhere;
	}

	.alert {
		border-radius: 6px;
		font-size: 0.86rem;
		line-height: 1.45;
		margin: 0;
		padding: 0.65rem 0.75rem;
	}

	.alert.error {
		background: color-mix(in srgb, var(--danger, #b42318) 12%, transparent);
		color: var(--text-primary);
	}

	.alert.warning {
		background: color-mix(in srgb, var(--warning, #f59e0b) 14%, transparent);
		color: var(--text-primary);
	}

	.actions {
		display: flex;
		justify-content: flex-end;
	}

	button {
		background: transparent;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		cursor: pointer;
		min-height: 2.25rem;
		padding: 0.35rem 0.8rem;
	}

	button:disabled {
		cursor: default;
		opacity: 0.55;
	}

	.primary {
		background: var(--accent-primary, var(--accent));
		border-color: transparent;
		color: var(--text-on-accent, white);
	}

	.empty {
		margin: 0;
	}
</style>
