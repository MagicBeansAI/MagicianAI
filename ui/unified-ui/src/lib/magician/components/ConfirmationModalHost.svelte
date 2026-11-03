<script lang="ts">
	/**
	 * Host for confirmation dialogs raised via `requestConfirmation()`.
	 *
	 * Mounts once at the app root (in `(app)/+layout.svelte`) and listens
	 * to `confirmationStore`. Wraps GAUI `ConfirmDialog` so every
	 * `window.confirm()` callsite in the codebase can route through one
	 * themed dialog without wiring per-component state.
	 *
	 * A `requestTextInput()` request adds one autofocused text field: Enter
	 * submits, Esc cancels, and confirm is disabled while the field is empty.
	 */
	import { onDestroy } from 'svelte';
	import { fade, fly } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';
	import { confirmationStore, resolveConfirmation } from '$lib/stores/confirmationStore';

	$: request = $confirmationStore.request;

	let inputValue = '';
	let lastRequestId: number | null = null;
	// A new request starts from its own initial value, never the last one's.
	$: if ((request?.id ?? null) !== lastRequestId) {
		lastRequestId = request?.id ?? null;
		inputValue = request?.input?.initialValue ?? '';
	}
	$: confirmDisabled = Boolean(request?.input) && inputValue.trim().length === 0;

	function focusField(node: HTMLInputElement): void {
		requestAnimationFrame(() => {
			node.focus();
			node.select();
		});
	}

	function confirm(): void {
		if (confirmDisabled) return;
		resolveConfirmation(true, request?.input ? inputValue : undefined);
	}

	function cancel(): void {
		resolveConfirmation(false);
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (!request) return;
		if (event.key === 'Escape') {
			event.preventDefault();
			cancel();
		} else if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
			event.preventDefault();
			confirm();
		}
	}

	onDestroy(() => {
		// Don't leave a pending promise dangling if the host unmounts.
		if (request) cancel();
	});
</script>

<svelte:window on:keydown={handleKeydown} />

{#if request}
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="confirmation-backdrop"
		on:click={cancel}
		transition:fade={{ duration: 140 }}
	>
		<!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
		<aside
			class="confirmation-card"
			role={request.input ? 'dialog' : 'alertdialog'}
			aria-modal="true"
			aria-labelledby="confirmation-title"
			data-keyboard-exclusive
			transition:fly={{ y: 12, duration: 180, easing: cubicOut }}
			on:click|stopPropagation
		>
			<h2 id="confirmation-title" class="confirmation-title">{request.title}</h2>
			{#if request.message}
				<p class="confirmation-message">{request.message}</p>
			{/if}
			{#if request.input}
				<input
					class="confirmation-input"
					type="text"
					bind:value={inputValue}
					aria-label={request.input.label}
					placeholder={request.input.placeholder ?? ''}
					maxlength={request.input.maxLength ?? null}
					autocomplete="off"
					spellcheck="false"
					use:focusField
				/>
			{/if}
			<div class="confirmation-actions">
				<button type="button" class="confirmation-btn confirmation-btn--ghost" on:click={cancel}>
					{request.cancelLabel ?? 'Cancel'}
				</button>
				<button
					type="button"
					class="confirmation-btn confirmation-btn--primary"
					class:destructive={request.destructive}
					disabled={confirmDisabled}
					on:click={confirm}
				>
					{request.confirmLabel ?? 'Confirm'}
				</button>
			</div>
		</aside>
	</div>
{/if}

<style>
	.confirmation-backdrop {
		position: fixed;
		inset: 0;
		z-index: 110;
		background: var(--bg-scrim);
		backdrop-filter: blur(4px);
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 1.5rem;
	}

	.confirmation-card {
		width: min(420px, 100%);
		background: var(--bg-base, #fffdf8);
		border: 1px solid color-mix(in srgb, var(--border-soft, #d8d0c5) 80%, transparent);
		border-radius: var(--radius-xl);
		box-shadow: var(--shadow-lg);
		padding: 1.1rem 1.25rem 1rem;
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.confirmation-title {
		margin: 0;
		font-size: 1rem;
		font-weight: 700;
		color: var(--text-primary);
		font-family: var(--font-display, var(--font-primary));
	}

	.confirmation-message {
		margin: 0;
		font-size: 0.85rem;
		line-height: 1.5;
		color: var(--text-secondary);
		white-space: pre-wrap;
	}

	.confirmation-input {
		width: 100%;
		box-sizing: border-box;
		font-family: var(--font-primary);
		font-size: 0.9rem;
		padding: 0.5rem 0.7rem;
		border-radius: 8px;
		border: 1px solid var(--border-soft, #d8d0c5);
		background: var(--bg-soft, #f6f1e8);
		color: var(--text-primary);
	}

	.confirmation-input:focus {
		outline: none;
		border-color: var(--accent-fill, #b5651d);
		box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent-fill, #b5651d) 25%, transparent);
	}

	.confirmation-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		margin-top: 0.25rem;
	}

	.confirmation-btn {
		font-family: var(--font-primary);
		font-size: 0.8rem;
		padding: 0.42rem 0.95rem;
		border-radius: 8px;
		border: 1px solid transparent;
		cursor: pointer;
		transition: background 140ms ease, border-color 140ms ease, color 140ms ease;
	}

	.confirmation-btn--ghost {
		background: transparent;
		color: var(--text-secondary);
		border-color: var(--border-soft);
	}

	.confirmation-btn--ghost:hover {
		background: var(--bg-soft);
	}

	.confirmation-btn--primary {
		background: var(--accent-fill);
		color: var(--text-on-accent, #fff);
	}

	.confirmation-btn--primary:hover {
		filter: brightness(1.05);
	}

	.confirmation-btn:disabled {
		opacity: 0.5;
		cursor: not-allowed;
		filter: none;
	}

	.confirmation-btn--primary.destructive {
		background: var(--status-failed, var(--color-error, #c33));
	}
</style>
