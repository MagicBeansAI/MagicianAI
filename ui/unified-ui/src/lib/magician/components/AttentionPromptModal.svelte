<script lang="ts">
	/**
	 * Themed replacement for `window.prompt()` in HITL flows.
	 *
	 * Mounts once at the app root (in `(app)/+layout.svelte`) and listens
	 * to `attentionPromptStore`. When a request lands, renders a GAUI
	 * `Modal` around `HitlPromptFields` and resolves the awaiting promise on
	 * submit / cancel.
	 *
	 * **The fields are no longer this component's.** They were, and that was fine
	 * while this was the only surface that answered an ask; the unified task panel
	 * answers them in place now, and a second copy of eleven renderers is a second
	 * place for a validation floor to drift and a new input type to be forgotten.
	 * What is left here is the chrome only a dialog has: the title, the eyebrow,
	 * the overlay registration, the keyboard chord, and the Cancel that means
	 * *dismissed without answering* rather than *denied*.
	 *
	 * See `docs/archive/plans/2026-05-10-execution-panel-canvas-redesign.md` and
	 * `docs/components/unified-ui/unified-task-panel.md`.
	 */
	import { onDestroy } from 'svelte';
	import {
		retrievalStatusLine,
		subscribeRetrievalStatus,
		type RetrievalStatus
	} from '$lib/hitl/retrievalStatus';
	import Modal from '$lib/magician/components/generative/Modal.svelte';
	import Button from '$lib/magician/components/generative/Button.svelte';
	import HitlPromptFields, { PROMPT_FOCUS_SELECTOR } from '$lib/hitl/HitlPromptFields.svelte';
	import {
		attentionPromptStore,
		resolveAttentionPrompt,
		PROMPT_HAS_OWN_ACTIONS,
		type AttentionPromptResult
	} from '$lib/stores/attentionPromptStore';
	import {
		OVERLAY_IDS,
		OVERLAY_PRIORITIES,
		release,
		requestFocus
	} from '$lib/shell/overlayCoordinator';

	let overlayRegistered = false;
	/** The fields component, so the footer's Submit can answer what it renders. */
	let fields: HitlPromptFields | null = null;
	let canSubmit = false;

	$: request = $attentionPromptStore.request;

	/**
	 * Secure HITL P6: while an `otp` ask is open, say what automatic
	 * retrieval is doing for it — a value-free line driven by the resolver's
	 * own status events, so the owner knows whether to wait a moment or type
	 * the code.
	 */
	let retrieval: RetrievalStatus | null = null;
	let unsubscribeRetrieval: (() => void) | null = null;
	let retrievalFor: string | null = null;
	$: {
		const correlationId = request?.sensitive?.kind === 'otp' ? request.sensitive.correlationId ?? null : null;
		if (correlationId !== retrievalFor) {
			retrievalFor = correlationId;
			retrieval = null;
			if (unsubscribeRetrieval) {
				unsubscribeRetrieval();
				unsubscribeRetrieval = null;
			}
			if (correlationId) {
				unsubscribeRetrieval = subscribeRetrievalStatus(correlationId, (next) => {
					if (retrievalFor === correlationId) retrieval = next;
				});
			}
		}
	}
	$: retrievalLine = retrieval ? retrievalStatusLine(retrieval) : null;
	$: if (request && !overlayRegistered) {
		overlayRegistered = true;
		requestFocus({
			id: OVERLAY_IDS.attentionPrompt,
			priority: OVERLAY_PRIORITIES.attentionInput,
			onClose: cancel
		});
	}
	$: if (!request && overlayRegistered) {
		overlayRegistered = false;
		release(OVERLAY_IDS.attentionPrompt);
	}
	// Nothing is answerable while no request is mounted; the fields component
	// stops updating `canSubmit` the moment it is destroyed and would otherwise
	// leave the last request's answer behind it.
	$: if (!request) canSubmit = false;

	/**
	 * Reactive rather than a function called from the template: an expression
	 * that reads no component variable is not re-evaluated when one changes, so a
	 * selector computed that way is whatever the first request needed and then
	 * never moves. The two shapes that render no field at all depend on this
	 * landing correctly, and one of them is the refusal on an authorization.
	 */
	$: initialFocusSelector = request ? PROMPT_FOCUS_SELECTOR[request.kind] : '';

	function answer(event: CustomEvent<AttentionPromptResult>): void {
		resolveAttentionPrompt(event.detail);
	}

	function cancel(): void {
		resolveAttentionPrompt(null);
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (!request) return;
		const eventTarget = event.target;
		if (
			!(eventTarget instanceof Element) ||
			!eventTarget.closest('[data-modal-id="attention-prompt"]')
		) {
			return;
		}
		const isTextarea = request.kind === 'multiline' || request.kind === 'guidance';
		const isEnter = event.key === 'Enter';
		const submitChord = isTextarea
			? isEnter && (event.metaKey || event.ctrlKey)
			: isEnter && !event.shiftKey;
		// `canSubmit` is the single source of truth for "is this request
		// answerable right now" — the multi_choice "require ≥1 selection
		// regardless of minSelections" floor, and the reason Enter cannot grant a
		// capability: `HitlPromptFields` holds it permanently `false` for every
		// shape that carries its own named controls. The Submit-button click path
		// reads the same value, so neither can drift, and there is no second
		// keyboard-specific guard to fall out of step with this one.
		if (submitChord && canSubmit) {
			event.preventDefault();
			fields?.submit();
		}
	}

	onDestroy(() => {
		release(OVERLAY_IDS.attentionPrompt);
		if (unsubscribeRetrieval) unsubscribeRetrieval();
		if (request) cancel();
	});
