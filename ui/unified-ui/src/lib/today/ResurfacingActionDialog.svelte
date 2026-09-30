<script lang="ts">
	import { createEventDispatcher } from 'svelte';

	import Modal from '$lib/magician/components/generative/Modal.svelte';
	import type { ResurfacingCard, ResurfacingDetail } from './resurfacingQueries';
	import {
		buildResurfacingActionInput,
		defaultResurfacingActionDraft,
		type ResurfacingActionDraft,
		type ResurfacingDialogActionKind
	} from './resurfacingPresentation';

	export let open = false;
	export let kind: ResurfacingDialogActionKind | null = null;
	export let card: ResurfacingCard | null = null;
	export let detail: ResurfacingDetail | null = null;
	export let busy = false;
	export let serverError: string | null = null;
	export let uiThreadId: string | null = null;
	/**
	 * Lane-neutral overrides so a surface without a `ResurfacingCard` (message
	 * follow-ups) can reuse this dialog. The submit event is already generic —
	 * `{ kind, input }` — so only the heading and the draft-reset key needed
	 * decoupling.
	 */
	export let targetKey: string | null = null;
	export let sourceLabel: string | null = null;
	/** Seeds the note, mirroring how the card path seeds it from the summary. */
	export let sourceNote: string | null = null;

	$: resolvedTargetKey = targetKey ?? card?.candidate_id ?? null;
	$: resolvedSourceLabel = sourceLabel ?? card?.source_title ?? card?.line ?? '';

	const dispatch = createEventDispatcher<{
		cancel: void;
		submit: { kind: ResurfacingDialogActionKind; input: Record<string, unknown> };
	}>();

	let draft: ResurfacingActionDraft | null = null;
	let validationError: string | null = null;
	let draftKey = '';

	$: nextDraftKey = open && kind && resolvedTargetKey ? `${resolvedTargetKey}:${kind}` : '';
	// Without a card there is no source body to seed from, so the draft carries
	// only what the caller supplied. Same shape either way, so everything
	// downstream (validation, input building, submit) is unchanged.
	function genericDraft(label: string, note: string): ResurfacingActionDraft {
		const when = new Date();
		when.setDate(when.getDate() + 1);
		when.setHours(9, 0, 0, 0);
		const pad = (value: number) => String(value).padStart(2, '0');
		return {
			title: label.slice(0, 200),
			instruction: note.slice(0, 4_000),
			atLocal: `${when.getFullYear()}-${pad(when.getMonth() + 1)}-${pad(when.getDate())}T${pad(when.getHours())}:${pad(when.getMinutes())}`,
			timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
			recipient: '',
			channel: 'email',
			fact: note.slice(0, 1_200)
		};
	}

	$: if (nextDraftKey && nextDraftKey !== draftKey) {
		draftKey = nextDraftKey;
		draft = card
			? defaultResurfacingActionDraft(card, detail)
			: genericDraft(resolvedSourceLabel, sourceNote ?? resolvedSourceLabel);
		validationError = null;
	}
	$: if (!open && draftKey) {
		draftKey = '';
		validationError = null;
	}

	$: title = dialogTitle(kind);
	$: confirmLabel = dialogConfirmLabel(kind);

	function dialogTitle(action: ResurfacingDialogActionKind | null): string {
		switch (action) {
			case 'create_task': return 'Create task';
			case 'create_reminder': return 'Create in Apple Reminders';
			case 'share': return 'Prepare share';
			case 'save_to_memory': return 'Save to memory';
			default: return 'Worth a look action';
		}
	}

	function dialogConfirmLabel(action: ResurfacingDialogActionKind | null): string {
		switch (action) {
			case 'create_task': return 'Create task';
			case 'create_reminder': return 'Create in Apple Reminders';
			case 'share': return 'Create draft';
			case 'save_to_memory': return 'Submit for review';
			default: return 'Continue';
		}
	}

	function cancel(): void {
		if (busy) return;
		dispatch('cancel');
	}

	function submit(): void {
		if (!kind || !draft || busy) return;
		const result = buildResurfacingActionInput(kind, draft, uiThreadId);
		if (!result.ok) {
			validationError = result.error;
			return;
		}
		validationError = null;
		dispatch('submit', { kind, input: result.input });
	}
</script>

<Modal
	{open}
	{title}
	size="md"
	closable={!busy}
	idBase="resurfacing-action-dialog"
	initialFocusSelector=".resurfacing-dialog__initial"
	on:close={cancel}
