<script lang="ts">
	import { onMount } from 'svelte';

	import { showError, showSuccess } from '$lib/shared/stores/notifications';

	/**
	 * Where this machine keeps tasks, chat, execution history, memory and
	 * artifacts. There is one backend (`local_file`); the panel reports where
	 * it resolved and any warning. Notes are separate: they live in the notes
	 * folder whatever this says.
	 */
	interface WorkspaceStorageEnvelope {
		settings_path: string;
		settings: { provider: string };
		resolved: {
			provider: string;
			local_root: string;
			active_runtime_root: string;
		};
		warnings: string[];
	}

	let envelope = $state<WorkspaceStorageEnvelope | null>(null);
	let error = $state<string | null>(null);
	let loading = $state(false);

	onMount(() => {
		void refresh(false);
	});

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
			envelope = (await response.json()) as WorkspaceStorageEnvelope;
			if (showToast) showSuccess('Workspace storage loaded');
		} catch (caught) {
			const message = caught instanceof Error ? caught.message : 'Failed to load workspace storage';
			error = message;
			if (showToast) showError(message);
		} finally {
			loading = false;
		}
	}
</script>

<section class="card" aria-labelledby="workspace-storage-title">
	<div class="header">
		<div>
			<p class="overline">Canonical runtime</p>
			<h2 id="workspace-storage-title">Workspace storage</h2>
			<p>
				Where this workspace's tasks, chat, execution history, memory, and artifacts live on this
				machine. Notes are kept separately, in the notes folder.
			</p>
		</div>
		<button type="button" disabled={loading} onclick={() => void refresh(true)}>
			{loading ? 'Refreshing…' : 'Refresh'}
		</button>
	</div>

	{#if error}
		<p class="alert error" role="alert">{error}</p>
	{/if}

	{#if envelope}
		<dl>
			<div>
				<dt>Settings file</dt>
				<dd>{envelope.settings_path}</dd>
			</div>
			<div>
				<dt>Runtime root</dt>
				<dd>{envelope.resolved.active_runtime_root}</dd>
			</div>
		</dl>
		{#each envelope.warnings ?? [] as warning (warning)}
			<p class="alert warning" role="status">{warning}</p>
		{/each}
	{:else if loading}
		<p class="empty">Loading workspace storage…</p>
	{/if}
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
	p {
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
	dd {
		color: var(--text-secondary);
		font-size: 0.92rem;
		line-height: 1.45;
	}

	button {
		font: inherit;
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

	.empty {
		margin: 0;
	}
</style>
