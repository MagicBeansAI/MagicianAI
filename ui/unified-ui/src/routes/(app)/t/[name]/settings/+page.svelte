<script lang="ts">
	import Button from '$lib/magician/components/generative/Button.svelte';
	import Input from '$lib/magician/components/generative/Input.svelte';
	import TextArea from '$lib/magician/components/generative/TextArea.svelte';
	import { chatStore } from '$lib/stores/chatStore';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { threadStore } from '$lib/stores/threadStore';
	import { showError, showSuccess } from '$lib/shared/stores/notifications';
	import { getThreadPageContext } from '$lib/threads/threadPageContext';

	// Settings tab of the thread workspace: per-thread name + memory + archive
	// and a Fresh Run. Thread identity/load comes from the shell
	// (../+layout.svelte) via context.
	const threadPage = getThreadPageContext();
	$: threadName = $threadPage.threadName;
	$: threadDetail = $threadPage.threadDetail;

	let threadTitleDraft = '';
	let threadMemoryDraft = '';
	let threadMetaSaving = false;
	let appliedThreadDetailSignature: string | null = null;

	$: threadSignature = threadDetail
		? `${threadDetail.id}:${threadDetail.updated_at}:${threadDetail.memory_updated_at ?? 'none'}`
		: null;

	// Seed the editable drafts whenever the active thread (or its persisted
	// memory) changes.
	$: if (threadSignature !== appliedThreadDetailSignature) {
		appliedThreadDetailSignature = threadSignature;
		threadTitleDraft = threadDetail?.name || threadName;
		threadMemoryDraft = threadDetail?.memory_text || '';
	}

	$: currentScopeKey = `${$scopeIdentityStore.principal}:${$scopeIdentityStore.workspace}`;

	function captureToken(): string {
		return `${currentScopeKey}:${threadName}`;
	}

	function isStale(token: string): boolean {
		return token !== `${currentScopeKey}:${threadName}`;
	}

	async function saveThreadMeta(): Promise<void> {
		if (threadMetaSaving) return;
		const token = captureToken();
		threadMetaSaving = true;
		try {
			const updated = await threadStore.updateThread(threadName, {
				name: threadTitleDraft.trim() || threadName,
				memory_text: threadMemoryDraft
			});
			if (isStale(token)) return;
			if (!updated) {
				throw new Error('Thread could not be saved.');
			}
			showSuccess(`Saved #${updated.id}.`);
		} catch (error) {
			if (isStale(token)) return;
			showError(error instanceof Error ? error.message : 'Failed to save thread details');
		} finally {
			if (!isStale(token)) {
				threadMetaSaving = false;
			}
		}
	}

	async function toggleArchiveThread(): Promise<void> {
		const token = captureToken();
		try {
			const updated = await threadStore.updateThread(threadName, {
				archived: !(threadDetail?.archived || false)
			});
			if (isStale(token)) return;
			if (!updated) {
				throw new Error('Thread could not be updated.');
			}
			showSuccess(updated.archived ? `Archived #${updated.id}.` : `Restored #${updated.id}.`);
		} catch (error) {
			if (isStale(token)) return;
			showError(error instanceof Error ? error.message : 'Failed to update thread');
		}
	}
</script>

<section class="thread-settings">
	<div class="thread-section__header">
		<div>
			<h2>Thread Context</h2>
			<p>Persisted metadata and memory scoped to #{threadName}.</p>
		</div>
		<div class="thread-settings__actions">
			{#if threadName !== 'general'}
				<Button
					label={threadDetail?.archived ? 'Restore Thread' : 'Archive Thread'}
					variant="outline"
					size="sm"
					on:click={() => void toggleArchiveThread()}
				/>
			{/if}
			<Button
				label={threadMetaSaving ? 'Saving...' : 'Save'}
				size="sm"
				disabled={threadMetaSaving}
				on:click={() => void saveThreadMeta()}
			/>
		</div>
	</div>

	<div class="thread-meta-grid">
		<div class="thread-meta-field">
			<Input
				label="Name"
				value={threadTitleDraft}
				placeholder="Thread name"
				on:change={(event) => (threadTitleDraft = event.detail.value)}
			/>
		</div>

		<div class="thread-meta-field thread-meta-field--full">
			<TextArea
				label="Thread memory"
				value={threadMemoryDraft}
				rows={6}
				placeholder="Persisted context, preferences, and decisions for this thread"
				on:change={(event) => (threadMemoryDraft = event.detail.value)}
			/>
		</div>
	</div>

	<Button
		label="Fresh Run"
		variant="outline"
		size="sm"
		on:click={async () => {
			await chatStore.newExecution(threadName);
		}}
	/>
</section>

<style>
	.thread-settings {
		flex: 1;
		min-height: 0;
		width: 100%;
		max-width: 1320px;
		margin: 0 auto;
		overflow-y: auto;
		padding: 0.75rem 0.75rem 0.25rem;
		border-radius: var(--radius-md, 18px);
		background: var(--bg-surface, #fff8f2);
		border: 1px solid var(--border-soft, #eee4dc);
		box-shadow: var(--shadow-sm, 0 6px 18px rgba(45, 52, 54, 0.06));
	}

	.thread-section__header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
	}

	.thread-section__header h2 {
		margin: 0;
		color: var(--text-primary, #2d3436);
	}

	.thread-section__header p {
		margin: 0.3rem 0 0;
		color: var(--text-secondary, #5f6668);
	}

	.thread-settings__actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem;
	}

	.thread-meta-grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 0.8rem;
		margin-top: 1rem;
		margin-bottom: 1rem;
	}

	.thread-meta-field {
		display: grid;
		gap: 0.45rem;
		font-size: 0.84rem;
		color: var(--text-secondary, #5f6668);
	}

	.thread-meta-field--full {
		grid-column: 1 / -1;
	}

	.thread-meta-field :global(.muij-input-field),
	.thread-meta-field :global(.muij-textarea-input) {
		width: 100%;
		border-radius: var(--radius-sm, 12px);
		border: 1px solid var(--border-soft, #eee4dc);
		background: var(--bg-surface, #fff8f2);
		padding: 0.8rem 0.95rem;
		font: inherit;
		color: var(--text-primary, #2d3436);
	}

	.thread-meta-field :global(.muij-input-label),
	.thread-meta-field :global(.muij-textarea-label) {
		font-size: 0.84rem;
		color: var(--text-secondary, #5f6668);
	}

	@media (max-width: 900px) {
		.thread-section__header {
			flex-direction: column;
			align-items: flex-start;
		}

		.thread-meta-grid {
			grid-template-columns: 1fr;
		}
	}
</style>