>
	{#if kind && resolvedTargetKey && draft}
		<form class="resurfacing-dialog" on:submit|preventDefault={submit} novalidate>
			<p class="resurfacing-dialog__source">{resolvedSourceLabel}</p>

			{#if kind === 'create_task'}
				<label>
					<span>Title</span>
					<input class="resurfacing-dialog__initial" bind:value={draft.title} maxlength="200" />
				</label>
				<label>
					<span>Instruction</span>
					<textarea bind:value={draft.instruction} rows="6" maxlength="4000"></textarea>
				</label>
			{:else if kind === 'create_reminder'}
				<p class="resurfacing-dialog__source">This creates a native reminder and opens Apple Reminders on this Mac.</p>
				<label>
					<span>Title</span>
					<input class="resurfacing-dialog__initial" bind:value={draft.title} maxlength="200" />
				</label>
				<label>
					<span>Reminder note</span>
					<textarea bind:value={draft.instruction} rows="4" maxlength="4000"></textarea>
				</label>
				<div class="resurfacing-dialog__grid">
					<label>
						<span>Date and time</span>
						<input type="datetime-local" bind:value={draft.atLocal} />
					</label>
					<label>
						<span>Browser timezone</span>
						<input value={draft.timezone} readonly aria-readonly="true" />
					</label>
				</div>
			{:else if kind === 'share'}
				<div class="resurfacing-dialog__grid">
					<label>
						<span>Recipient</span>
						<input
							class="resurfacing-dialog__initial"
							bind:value={draft.recipient}
							maxlength="320"
							autocomplete="off"
						/>
					</label>
					<label>
						<span>Channel</span>
						<select bind:value={draft.channel}>
							<option value="email">Email</option>
							<option value="whatsapp">WhatsApp</option>
							<option value="telegram">Telegram</option>
							<option value="imessage">iMessage</option>
						</select>
					</label>
				</div>
				<label>
					<span>Draft instruction</span>
					<textarea bind:value={draft.instruction} rows="5" maxlength="4000"></textarea>
				</label>
			{:else if kind === 'save_to_memory'}
				<label>
					<span>Title</span>
					<input class="resurfacing-dialog__initial" bind:value={draft.title} maxlength="200" />
				</label>
				<label>
					<span>Fact submitted for review</span>
					<textarea bind:value={draft.fact} rows="6" maxlength="1200"></textarea>
				</label>
			{/if}

			{#if validationError || serverError}
				<p class="resurfacing-dialog__error" role="alert">{validationError || serverError}</p>
			{/if}

			<div class="resurfacing-dialog__actions">
				<button type="button" class="resurfacing-dialog__button" disabled={busy} on:click={cancel}>
					Cancel
				</button>
				<button
					type="submit"
					class="resurfacing-dialog__button resurfacing-dialog__button--primary"
					disabled={busy}
				>
					{busy ? 'Working…' : confirmLabel}
				</button>
			</div>
		</form>
	{/if}
</Modal>

<style>
	.resurfacing-dialog {
		display: grid;
		gap: 0.9rem;
		color: var(--text-primary);
	}

	.resurfacing-dialog__source {
		margin: 0;
		color: var(--text-muted);
		font-size: var(--text-xs);
		line-height: 1.4;
		overflow-wrap: anywhere;
	}

	.resurfacing-dialog label {
		display: grid;
		gap: 0.35rem;
		min-width: 0;
	}

	.resurfacing-dialog label > span {
		color: var(--text-secondary);
		font-size: var(--text-xs);
		font-weight: 650;
	}

	.resurfacing-dialog input,
	.resurfacing-dialog textarea,
	.resurfacing-dialog select {
		width: 100%;
		min-width: 0;
		box-sizing: border-box;
		border: 1px solid var(--border-default, var(--border-soft));
		border-radius: var(--radius-sm);
		background: var(--bg-card);
		color: var(--text-primary);
		font: inherit;
		font-size: var(--text-sm);
		line-height: 1.4;
		padding: 0.55rem 0.65rem;
	}

	.resurfacing-dialog textarea {
		resize: vertical;
	}

	.resurfacing-dialog input:focus,
	.resurfacing-dialog textarea:focus,
	.resurfacing-dialog select:focus {
		outline: 2px solid color-mix(in srgb, var(--accent-primary) 42%, transparent);
		outline-offset: 1px;
		border-color: var(--accent-primary);
	}

	.resurfacing-dialog__grid {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 0.75rem;
	}

	.resurfacing-dialog__error {
		margin: 0;
		padding: 0.55rem 0.65rem;
		border-left: 3px solid var(--color-error);
		background: color-mix(in srgb, var(--color-error) 8%, transparent);
		color: var(--color-error);
		font-size: var(--text-xs);
		line-height: 1.4;
	}

	.resurfacing-dialog__actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		flex-wrap: wrap;
	}

	.resurfacing-dialog__button {
		min-height: 2.2rem;
		padding: 0.45rem 0.8rem;
		border: 1px solid var(--border-default, var(--border-soft));
		border-radius: var(--radius-sm);
		background: var(--bg-card);
		color: var(--text-secondary);
		font: inherit;
		font-size: var(--text-sm);
		line-height: 1;
		white-space: nowrap;
		cursor: pointer;
	}

	.resurfacing-dialog__button--primary {
		border-color: transparent;
		background: var(--accent-primary);
		color: var(--accent-on-primary, #fff);
	}

	.resurfacing-dialog__button:disabled {
		opacity: 0.55;
		cursor: default;
	}

	@media (max-width: 640px) {
		.resurfacing-dialog__grid {
			grid-template-columns: 1fr;
		}
	}
</style>