</script>

<svelte:window on:keydown={handleKeydown} />

<Modal
	open={!!request}
	title={request?.title ?? ''}
	size={request?.kind === 'diff_approval' ? 'xl' : 'md'}
	layer="attention-child"
	idBase="attention-prompt"
	{initialFocusSelector}
	returnFocusSelector="[data-attention-center-dialog], [data-attention-trigger]"
	on:close={cancel}
>
	{#if request}
		<div class="attention-modal-body">
			<div class="attention-modal__eyebrow">
				HITL · {request.kind}
				{#if request.chainPosition && request.chainTotal && request.chainTotal > 1}
					<span class="attention-modal__chain">
						· STEP {request.chainPosition} OF {request.chainTotal}
					</span>
				{/if}
			</div>
			{#if request.body}
				<p class="attention-modal__body-text">{request.body}</p>
			{/if}
			{#if retrievalLine}
				<p class="attention-modal__retrieval" role="status" data-retrieval-status={retrieval?.status}>
					{retrievalLine}
				</p>
			{/if}

			<div class="attention-modal__field">
				<HitlPromptFields
					bind:this={fields}
					bind:canSubmit
					{request}
					on:answer={answer}
					on:dismiss={cancel}
				/>
			</div>

			<!--
				**Cancel always renders; Submit only when the shape has no controls of
				its own.** They are different acts: Cancel dismisses the dialog and
				leaves the pause live, while a confirmation's `No` and an
				authorization's `Deny` are *answers* that resolve it. A generic Submit
				beside a pair of named decisions would be a third control for one
				answer, and the reader would have to guess which of them ends the
				prompt.
			-->
			<div class="attention-modal__footer">
				<Button
					label={request.cancelLabel ?? 'Cancel'}
					variant="outline"
					size="sm"
					on:click={cancel}
				/>
				{#if !PROMPT_HAS_OWN_ACTIONS[request.kind]}
					<Button
						label={request.confirmLabel ?? 'Submit'}
						variant="primary"
						size="sm"
						disabled={!canSubmit}
						on:click={() => fields?.submit()}
					/>
				{/if}
			</div>
		</div>
	{/if}
</Modal>

<style>
	.attention-modal-body {
		display: flex;
		flex-direction: column;
		gap: 0.85rem;
	}

	.attention-modal__retrieval {
		color: var(--text-secondary);
		font-size: 0.82rem;
		margin: 0;
	}
	.attention-modal__retrieval[data-retrieval-status='code_used'] {
		color: var(--color-success, var(--text-primary));
	}

	.attention-modal__eyebrow {
		font-family: var(--font-display, var(--font-primary));
		font-size: 0.62rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.18em;
		color: var(--accent-primary);
	}

	/* Multi-stage MVP — chain indicator (`STEP X OF N`) sits inline
	   with the eyebrow but slightly muted so it reads as a secondary
	   tag, not the primary kind label. */
	.attention-modal__chain {
		color: var(--text-secondary);
		font-weight: 600;
		margin-left: 0.25rem;
	}

	.attention-modal__body-text {
		margin: 0;
		font-size: 0.85rem;
		line-height: 1.5;
		color: var(--text-secondary);
		white-space: pre-wrap;
	}

	.attention-modal__field {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}

	.attention-modal__footer {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		padding-top: 0.6rem;
		border-top: 1px solid color-mix(in srgb, var(--border-soft) 60%, transparent);
	}
</style>
